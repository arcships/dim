//! One MCP server connection. No model requests, permissions or tool registry.
mod sse;
mod stdio;

use super::auth::{Credential, McpAuth, Refusal, credential_key};
use super::config::{BearerAuth, McpServerSettings, RemoteTransport};
use dim_agent_capabilities::host_platform::HostPlatform;
use dim_agent_runtime::StateCell;
use dim_agent_types::{
    ContentBlock, ImageSource, ImageSourceType, JsonObject, JsonSchema, ToolContent, ToolResult,
};
use futures_util::StreamExt;
use reqwest_mcp::{
    Client,
    header::{HeaderName, HeaderValue},
};
use rmcp::{
    ClientHandler, Peer, RoleClient, ServiceExt,
    model::{
        CallToolRequest, CallToolRequestParams, ClientCapabilities, ClientConfig,
        ClientJsonRpcMessage, ClientRequest, Implementation, ListToolsRequest,
        PaginatedRequestParams, ReadResourceRequest, ReadResourceRequestParams, ServerResult,
    },
    service::{
        NotificationContext, PeerRequestOptions, RunningService, RxJsonRpcMessage, ServiceError,
        TxJsonRpcMessage,
    },
    transport::{
        StreamableHttpClientTransport, Transport,
        common::client_side_sse::BoxedSseResponse,
        streamable_http_client::{
            AuthRequiredError, StreamableHttpClient, StreamableHttpClientTransportConfig,
            StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};
use rutis::host::{CtxHost, Key};
use rutis::{Ctx, Plugin};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(super) const DIAGNOSTIC_BYTES: usize = 4 * 1024;

pub(super) fn bounded_diagnostic(mut message: String) -> String {
    message.truncate(message.floor_char_boundary(DIAGNOSTIC_BYTES));
    message
}

#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    #[error("MCP server is closed")]
    Closed,
    #[error("MCP operation cancelled; remote effects may already have occurred")]
    Cancelled,
    /// `origin` names where the limit came from: the entry's `timeout`, the
    /// request kind's default, or a fixed deadline.
    #[error(
        "MCP operation timed out after {millis} ms ({origin}); remote effects may already have occurred"
    )]
    Timeout { millis: u64, origin: &'static str },
    #[error("invalid MCP configuration: {0}")]
    Configuration(&'static str),
    #[error(
        "MCP connection or request failed; remote effects may already have occurred; the operation was not replayed"
    )]
    RequestFailed,
    #[error("MCP server failed to start: {0}")]
    StartFailed(String),
    /// A request refused before contacting the server during start backoff.
    #[error("MCP server failed to start; the next attempt is in {0} s")]
    StartCooldown(u64),
    /// The server answered HTTP 404 to a request that carried a session id.
    /// The request was not processed, so replaying it once is safe.
    #[error("MCP session expired (HTTP 404)")]
    SessionExpired,
    #[error("invalid MCP server response: {0}")]
    InvalidResponse(String),
    #[error("MCP cleanup failed")]
    Cleanup,
    #[error("MCP server authorization required")]
    AuthRequired { expired: bool },
    /// A `403 insufficient_scope`, with the challenged scopes.
    #[error("MCP server requires additional scopes")]
    InsufficientScope(String),
}
pub type Result<T> = std::result::Result<T, McpServerError>;

/// A request's failure, split where the hand-off is (MCP Apps D12): only a
/// request the connection never took may be retried.
#[derive(Debug)]
pub enum ViewFailure {
    /// The connection never took the request, so nothing ran.
    Refused(McpServerError),
    /// The connection took the request and its answer did not arrive, so it
    /// may have run.
    Unanswered(McpServerError),
}
impl ViewFailure {
    pub fn error(self) -> McpServerError {
        match self {
            Self::Refused(error) | Self::Unanswered(error) => error,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: JsonSchema,
    /// The tool's `_meta.ui`, which `session.mcp.app` reads. The probe reports
    /// the discovered tools without it.
    #[serde(skip)]
    pub ui: Option<ToolUi>,
}

/// Who may call a tool (MCP Apps D7). A tool without `visibility` is both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolScope {
    Model,
    App,
}

/// A tool's `_meta.ui`: the view it declares and who may call it.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUi {
    #[serde(default)]
    pub resource_uri: Option<String>,
    #[serde(default)]
    pub visibility: Option<Vec<ToolScope>>,
}
impl ToolUi {
    pub fn allows(&self, scope: ToolScope) -> bool {
        self.visibility
            .as_ref()
            .is_none_or(|scopes| scopes.contains(&scope))
    }
}

/// One `resources/read` answer: the content whose URI was asked for.
pub struct McpResource {
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub blob: Option<String>,
    pub meta: Option<JsonObject>,
}

/// One `tools/call` answer before either consumer shapes it. The agent's tool
/// loop forms the Timeline's `ToolResult`, which carries no `_meta`; a view's
/// reply keeps `_meta` and the content as the server sent it.
pub struct McpCall {
    pub content: Vec<ContentBlock>,
    pub structured_content: Option<JsonObject>,
    pub is_error: Option<bool>,
    pub meta: Option<JsonObject>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpServerState {
    Disconnected,
    Connecting,
    Connected,
    Failed,
    Closed,
    /// The server refused the request for lack of a usable OAuth credential.
    /// The next request reconnects, for example once the user signs in.
    AuthRequired {
        expired: bool,
    },
}

#[derive(serde::Serialize)]
pub(super) struct McpRuntime {
    state: McpServerState,
    /// The last start or transport failure. It is status only: no model
    /// request ever carries it (it can quote a server's stderr).
    failure: Option<String>,
    tools: Option<Vec<McpTool>>,
}

/// Start-failure backoff. The first retry waits one second; each later one
/// doubles, capped at five minutes. A successful connection clears it.
#[derive(Default)]
struct Retry {
    failures: u32,
    next: Option<Instant>,
    attempt: u64,
}

fn retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(63);
    Duration::from_secs((1u64 << exponent).min(300))
}

/// A server registers widget tools only for a client that declares the
/// extension, so every connection declares it (MCP Apps D8). The media type is
/// the only document a host is served (`session.mcp.app.widget`).
const UI_EXTENSION: &str = "io.modelcontextprotocol/ui";
pub(super) const APP_MEDIA_TYPE: &str = "text/html;profile=mcp-app";

struct Notifications(StateCell<u64>);
impl ClientHandler for Notifications {
    fn get_info(&self) -> ClientConfig {
        let mut extension = JsonObject::new();
        extension.insert("mimeTypes".to_owned(), serde_json::json!([APP_MEDIA_TYPE]));
        let mut capabilities = ClientCapabilities::default();
        capabilities.extensions = Some([(UI_EXTENSION.to_owned(), extension)].into());
        ClientConfig::new(
            capabilities,
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }
    async fn on_tool_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.0.update(|revision| *revision += 1);
    }
}

type HttpError = StreamableHttpError<reqwest_mcp::Error>;

/// A connection's OAuth credential, and the state its refusals set.
#[derive(Clone)]
pub(super) struct Authorization {
    credential: Credential,
    server: reqwest_mcp::Url,
    state: StateCell<McpServerState>,
}
impl Authorization {
    fn refused(&self, refusal: Refusal) -> McpServerError {
        match refusal {
            Refusal::Unavailable => McpServerError::RequestFailed,
            Refusal::Required | Refusal::Expired => {
                let expired = matches!(refusal, Refusal::Expired);
                self.state.set(McpServerState::AuthRequired { expired });
                McpServerError::AuthRequired { expired }
            }
        }
    }
    /// Only `error="insufficient_scope"` is step-up; rmcp reports any `403`
    /// with a challenge as insufficient scope. Refused during the handshake,
    /// the connection cannot be used until the user signs in again.
    fn insufficient_scope(&self, header: &str) -> Option<String> {
        let scopes = self.credential.insufficient_scope(header, &self.server)?;
        self.state.update(|state| {
            if *state == McpServerState::Connecting {
                *state = McpServerState::AuthRequired { expired: false };
            }
        });
        Some(scopes.join(" "))
    }
}

#[derive(Clone)]
struct InitializingClient {
    client: Client,
    cancel: CancellationToken,
    authorization: Option<Authorization>,
}
impl InitializingClient {
    async fn authorized<T, Fut>(
        &self,
        auth_header: Option<String>,
        call: impl Fn(Option<String>) -> Fut,
    ) -> std::result::Result<T, HttpError>
    where
        Fut: Future<Output = std::result::Result<T, HttpError>>,
    {
        let Some(authorization) = &self.authorization else {
            return match call(auth_header).await {
                Err(HttpError::InsufficientScope(_)) => Err(forbidden()),
                result => result,
            };
        };
        let result = authorization
            .credential
            .send(call, |result| match result {
                Err(HttpError::AuthRequired(error)) => {
                    Some(Some(error.www_authenticate_header.clone()))
                }
                Err(HttpError::Client(error))
                    if error.status() == Some(reqwest_mcp::StatusCode::UNAUTHORIZED) =>
                {
                    Some(None)
                }
                Err(HttpError::UnexpectedServerResponse(message))
                    if message.starts_with("HTTP 401 ") =>
                {
                    Some(None)
                }
                _ => None,
            })
            .await
            .map_err(|refusal| match authorization.refused(refusal) {
                McpServerError::AuthRequired { .. } => {
                    HttpError::AuthRequired(AuthRequiredError::new(String::new()))
                }
                _ => HttpError::UnexpectedServerResponse("authorization unavailable".into()),
            })?;
        match result {
            Err(HttpError::InsufficientScope(error))
                if authorization
                    .insufficient_scope(&error.www_authenticate_header)
                    .is_none() =>
            {
                Err(forbidden())
            }
            result => result,
        }
    }
}
fn forbidden() -> HttpError {
    HttpError::UnexpectedServerResponse("forbidden".into())
}
impl StreamableHttpClient for InitializingClient {
    type Error = reqwest_mcp::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> std::result::Result<StreamableHttpPostResponse, HttpError> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
        .await
    }
    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> std::result::Result<StreamableHttpPostResponse, HttpError> {
        let post = self.authorized(auth_header, |token| {
            self.client.post_message_with_max_sse_event_size(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                token,
                headers.clone(),
                limit,
            )
        });
        let response = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(HttpError::TransportChannelClosed),
            result = post => result?,
        };
        Ok(match response {
            StreamableHttpPostResponse::Sse(stream, session) => StreamableHttpPostResponse::Sse(
                stream
                    .take_until(self.cancel.clone().cancelled_owned())
                    .boxed(),
                session,
            ),
            other => other,
        })
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> std::result::Result<(), HttpError> {
        self.authorized(auth_header, |token| {
            self.client
                .delete_session(uri.clone(), session_id.clone(), token, headers.clone())
        })
        .await
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> std::result::Result<BoxedSseResponse, HttpError> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            headers,
            StreamableHttpClientTransportConfig::default().max_sse_event_size,
        )
        .await
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> std::result::Result<BoxedSseResponse, HttpError> {
        self.authorized(auth_header, |token| {
            self.client.get_stream_with_max_sse_event_size(
                uri.clone(),
                session_id.clone(),
                last_event_id.clone(),
                token,
                headers.clone(),
                limit,
            )
        })
        .await
    }
}

struct OwnedTransport<T: Transport<RoleClient> + 'static> {
    inner: Option<T>,
    closed: Option<oneshot::Sender<Result<()>>>,
    state: StateCell<McpServerState>,
    failure: Arc<Mutex<Option<String>>>,
    abandoned: Arc<AtomicBool>,
}
impl<T: Transport<RoleClient> + 'static> Transport<RoleClient> for OwnedTransport<T> {
    type Error = T::Error;
    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = std::result::Result<(), T::Error>> + Send + 'static {
        self.inner.as_mut().expect("live transport").send(item)
    }
    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        let message = self.inner.as_mut().expect("live transport").receive().await;
        if message.is_none() {
            let mut failure = self.failure.lock().unwrap();
            if failure.is_none() {
                *failure = Some("MCP transport closed".into());
            }
            drop(failure);
            self.state.update(|state| {
                if !matches!(state, McpServerState::AuthRequired { .. }) {
                    *state = McpServerState::Disconnected;
                }
            });
        }
        message
    }
    async fn close(&mut self) -> std::result::Result<(), T::Error> {
        let result = if self.abandoned.load(Ordering::SeqCst) {
            // The session is gone: drop the transport without a graceful
            // close, which also kills a stdio child that is still running.
            self.inner.take();
            Ok(())
        } else if let Some(inner) = self.inner.as_mut() {
            inner.close().await
        } else {
            Ok(())
        };
        self.inner = None;
        if let Some(closed) = self.closed.take() {
            let _ = closed.send(
                result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|_| McpServerError::Cleanup),
            );
        }
        result
    }
}
impl<T: Transport<RoleClient> + 'static> Drop for OwnedTransport<T> {
    fn drop(&mut self) {
        if self.abandoned.load(Ordering::SeqCst) {
            self.inner.take();
            let _ = self.closed.take();
            return;
        }
        if let Some(mut inner) = self.inner.take() {
            let closed = self.closed.take();
            tokio::spawn(async move {
                let result = inner.close().await.map_err(|_| McpServerError::Cleanup);
                if let Some(closed) = closed {
                    let _ = closed.send(result);
                }
            });
        }
    }
}

struct Connection {
    running: RunningService<RoleClient, Notifications>,
    closed: oneshot::Receiver<Result<()>>,
    abandoned: Arc<AtomicBool>,
}
impl Connection {
    async fn close(mut self) -> Result<()> {
        let result = self.running.close().await;
        self.closed.await.map_err(|_| McpServerError::Cleanup)??;
        result.map_err(|_| McpServerError::Cleanup)?;
        Ok(())
    }

    /// Drop a connection whose transport is already gone. There is no
    /// graceful close to wait for, and the child must not outlive it.
    async fn abandon(self) {
        self.abandoned.store(true, Ordering::SeqCst);
        let _ = self.running.cancel().await;
    }
}

#[derive(Clone)]
enum TransportConfig {
    Http(StreamableHttpClientTransportConfig),
    Sse(StreamableHttpClientTransportConfig),
    Stdio(stdio::Config),
}

/// The limit every request except a tool call waits under.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// A tool call without an entry `timeout` waits this long.
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(600);
/// Starting a server, including the initial handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The limit a request waits under and the source a timeout names.
#[derive(Clone, Copy)]
struct RequestLimit {
    limit: Duration,
    origin: &'static str,
}
impl RequestLimit {
    fn millis(self) -> u64 {
        self.limit.as_millis() as u64
    }
    fn timeout(self) -> McpServerError {
        McpServerError::Timeout {
            millis: self.millis(),
            origin: self.origin,
        }
    }
}

/// A tool call takes its entry's `timeout`, or `TOOL_CALL_TIMEOUT` without
/// one; every other request keeps `REQUEST_TIMEOUT`.
fn request_limit(request: &ClientRequest, entry: Option<Duration>) -> RequestLimit {
    match (request, entry) {
        (ClientRequest::CallToolRequest(_), Some(limit)) => RequestLimit {
            limit,
            origin: "the entry's timeout",
        },
        (ClientRequest::CallToolRequest(_), None) => RequestLimit {
            limit: TOOL_CALL_TIMEOUT,
            origin: "the default",
        },
        _ => RequestLimit {
            limit: REQUEST_TIMEOUT,
            origin: "the request limit",
        },
    }
}

pub struct McpServer {
    transport: TransportConfig,
    /// The host a stdio child takes its `PATH` from: the entry's own, else
    /// this host's, with the installed runtimes appended. Read optionally, so
    /// a host without HostPlatform spawns as before.
    platform: Option<Arc<HostPlatform>>,
    tool_call_timeout: Option<Duration>,
    authorization: Option<Authorization>,
    state: StateCell<McpServerState>,
    tool_changes: StateCell<u64>,
    discovered_tools: Mutex<Option<Vec<McpTool>>>,
    connection: AsyncMutex<Option<Connection>>,
    generations: AtomicU64,
    failure: Arc<Mutex<Option<String>>>,
    retry: Arc<Mutex<Retry>>,
    stopping: CancellationToken,
    gate: Mutex<()>,
    tasks: TaskTracker,
}
impl McpServer {
    pub(super) fn snapshot(&self) -> McpRuntime {
        McpRuntime {
            state: self.state(),
            failure: self.failure(),
            tools: self.discovered_tools.lock().unwrap().clone(),
        }
    }

    pub fn state(&self) -> McpServerState {
        self.state.get()
    }

    /// The last start or transport failure. Status only, never a model request.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }

    /// A new failure replaces the previous one, even when no connection
    /// follows it.
    fn set_failure(&self, message: impl Into<String>) {
        *self.failure.lock().unwrap() = Some(bounded_diagnostic(message.into()));
    }

    /// A fallback reason for one event: a more specific message set by the
    /// transport keeps priority.
    fn set_failure_if_empty(&self, message: impl Into<String>) {
        let mut failure = self.failure.lock().unwrap();
        if failure.is_none() {
            *failure = Some(bounded_diagnostic(message.into()));
        }
    }

    fn clear_failure(&self) {
        *self.failure.lock().unwrap() = None;
    }

    fn cooldown_remaining(&self) -> Option<Duration> {
        self.retry
            .lock()
            .unwrap()
            .next
            .map(|next| next.saturating_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
    }

    fn cooldown_seconds(&self) -> u64 {
        let remaining = self.cooldown_remaining().unwrap_or_default();
        remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
    }

    fn record_failure(&self) -> (Duration, u64) {
        let mut retry = self.retry.lock().unwrap();
        retry.failures = retry.failures.saturating_add(1);
        retry.attempt = retry.attempt.wrapping_add(1);
        let delay = retry_delay(retry.failures);
        retry.next = Some(Instant::now() + delay);
        (delay, retry.attempt)
    }

    fn reset_retry(&self) {
        let mut retry = self.retry.lock().unwrap();
        retry.failures = 0;
        retry.next = None;
        retry.attempt = retry.attempt.wrapping_add(1);
    }

    /// Move a cooling failure to `Disconnected` when its delay ends. The
    /// failure reason stays until a connection succeeds.
    fn schedule_state_reset(&self, delay: Duration, attempt: u64) {
        let state = self.state.clone();
        let retry = self.retry.clone();
        let stopping = self.stopping.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                _ = stopping.cancelled() => {}
                _ = tokio::time::sleep(delay) => {
                    let mut retry = retry.lock().unwrap();
                    if retry.attempt != attempt {
                        return;
                    }
                    retry.next = None;
                    if matches!(state.get(), McpServerState::Failed) {
                        state.set(McpServerState::Disconnected);
                    }
                }
            }
        });
    }
    /// The generation of the connection this server holds: a new one starts
    /// whenever a connection is established, and the Session's entry version
    /// follows it (MCP Apps D3).
    pub fn generation(&self) -> u64 {
        self.generations.load(Ordering::SeqCst)
    }
    /// The tools of the last complete discovery, including the ones the
    /// Session publishes to no model (`visibility` without `model`).
    pub fn discovered(&self) -> Option<Vec<McpTool>> {
        self.discovered_tools.lock().unwrap().clone()
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<McpServerState> {
        self.state.subscribe()
    }
    pub fn tool_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.tool_changes.subscribe()
    }
    /// Changes when this server's OAuth credential changes; `None` without
    /// OAuth.
    pub fn credential_changes(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.authorization
            .as_ref()
            .map(|authorization| authorization.credential.subscribe())
    }

    /// Fetch every page; reject cycles/duplicate names instead of publishing a partial set.
    pub async fn tools(self: &Arc<Self>, cancel: &CancellationToken) -> Result<Vec<McpTool>> {
        let owner = self.clone();
        self.run(cancel, move |cancel| async move {
            let mut cursor = None;
            let mut cursors = BTreeSet::new();
            let mut names = BTreeSet::new();
            let mut tools = Vec::new();
            loop {
                let params = PaginatedRequestParams::default().with_cursor(cursor);
                let request = ClientRequest::ListToolsRequest(ListToolsRequest::with_param(params));
                let ServerResult::ListToolsResult(page) = owner.request(request, &cancel).await?
                else {
                    return Err(McpServerError::InvalidResponse(
                        "tools/list returned an unexpected result".into(),
                    ));
                };
                for tool in page.tools {
                    if tool.name.is_empty() {
                        return Err(McpServerError::InvalidResponse(
                            "a tool name must not be empty".into(),
                        ));
                    }
                    if !names.insert(tool.name.to_string()) {
                        return Err(McpServerError::InvalidResponse(format!(
                            "tool {}: the tool name repeats an earlier page",
                            tool.name
                        )));
                    }
                    if tool.input_schema.get("type").and_then(|v| v.as_str()) != Some("object") {
                        return Err(McpServerError::InvalidResponse(format!(
                            "tool {}: inputSchema.type must be \"object\"",
                            tool.name
                        )));
                    }
                    let name = tool.name.into_owned();
                    let input_schema = serde_json::from_value(serde_json::Value::Object(
                        (*tool.input_schema).clone(),
                    ))
                    .map_err(|error| {
                        McpServerError::InvalidResponse(format!("tool {name}: {error}"))
                    })?;
                    tools.push(McpTool {
                        name,
                        description: tool.description.map(|s| s.into_owned()),
                        input_schema,
                        ui: tool
                            .meta
                            .as_ref()
                            .and_then(|meta| meta.0.get("ui"))
                            .and_then(|ui| serde_json::from_value(ui.clone()).ok()),
                    });
                }
                cursor = page.next_cursor;
                let Some(next) = cursor.as_ref() else {
                    *owner.discovered_tools.lock().unwrap() = Some(tools.clone());
                    return Ok(tools);
                };
                if !cursors.insert(next.clone()) {
                    return Err(McpServerError::InvalidResponse(
                        "tools/list returned a cursor that repeats an earlier page".into(),
                    ));
                }
            }
        })
        .await
    }

    /// Server-local name. Session namespacing and request binding belong to the consumer.
    pub async fn call(
        self: &Arc<Self>,
        name: String,
        arguments: JsonObject,
        cancel: &CancellationToken,
    ) -> Result<ToolResult> {
        let mut answer = self
            .call_result(name, arguments, cancel)
            .await
            .map_err(ViewFailure::error)?;
        if let Some(structured) = &answer.structured_content {
            answer.content.push(ContentBlock::Text {
                text: serde_json::Value::Object(structured.clone()).to_string(),
            });
        }
        Ok(ToolResult {
            content: ToolContent::Blocks(answer.content),
            structured_content: answer.structured_content,
            is_error: answer.is_error,
            ..ToolResult::default()
        })
    }

    /// The same call, keeping the result's `_meta` and the content as the
    /// server sent it: the shape a view's reply carries.
    pub async fn call_result(
        self: &Arc<Self>,
        name: String,
        arguments: JsonObject,
        cancel: &CancellationToken,
    ) -> std::result::Result<McpCall, ViewFailure> {
        let owner = self.clone();
        self.run_view(cancel, move |cancel| async move {
            let mut params = CallToolRequestParams::new(name);
            params.arguments = Some(arguments);
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
            let ServerResult::CallToolResult(result) = owner.exchange(request, &cancel).await?
            else {
                return Err(ViewFailure::Unanswered(McpServerError::InvalidResponse(
                    "tools/call returned an unexpected result".into(),
                )));
            };
            let mut content = Vec::new();
            for block in result.content {
                content.push(match block {
                    rmcp::model::ContentBlock::Text(text) => ContentBlock::Text { text: text.text },
                    rmcp::model::ContentBlock::Image(image) => ContentBlock::Image {
                        source: ImageSource {
                            file_id: None,
                            source_type: ImageSourceType::Base64,
                            media_type: image.mime_type,
                            data: Some(image.data),
                            url: None,
                        },
                    },
                    other => ContentBlock::Text {
                        text: serde_json::to_string(&other).map_err(|error| {
                            ViewFailure::Unanswered(McpServerError::InvalidResponse(format!(
                                "a tool result content block is not JSON: {error}"
                            )))
                        })?,
                    },
                });
            }
            Ok(McpCall {
                content,
                structured_content: result
                    .structured_content
                    .and_then(|v| v.as_object().cloned()),
                is_error: result.is_error,
                meta: result.meta.map(|meta| meta.0),
            })
        })
        .await
    }

    /// The one document a widget tool declares: a resource read by URI, which
    /// the Session issues only for the URI it resolved from a tool.
    pub async fn resource(
        self: &Arc<Self>,
        uri: String,
        cancel: &CancellationToken,
    ) -> std::result::Result<McpResource, ViewFailure> {
        let owner = self.clone();
        self.run_view(cancel, move |cancel| async move {
            let request = ClientRequest::ReadResourceRequest(ReadResourceRequest::new(
                ReadResourceRequestParams::new(uri.clone()),
            ));
            let ServerResult::ReadResourceResult(result) = owner.exchange(request, &cancel).await?
            else {
                return Err(ViewFailure::Unanswered(McpServerError::InvalidResponse(
                    "resources/read returned an unexpected result".into(),
                )));
            };
            let content = result
                .contents
                .into_iter()
                .find(|content| match content {
                    rmcp::model::ResourceContents::TextResourceContents { uri: read, .. }
                    | rmcp::model::ResourceContents::BlobResourceContents { uri: read, .. } => {
                        *read == uri
                    }
                    _ => false,
                })
                .ok_or_else(|| {
                    ViewFailure::Unanswered(McpServerError::InvalidResponse(format!(
                        "resources/read answered a resource other than {uri}"
                    )))
                })?;
            Ok(match content {
                rmcp::model::ResourceContents::TextResourceContents {
                    mime_type,
                    text,
                    meta,
                    ..
                } => McpResource {
                    mime_type,
                    text: Some(text),
                    blob: None,
                    meta: meta.map(|meta| meta.0),
                },
                rmcp::model::ResourceContents::BlobResourceContents {
                    mime_type,
                    blob,
                    meta,
                    ..
                } => McpResource {
                    mime_type,
                    text: None,
                    blob: Some(blob),
                    meta: meta.map(|meta| meta.0),
                },
                _ => {
                    return Err(ViewFailure::Unanswered(McpServerError::InvalidResponse(
                        format!("resources/read answered an unsupported resource for {uri}"),
                    )));
                }
            })
        })
        .await
    }

    async fn peer_with_generation(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(Peer<RoleClient>, u64)> {
        let mut connection = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(McpServerError::Cancelled),
            guard = self.connection.lock() => guard,
        };
        if matches!(self.state.get(), McpServerState::AuthRequired { .. })
            && let Some(refused) = connection.take()
        {
            let _ = refused.close().await;
        }
        if let Some(current) = connection.as_ref() {
            if !current.running.is_closed() && matches!(self.state.get(), McpServerState::Connected)
            {
                return Ok((current.running.peer().clone(), self.generation()));
            }
            // The transport is gone even when its task has not marked the
            // cell yet. Its child must not outlive the connection.
            let closed = connection.take().expect("checked connection");
            closed.abandon().await;
            self.set_failure_if_empty("MCP transport closed");
            self.state.set(McpServerState::Disconnected);
        }
        if self.state.get() == McpServerState::Failed {
            if self.cooldown_remaining().is_some() {
                return Err(McpServerError::StartCooldown(self.cooldown_seconds()));
            }
            self.state.set(McpServerState::Disconnected);
        }
        let previous = self.failure();
        self.state.set(McpServerState::Connecting);
        match self.connect(cancel).await {
            Ok(connected) => {
                let peer = connected.running.peer().clone();
                *connection = Some(connected);
                let restarted = self.generations.load(Ordering::SeqCst) > 0;
                self.generations.fetch_add(1, Ordering::SeqCst);
                let generation = self.generations.load(Ordering::SeqCst);
                self.reset_retry();
                self.clear_failure();
                self.state.update(|state| {
                    if *state == McpServerState::Connecting {
                        *state = McpServerState::Connected;
                    }
                });
                if restarted {
                    // A Session re-reads the tool list for this generation.
                    self.tool_changes.update(|revision| *revision += 1);
                }
                Ok((peer, generation))
            }
            Err(error) => {
                if let McpServerState::AuthRequired { expired } = self.state.get() {
                    return Err(McpServerError::AuthRequired { expired });
                }
                if matches!(error, McpServerError::Cancelled | McpServerError::Closed) {
                    self.state.set(McpServerState::Disconnected);
                    return Err(error);
                }
                if self.failure() == previous {
                    self.set_failure(error.to_string());
                }
                let (delay, attempt) = self.record_failure();
                self.state.set(McpServerState::Failed);
                self.schedule_state_reset(delay, attempt);
                Err(error)
            }
        }
    }

    async fn connect(&self, cancel: &CancellationToken) -> Result<Connection> {
        let handshake_cancel = CancellationToken::new();
        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
        match &self.transport {
            TransportConfig::Http(config) => {
                let transport = StreamableHttpClientTransport::with_client(
                    InitializingClient {
                        client: http_client()?,
                        cancel: handshake_cancel.clone(),
                        authorization: self.authorization.clone(),
                    },
                    config.clone(),
                );
                self.initialize(transport, cancel, handshake_cancel, deadline)
                    .await
            }
            TransportConfig::Sse(config) => {
                let transport = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(McpServerError::Cancelled),
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(McpServerError::Timeout {
                            millis: CONNECT_TIMEOUT.as_millis() as u64,
                            origin: "the connection deadline",
                        });
                    }
                    result = sse::SseTransport::connect(http_client()?, config, self.authorization.clone(), handshake_cancel.clone()) => result?,
                };
                self.initialize(transport, cancel, handshake_cancel, deadline)
                    .await
            }
            TransportConfig::Stdio(config) => {
                self.initialize(
                    config.spawn(self.failure.clone(), self.platform.as_deref())?,
                    cancel,
                    handshake_cancel,
                    deadline,
                )
                .await
            }
        }
    }

    async fn initialize<T: Transport<RoleClient> + 'static>(
        &self,
        transport: T,
        cancel: &CancellationToken,
        handshake_cancel: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<Connection> {
        let previous = self.failure();
        let (tx, closed) = oneshot::channel();
        let abandoned = Arc::new(AtomicBool::new(false));
        let transport = OwnedTransport {
            inner: Some(transport),
            closed: Some(tx),
            state: self.state.clone(),
            failure: self.failure.clone(),
            abandoned: abandoned.clone(),
        };
        let lifetime = CancellationToken::new();
        let initialize =
            Notifications(self.tool_changes.clone()).serve_with_ct(transport, lifetime.clone());
        tokio::pin!(initialize);
        let failure = tokio::select! {
            biased;
            result = &mut initialize => {
                match result {
                    Ok(running) => {
                        self.clear_failure();
                        return Ok(Connection {
                            running,
                            closed,
                            abandoned,
                        });
                    },
                    Err(error) => {
                        handshake_cancel.cancel();
                        if self.failure() == previous {
                            self.set_failure(error.to_string());
                        }
                        closed.await.map_err(|_| McpServerError::Cleanup)??;
                        return Err(McpServerError::RequestFailed);
                    }
                }
            }
            _ = cancel.cancelled() => McpServerError::Cancelled,
            _ = tokio::time::sleep_until(deadline) => McpServerError::Timeout {
                millis: CONNECT_TIMEOUT.as_millis() as u64,
                origin: "the connection deadline",
            },
        };
        handshake_cancel.cancel();
        lifetime.cancel();
        if let Ok(running) = initialize.await {
            Connection {
                running,
                closed,
                abandoned,
            }
            .close()
            .await?;
        } else {
            closed.await.map_err(|_| McpServerError::Cleanup)??;
        }
        Err(failure)
    }

    async fn request(
        &self,
        request: ClientRequest,
        cancel: &CancellationToken,
    ) -> Result<ServerResult> {
        self.exchange(request, cancel)
            .await
            .map_err(ViewFailure::error)
    }

    /// Drop the connection a `404` used: its session is already gone, so
    /// there is no graceful close to wait for. The generation check keeps a
    /// caller that already reconnected from retiring the new session.
    async fn expire(&self, generation: u64) {
        let mut connection = self.connection.lock().await;
        if self.generations.load(Ordering::SeqCst) != generation {
            return;
        }
        let Some(current) = connection.take() else {
            return;
        };
        current.abandon().await;
        self.set_failure(McpServerError::SessionExpired.to_string());
        self.state.set(McpServerState::Disconnected);
    }

    /// The request path every call shares, reporting whether the connection
    /// already held the request when it failed.
    async fn exchange(
        &self,
        request: ClientRequest,
        cancel: &CancellationToken,
    ) -> std::result::Result<ServerResult, ViewFailure> {
        let limit = request_limit(&request, self.tool_call_timeout);
        let deadline = tokio::time::Instant::now() + limit.limit;
        let mut retried = false;
        loop {
            let (peer, generation) = self
                .peer_with_generation(cancel)
                .await
                .map_err(ViewFailure::Refused)?;
            if cancel.is_cancelled() {
                return Err(ViewFailure::Refused(McpServerError::Cancelled));
            }
            // Connecting counts against the limit, and a request sent after
            // the deadline would only be cancelled at once. Nothing left the
            // connection, so this is a refusal, not an unanswered request.
            if tokio::time::Instant::now() >= deadline {
                return Err(ViewFailure::Refused(limit.timeout()));
            }
            let mut handle = peer
                .send_cancellable_request(request.clone(), PeerRequestOptions::no_options())
                .await
                .map_err(|_| ViewFailure::Refused(McpServerError::RequestFailed))?;
            let failure = tokio::select! {
                biased;
                _ = cancel.cancelled() => McpServerError::Cancelled,
                _ = tokio::time::sleep_until(deadline) => limit.timeout(),
                response = &mut handle.rx => {
                    match response {
                        Err(_) => McpServerError::RequestFailed,
                        Ok(Ok(response)) => return Ok(response),
                        Ok(Err(error)) => {
                            let failure = self.request_failure(&error);
                            if matches!(failure, McpServerError::SessionExpired) {
                                // The server did not process a request that
                                // carried the expired session id, so one replay
                                // on a fresh session is safe. A second 404 in a
                                // row fails the call.
                                self.expire(generation).await;
                                if !retried {
                                    retried = true;
                                    continue;
                                }
                            }
                            return Err(ViewFailure::Unanswered(failure));
                        }
                    }
                }
            };
            let notice = match &failure {
                McpServerError::Timeout { millis, origin } => {
                    format!("MCP operation timed out after {millis} ms ({origin})")
                }
                _ => "MCP operation cancelled".to_owned(),
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), handle.cancel(Some(notice))).await;
            return Err(ViewFailure::Unanswered(failure));
        }
    }

    /// Authorization outcomes of a request, from the transport's own error.
    fn request_failure(&self, error: &ServiceError) -> McpServerError {
        let ServiceError::TransportSend(error) = error else {
            return McpServerError::RequestFailed;
        };
        let refused = match self.state.get() {
            McpServerState::AuthRequired { expired } => McpServerError::AuthRequired { expired },
            _ => McpServerError::RequestFailed,
        };
        match error.error.downcast_ref::<HttpError>() {
            Some(HttpError::AuthRequired(_)) => return refused,
            Some(HttpError::InsufficientScope(error)) => {
                return McpServerError::InsufficientScope(
                    error.required_scope.clone().unwrap_or_default(),
                );
            }
            Some(HttpError::SessionExpired) => {
                return McpServerError::SessionExpired;
            }
            _ => {}
        }
        match error.error.downcast_ref::<McpServerError>() {
            Some(McpServerError::AuthRequired { .. }) => refused,
            Some(McpServerError::InsufficientScope(scopes)) => {
                McpServerError::InsufficientScope(scopes.clone())
            }
            Some(McpServerError::SessionExpired) => McpServerError::SessionExpired,
            _ => McpServerError::RequestFailed,
        }
    }

    async fn run<T, F, Fut>(&self, cancel: &CancellationToken, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        self.spawn(cancel, |error| error, operation).await
    }

    async fn run_view<T, F, Fut>(
        &self,
        cancel: &CancellationToken,
        operation: F,
    ) -> std::result::Result<T, ViewFailure>
    where
        T: Send + 'static,
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = std::result::Result<T, ViewFailure>> + Send + 'static,
    {
        self.spawn(cancel, ViewFailure::Refused, operation).await
    }

    /// Runs one operation in the part's own task, refusing it before it starts
    /// once the server stops. `refused` names a failure that never reached the
    /// connection.
    async fn spawn<T, E, F, Fut>(
        &self,
        cancel: &CancellationToken,
        refused: impl Fn(McpServerError) -> E,
        operation: F,
    ) -> std::result::Result<T, E>
    where
        T: Send + 'static,
        E: Send + 'static,
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = std::result::Result<T, E>> + Send + 'static,
    {
        let task = {
            let _gate = self.gate.lock().unwrap();
            if self.stopping.is_cancelled() {
                return Err(refused(McpServerError::Closed));
            }
            if cancel.is_cancelled() {
                return Err(refused(McpServerError::Cancelled));
            }
            let stopping = self.stopping.clone();
            let cancel = cancel.child_token();
            self.tasks.spawn(async move {
                let operation = operation(cancel.clone());
                tokio::pin!(operation);
                tokio::select! {
                    biased;
                    _ = stopping.cancelled() => {},
                    result = &mut operation => return result,
                }
                cancel.cancel();
                operation.await
            })
        };
        task.await
            .map_err(|_| refused(McpServerError::RequestFailed))?
    }
}

fn http_configuration(
    url: &str,
    headers: &std::collections::BTreeMap<String, String>,
    auth: &Option<BearerAuth>,
) -> Result<StreamableHttpClientTransportConfig> {
    let uri =
        reqwest_mcp::Url::parse(url).map_err(|_| McpServerError::Configuration("invalid URL"))?;
    if !matches!(uri.scheme(), "http" | "https")
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
    {
        return Err(McpServerError::Configuration(
            "expected HTTP URL without userinfo or fragment",
        ));
    }
    let mut custom_headers = HashMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| McpServerError::Configuration("invalid header name"))?;
        if matches!(
            name.as_str(),
            "accept" | "mcp-session-id" | "mcp-protocol-version" | "last-event-id"
        ) {
            return Err(McpServerError::Configuration("reserved header name"));
        }
        let mut value = HeaderValue::from_str(value)
            .map_err(|_| McpServerError::Configuration("invalid header value"))?;
        value.set_sensitive(true);
        if custom_headers.insert(name, value).is_some() {
            return Err(McpServerError::Configuration("duplicate header name"));
        }
    }
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned())
        .custom_headers(custom_headers)
        .reinit_on_expired_session(false);
    if let Some(BearerAuth::Bearer { token }) = auth {
        if headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
        {
            return Err(McpServerError::Configuration("ambiguous authorization"));
        }
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| McpServerError::Configuration("invalid bearer token"))?;
        config = config.auth_header(token.clone());
    }
    Ok(config)
}

fn http_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest_mcp::redirect::Policy::none())
        .retry(reqwest_mcp::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| McpServerError::Configuration("HTTP client initialization failed"))
}

pub(super) struct McpServerPlugin {
    pub key: Key<McpServer>,
    transport: TransportConfig,
    tool_call_timeout: Option<Duration>,
    oauth: Option<(super::auth::CredentialKey, reqwest_mcp::Url)>,
}
impl McpServerPlugin {
    pub fn new(id: &str, configuration: McpServerSettings, key: Key<McpServer>) -> Result<Self> {
        let oauth = credential_key(id, &configuration);
        let tool_call_timeout = configuration.timeout().map(Duration::from_millis);
        let transport = match &configuration {
            McpServerSettings::Stdio {
                command,
                args,
                env,
                cwd,
                ..
            } => TransportConfig::Stdio(stdio::Config::new(command, args, env, cwd)?),
            McpServerSettings::Remote {
                kind,
                url,
                headers,
                auth,
                ..
            } => {
                let config = http_configuration(url, headers, auth)?;
                match kind {
                    RemoteTransport::Http => TransportConfig::Http(config),
                    RemoteTransport::Sse => TransportConfig::Sse(config),
                }
            }
        };
        Ok(Self {
            key,
            transport,
            tool_call_timeout,
            oauth,
        })
    }
}
impl Plugin for McpServerPlugin {
    type Deps = Arc<McpAuth>;
    fn name(&self) -> &str {
        "McpServer"
    }
    async fn apply(&self, ctx: &Ctx, auth: Arc<McpAuth>) -> rutis::Result {
        let state = StateCell::new(McpServerState::Disconnected);
        let service = Arc::new(McpServer {
            transport: self.transport.clone(),
            platform: ctx.get_at(&Key::<HostPlatform>::of()),
            tool_call_timeout: self.tool_call_timeout,
            authorization: self.oauth.clone().map(|(key, server)| Authorization {
                credential: auth.credential(key),
                server,
                state: state.clone(),
            }),
            state,
            tool_changes: StateCell::new(0),
            discovered_tools: Mutex::new(None),
            connection: AsyncMutex::new(None),
            generations: AtomicU64::new(0),
            failure: Arc::new(Mutex::new(None)),
            retry: Arc::new(Mutex::new(Retry::default())),
            stopping: ctx.cancellation_token(),
            gate: Mutex::new(()),
            tasks: TaskTracker::new(),
        });
        let owner = service.clone();
        ctx.effect(move || async move {
            {
                let _gate = owner.gate.lock().unwrap();
                owner.tasks.close();
            }
            owner.tasks.wait().await;
            let connection = owner.connection.lock().await.take();
            let result = match connection {
                Some(connection) => connection.close().await,
                None => Ok(()),
            };
            owner.state.set(if result.is_ok() {
                McpServerState::Closed
            } else {
                McpServerState::Failed
            });
            Ok(result?)
        })?;
        ctx.provide_at(self.key.clone(), service)?;
        Ok(())
    }
}
