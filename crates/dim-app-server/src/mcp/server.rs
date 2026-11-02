//! One MCP server connection. No model requests, permissions or tool registry.
mod sse;
mod stdio;

use super::auth::{Credential, McpAuth, Refusal, credential_key};
use super::config::{BearerAuth, McpServerSettings, RemoteTransport};
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
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    #[error("MCP server is closed")]
    Closed,
    #[error("MCP operation cancelled; remote effects may already have occurred")]
    Cancelled,
    #[error("MCP operation timed out; remote effects may already have occurred")]
    Timeout,
    #[error("invalid MCP configuration: {0}")]
    Configuration(&'static str),
    #[error("MCP connection or request failed; the operation was not replayed")]
    RequestFailed,
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
    tools: Option<Vec<McpTool>>,
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
            self.state.update(|state| {
                if !matches!(state, McpServerState::AuthRequired { .. }) {
                    *state = McpServerState::Failed;
                }
            });
        }
        message
    }
    async fn close(&mut self) -> std::result::Result<(), T::Error> {
        let result = if let Some(inner) = self.inner.as_mut() {
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
}
impl Connection {
    async fn close(mut self) -> Result<()> {
        let result = self.running.close().await;
        self.closed.await.map_err(|_| McpServerError::Cleanup)??;
        result.map_err(|_| McpServerError::Cleanup)?;
        Ok(())
    }
}

#[derive(Clone)]
enum TransportConfig {
    Http(StreamableHttpClientTransportConfig),
    Sse(StreamableHttpClientTransportConfig),
    Stdio(stdio::Config),
}

pub struct McpServer {
    transport: TransportConfig,
    authorization: Option<Authorization>,
    state: StateCell<McpServerState>,
    tool_changes: StateCell<u64>,
    discovered_tools: Mutex<Option<Vec<McpTool>>>,
    connection: AsyncMutex<Option<Connection>>,
    generations: AtomicU64,
    stopping: CancellationToken,
    gate: Mutex<()>,
    tasks: TaskTracker,
}
impl McpServer {
    pub(super) fn snapshot(&self) -> McpRuntime {
        McpRuntime {
            state: self.state(),
            tools: self.discovered_tools.lock().unwrap().clone(),
        }
    }

    pub fn state(&self) -> McpServerState {
        self.state.get()
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

    async fn peer(&self, cancel: &CancellationToken) -> Result<Peer<RoleClient>> {
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
        if let Some(connection) = connection.as_ref() {
            if connection.running.is_closed() {
                self.state.set(McpServerState::Failed);
                return Err(McpServerError::RequestFailed);
            }
            return Ok(connection.running.peer().clone());
        }
        if self.state.get() == McpServerState::Failed {
            return Err(McpServerError::RequestFailed);
        }
        self.state.set(McpServerState::Connecting);
        let result = self.connect(cancel).await;
        match result {
            Ok(connected) => {
                let peer = connected.running.peer().clone();
                *connection = Some(connected);
                self.generations.fetch_add(1, Ordering::SeqCst);
                self.state.update(|state| {
                    if *state == McpServerState::Connecting {
                        *state = McpServerState::Connected;
                    }
                });
                Ok(peer)
            }
            Err(error) => {
                if let McpServerState::AuthRequired { expired } = self.state.get() {
                    return Err(McpServerError::AuthRequired { expired });
                }
                self.state.set(McpServerState::Failed);
                Err(error)
            }
        }
    }

    async fn connect(&self, cancel: &CancellationToken) -> Result<Connection> {
        let handshake_cancel = CancellationToken::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
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
                    _ = tokio::time::sleep_until(deadline) => return Err(McpServerError::Timeout),
                    result = sse::SseTransport::connect(http_client()?, config, self.authorization.clone(), handshake_cancel.clone()) => result?,
                };
                self.initialize(transport, cancel, handshake_cancel, deadline)
                    .await
            }
            TransportConfig::Stdio(config) => {
                self.initialize(config.spawn()?, cancel, handshake_cancel, deadline)
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
        let (tx, closed) = oneshot::channel();
        let transport = OwnedTransport {
            inner: Some(transport),
            closed: Some(tx),
            state: self.state.clone(),
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
                        return Ok(Connection { running, closed });
                    },
                    Err(_) => {
                        handshake_cancel.cancel();
                        closed.await.map_err(|_| McpServerError::Cleanup)??;
                        return Err(McpServerError::RequestFailed);
                    }
                }
            }
            _ = cancel.cancelled() => McpServerError::Cancelled,
            _ = tokio::time::sleep_until(deadline) => McpServerError::Timeout,
        };
        handshake_cancel.cancel();
        lifetime.cancel();
        if let Ok(running) = initialize.await {
            Connection { running, closed }.close().await?;
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

    /// The request path every call shares, reporting whether the connection
    /// already held the request when it failed.
    async fn exchange(
        &self,
        request: ClientRequest,
        cancel: &CancellationToken,
    ) -> std::result::Result<ServerResult, ViewFailure> {
        let peer = self.peer(cancel).await.map_err(ViewFailure::Refused)?;
        if cancel.is_cancelled() {
            return Err(ViewFailure::Refused(McpServerError::Cancelled));
        }
        let mut handle = peer
            .send_cancellable_request(request, PeerRequestOptions::no_options())
            .await
            .map_err(|_| ViewFailure::Refused(McpServerError::RequestFailed))?;
        let failure = tokio::select! {
            biased;
            _ = cancel.cancelled() => McpServerError::Cancelled,
            _ = tokio::time::sleep(Duration::from_secs(60)) => McpServerError::Timeout,
            response = &mut handle.rx => {
                return match response {
                    Err(_) => Err(ViewFailure::Unanswered(McpServerError::RequestFailed)),
                    Ok(response) => response
                        .map_err(|error| ViewFailure::Unanswered(self.failure(&error))),
                };
            }
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            handle.cancel(Some("MCP operation cancelled".into())),
        )
        .await;
        Err(ViewFailure::Unanswered(failure))
    }

    /// Authorization outcomes of a request, from the transport's own error.
    fn failure(&self, error: &ServiceError) -> McpServerError {
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
            _ => {}
        }
        match error.error.downcast_ref::<McpServerError>() {
            Some(McpServerError::AuthRequired { .. }) => refused,
            Some(McpServerError::InsufficientScope(scopes)) => {
                McpServerError::InsufficientScope(scopes.clone())
            }
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
    oauth: Option<(super::auth::CredentialKey, reqwest_mcp::Url)>,
}
impl McpServerPlugin {
    pub fn new(id: &str, configuration: McpServerSettings, key: Key<McpServer>) -> Result<Self> {
        let oauth = credential_key(id, &configuration);
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
