//! Session visibility and versioned MCP contributions. No registry or prompt ownership.
use super::{
    McpConnection, McpError, McpServerEntry, McpService,
    server::{APP_MEDIA_TYPE, McpServerError, McpServerState, McpTool, ToolScope, ViewFailure},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::{
    host::{HostError, HostFuture, ToolExecutionContext},
    tool_registry::{
        ToolContribution, ToolHandler, ToolProvider, ToolProviderChanged, ToolRegistry,
        ToolSourceSet, tool_provider_changes,
    },
};
use dim_agent_runtime::{
    AgentBuilder, Names, PublicService, SESSION_GROUP, SessionConfig, SessionLifecycle,
    SessionLoader, StateCell,
};
use dim_agent_types::{
    ToolCall, ToolContent, ToolDefinition, ToolMetadata, ToolResult, ToolSource,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, InstanceId, Key, TypeKey};
use rutis::{Ctx, Error};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Requires the Session part in the profile and adds the per-server
/// extension; McpPlugin, which the host places at the process, registers the
/// part.
pub fn install(builder: &mut AgentBuilder) {
    crate::register_names(builder.catalog_mut());
    builder
        .session_extension_part::<super::publication::ConnectionConfig>(
            super::publication::GROUP,
            "McpServer",
            |_, value| Ok(Box::new(super::publication::ConnectionPlugin::new(value))),
        )
        .require_part("McpSession")
        .require_part("TemporaryMcp");
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum McpSourceState {
    Disabled,
    Connecting,
    Ready {
        version: String,
    },
    Failed {
        message: String,
    },
    /// The transport is gone but the last tools stay published and callable.
    /// The reason is kept until a connection succeeds.
    Disconnected {
        message: String,
    },
    /// The server needs the user to sign in; clears by itself once the
    /// server's credential changes.
    AuthRequired {
        expired: bool,
    },
}
impl McpSourceState {
    /// `session.mcp` state name with its version and failure message. MCP owns
    /// this vocabulary; the protocol projection only copies it.
    pub fn into_wire(self) -> (&'static str, Option<String>, Option<String>) {
        match self {
            Self::Disabled => ("disabled", None, None),
            Self::Connecting => ("connecting", None, None),
            Self::Ready { version } => ("ready", Some(version), None),
            Self::Failed { message } => ("failed", None, Some(message)),
            Self::Disconnected { message } => ("disconnected", None, Some(message)),
            Self::AuthRequired { expired } => (
                "auth_required",
                None,
                Some(
                    if expired {
                        "authorization expired"
                    } else {
                        "authorization required"
                    }
                    .into(),
                ),
            ),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSessionState {
    pub servers: BTreeMap<String, McpSourceState>,
    pub catalog_error: Option<String>,
    pub closed: bool,
}

/// The mount read's reply (MCP Apps §4.1). `text` and `blob` are exclusive;
/// `app` is the document's own `_meta.ui`, which the host applies.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpWidgetResult {
    pub server: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: dim_agent_types::JsonSchema,
    pub uri: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<dim_agent_types::JsonObject>,
}

/// A view's tool result (MCP Apps §4.2), `_meta` included. It is the reply to
/// the client that asked; the Timeline's `ToolResult` is never built from it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppCallResult {
    pub content: ToolContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<dim_agent_types::JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<dim_agent_types::JsonObject>,
}

/// Why `session.mcp.app` did not answer (MCP Apps §4.3). The messages are the
/// daemon's own: nothing a server answered reaches the reply, which can carry
/// an endpoint or a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpAppError {
    #[error("the tool declares no widget document")]
    Absent,
    #[error("the widget document is not a text/html;profile=mcp-app resource")]
    Unsupported,
    #[error("the tool is not callable by an app")]
    NotCallable,
    #[error("the MCP server is not an entry of this Session")]
    UnknownServer,
    #[error("the MCP server has no usable connection")]
    Unavailable,
    #[error("the request did not reach the MCP connection")]
    Refused,
    #[error("the request reached the MCP connection and its outcome is unknown")]
    Unknown,
}
impl From<ViewFailure> for McpAppError {
    fn from(failure: ViewFailure) -> Self {
        match failure {
            // A refused credential is the one answer that ends the entry's
            // connection without running anything, on either side of the
            // hand-off.
            ViewFailure::Refused(McpServerError::AuthRequired { .. })
            | ViewFailure::Refused(McpServerError::InsufficientScope(_))
            | ViewFailure::Unanswered(McpServerError::AuthRequired { .. })
            | ViewFailure::Unanswered(McpServerError::InsufficientScope(_)) => Self::Unavailable,
            ViewFailure::Refused(_) => Self::Refused,
            ViewFailure::Unanswered(_) => Self::Unknown,
        }
    }
}

/// The document a host is handed, which it caches and sandboxes (MCP Apps
/// D11); a call's result has the connection's own size behavior.
const MAX_APP_DOCUMENT: usize = 4 * 1024 * 1024;

/// An entry's connection serves a request only while it can open or hold one.
/// Every other state is a refusal nothing was sent on (MCP Apps §5).
fn usable(state: McpServerState) -> bool {
    matches!(
        state,
        McpServerState::Disconnected | McpServerState::Connected
    )
}

/// Management view, without credentials, transport handles or registry access.
pub struct McpSession {
    files: Option<Arc<dim_agent_capabilities::host::SessionFilesHost>>,
    session_id: String,
    connections: super::McpOwner,
    entries: Mutex<BTreeMap<String, Published>>,
    names: Mutex<BTreeMap<String, PublishedTool>>,
    state: StateCell<McpSessionState>,
    ctx: Ctx,
    /// The Session instance, which names its tool provider changes.
    session: InstanceId,
    refresh: watch::Sender<()>,
    stopped: CancellationToken,
}

/// What the Session holds for one entry of its selection: the tools it
/// published to the model and the connection generation they came from. A
/// view's mount read and call use the same generation, so a replaced one
/// cannot serve a view a tool list the entry no longer declares.
struct Published {
    set: Arc<ToolSourceSet>,
    connection: Arc<McpConnection>,
}

/// A published tool name and what a mount read resolves it to. The Session
/// keeps the names of an entry it stopped publishing — disabled, connecting,
/// failed or awaiting sign-in — so a view mounted from an earlier `tool/call`
/// fact learns that its entry is unusable (`mcp_unavailable`) instead of that
/// the name is unknown (`mcp_app_absent`, MCP Apps §4.3).
#[derive(Clone)]
struct PublishedTool {
    server: String,
    name: String,
}
impl PublicService for McpSession {}
#[rutis::service]
impl McpSession {
    /// This Session's entries and their states: what the `mcpSession`
    /// face's `state` reads.
    pub fn state(&self) -> McpSessionState {
        self.state.get()
    }
    /// The state watch Rust callers read beside the face's snapshot.
    #[service(skip)]
    pub fn subscribe(&self) -> watch::Receiver<McpSessionState> {
        self.state.subscribe()
    }
    /// Requests rediscovery, including external file edits and failed connections.
    /// Completion is reflected in state and B's ToolSourcesView, not this return.
    pub fn refresh(&self) -> Result<(), McpError> {
        if self.stopped.is_cancelled() {
            return Err(McpError::Closed);
        }
        self.refresh.send_replace(());
        Ok(())
    }
    fn publish(
        &self,
        id: &str,
        published: Option<(Arc<ToolSourceSet>, Arc<McpConnection>)>,
        status: McpSourceState,
        cancel: &CancellationToken,
    ) {
        let mut entries = self.entries.lock().unwrap();
        let mut names = self.names.lock().unwrap();
        if cancel.is_cancelled() {
            return;
        }
        match published {
            Some((set, connection)) => {
                names.retain(|_, tool| tool.server != id);
                for tool in &set.tools {
                    if let Some(origin) = published_tool(&tool.definition) {
                        names.insert(tool.definition.name.clone(), origin);
                    }
                }
                entries.insert(id.to_owned(), Published { set, connection });
            }
            None => {
                entries.remove(id);
            }
        }
        self.state.update(|state| {
            state.servers.insert(id.to_owned(), status);
        });
        drop(names);
        drop(entries);
        self.tools_changed();
    }
    fn remove(&self, id: &str) {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(id);
        self.state.update(|state| {
            state.servers.remove(id);
        });
        drop(entries);
        self.tools_changed();
    }
    fn tools_changed(&self) {
        if let Err(error) = self.ctx.emit(ToolProviderChanged {
            name: tool_provider_changes(self.session),
        }) {
            tracing::warn!(%error, "failed to publish tools.provider_changed");
        }
    }

    /// The widget a published tool declares (MCP Apps D4): its entry, the
    /// server's own tool name, the document that entry's connection serves,
    /// and the tool definition a host applies to the view. What the
    /// `mcpSession` face's `widget` answers; a dropped call cancels the
    /// document read.
    pub async fn widget(
        &self,
        published: String,
    ) -> std::result::Result<McpWidgetResult, McpAppError> {
        if self.stopped.is_cancelled() {
            return Err(McpAppError::Unavailable);
        }
        let cancel = self.stopped.child_token();
        let _dropped = cancel.clone().drop_guard();
        let resolved = self
            .names
            .lock()
            .unwrap()
            .get(&published)
            .cloned()
            .ok_or(McpAppError::Absent)?;
        let server = resolved.server;
        let name = resolved.name;
        let connection = {
            let entries = self.entries.lock().unwrap();
            entries
                .get(&server)
                .map(|entry| entry.connection.clone())
                .ok_or(McpAppError::Unavailable)?
        };
        if !usable(connection.state()) {
            return Err(McpAppError::Unavailable);
        }
        let discovered = connection.discovered().ok_or(McpAppError::Absent)?;
        let tool = discovered
            .iter()
            .find(|tool| tool.name == name)
            .ok_or(McpAppError::Absent)?;
        let uri = tool
            .ui
            .as_ref()
            .and_then(|ui| ui.resource_uri.clone())
            .ok_or(McpAppError::Absent)?;
        if !uri.starts_with("ui://") {
            return Err(McpAppError::Unsupported);
        }
        let document = connection
            .resource(uri.clone(), &cancel)
            .await
            .map_err(McpAppError::from)?;
        let mime_type = document.mime_type.ok_or(McpAppError::Unsupported)?;
        if mime_type != APP_MEDIA_TYPE {
            return Err(McpAppError::Unsupported);
        }
        let size = match (&document.text, &document.blob) {
            (Some(text), None) => text.len(),
            (None, Some(blob)) => base64::decoded_len_estimate(blob.len()),
            _ => return Err(McpAppError::Unsupported),
        };
        if size > MAX_APP_DOCUMENT {
            return Err(McpAppError::Unsupported);
        }
        Ok(McpWidgetResult {
            server,
            name,
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            uri,
            mime_type,
            text: document.text,
            blob: document.blob,
            app: document
                .meta
                .and_then(|meta| meta.get("ui").and_then(|ui| ui.as_object()).cloned()),
        })
    }

    /// One tool call on one entry of this Session, on that entry's own
    /// connection (MCP Apps D5). It is the user's action on an enabled entry,
    /// not an agent decision: no branch admits it and it writes no fact. What
    /// the `mcpSession` face's `call` answers; a dropped call cancels the
    /// server request.
    pub async fn call(
        &self,
        server: String,
        tool: String,
        arguments: dim_agent_types::JsonObject,
    ) -> std::result::Result<McpAppCallResult, McpAppError> {
        if self.stopped.is_cancelled() {
            return Err(McpAppError::Unavailable);
        }
        let cancel = self.stopped.child_token();
        let _dropped = cancel.clone().drop_guard();
        if !self.state.get().servers.contains_key(&server) {
            return Err(McpAppError::UnknownServer);
        }
        let connection = {
            let entries = self.entries.lock().unwrap();
            entries
                .get(&server)
                .map(|entry| entry.connection.clone())
                .ok_or(McpAppError::Unavailable)?
        };
        if !usable(connection.state()) {
            return Err(McpAppError::Unavailable);
        }
        let discovered = connection.discovered().ok_or(McpAppError::Unavailable)?;
        let tool = discovered
            .iter()
            .find(|discovered| discovered.name == tool)
            .ok_or(McpAppError::Absent)?;
        if !tool.ui.as_ref().is_none_or(|ui| ui.allows(ToolScope::App)) {
            return Err(McpAppError::NotCallable);
        }
        let answer = connection
            .call_result(tool.name.clone(), arguments, &cancel)
            .await
            .map_err(McpAppError::from)?;
        Ok(McpAppCallResult {
            content: ToolContent::Blocks(answer.content),
            structured_content: answer.structured_content,
            is_error: answer.is_error,
            meta: answer.meta,
        })
    }
}
struct Sources(Arc<McpSession>);
impl ToolProvider for Sources {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        self.0
            .entries
            .lock()
            .unwrap()
            .values()
            .map(|entry| entry.set.clone())
            .collect()
    }
}

pub struct McpSessionPlugin {
    files: Key<dim_agent_capabilities::host::SessionFilesHost>,
    identity: Key<dim_agent_runtime::SessionIdentity>,
    session: InstanceId,
    mcp: Key<McpSession>,
    directory: Key<McpService>,
    config: Key<SessionConfig>,
    loader: Key<SessionLoader>,
    lifecycle: Key<SessionLifecycle>,
    tools: Key<ToolRegistry>,
}
impl McpSessionPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            files: names.key()?,
            identity: names.key()?,
            session: names.instance(SESSION_GROUP)?,
            mcp: names.key()?,
            directory: names.key()?,
            config: names.key()?,
            loader: names.key()?,
            lifecycle: names.key()?,
            tools: names.key()?,
        })
    }
}
impl DynPlugin for McpSessionPlugin {
    fn name(&self) -> &str {
        "McpSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.directory.erase(),
            self.identity.erase(),
            self.config.erase(),
            self.loader.erase(),
            self.lifecycle.erase(),
            self.tools.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let directory = ctx.require_at(&self.directory)?;
            let config = ctx.require_at(&self.config)?;
            let loader = ctx.require_at(&self.loader)?;
            let session = ctx.require_at(&self.lifecycle)?;
            let tools = ctx.require_at(&self.tools)?;
            let project = directory.settings.project_root(Path::new(&config.cwd));
            let settings = super::config::settings_changes(ctx)?;
            let applied = directory.publication.applied();
            let stopped = ctx.cancellation_token();
            let owner = Arc::new(McpSession {
                files: ctx.get_at(&self.files),
                session_id: ctx.require_at(&self.identity)?.session_id.clone(),
                connections: super::McpOwner::in_session(
                    ctx.clone(),
                    loader,
                    self.session,
                    (*session).clone(),
                ),
                entries: Mutex::default(),
                names: Mutex::default(),
                state: StateCell::new(McpSessionState::default()),
                ctx: ctx.clone(),
                session: self.session,
                refresh: watch::Sender::new(()),
                stopped: stopped.clone(),
            });
            let refresh = owner.refresh.subscribe();
            let tasks = TaskTracker::new();
            let cleanup = owner.clone();
            let drain = tasks.clone();
            let publication = directory.publication.clone();
            ctx.effect(move || async move {
                drain.close();
                drain.wait().await;
                cleanup.entries.lock().unwrap().clear();
                cleanup.names.lock().unwrap().clear();
                cleanup.state.set(McpSessionState {
                    closed: true,
                    ..Default::default()
                });
                publication.refresh();
                Ok(())
            })?;
            ctx.provide_at(self.mcp.clone(), owner.clone())?;
            tools.register_source(ctx, "mcp", Arc::new(Sources(owner.clone())))?;
            let workers = tasks.clone();
            tasks.spawn(async move {
                tokio::select! {
                    _ = owner.stopped.cancelled() => return,
                    published = session.activated() => if !published { return },
                }
                let changes = Changes {
                    settings,
                    applied,
                    refresh,
                };
                reconcile(directory, owner, project, &session, changes, workers).await
            });
            Ok(Effect::Done)
        })
    }
}

/// What makes a Session re-read its selection: a settings write, a change
/// the publisher applied (including external file edits), or a refresh.
struct Changes {
    settings: watch::Receiver<()>,
    applied: watch::Receiver<BTreeMap<super::McpSource, super::McpApplication>>,
    refresh: watch::Receiver<()>,
}

/// One entry of the Session's selection that its reconcile keeps active.
struct Active {
    entry: McpServerEntry,
    cancel: CancellationToken,
    /// The generation this entry's discovery acquired, kept for as long as the
    /// entry stays selected: a refresh replaces the discovery without leaving
    /// the generation with no lease, which would retire it (ADR 004) and with
    /// it the start backoff the generation recorded.
    lease: Arc<Lease>,
}

/// The generation an entry's discovery holds. `reconcile` keeps one per active
/// entry; the discovering task keeps the connection in it.
#[derive(Default)]
struct Lease(Mutex<Option<Arc<McpConnection>>>);
impl Lease {
    fn hold(&self, connection: &Arc<McpConnection>) {
        *self.0.lock().unwrap() = Some(connection.clone());
    }
    /// Drops the hold: a discovery that ended on its own error leaves nothing
    /// using the generation, so it retires (ADR 004).
    fn release(&self) {
        *self.0.lock().unwrap() = None;
    }
}

/// Publishes the entry as `Connecting` and runs one discovery for it with
/// `cancel` and `lease`.
fn start_discovery(
    directory: &Arc<McpService>,
    owner: &Arc<McpSession>,
    project: Option<usize>,
    id: &str,
    cancel: &CancellationToken,
    lease: &Arc<Lease>,
    tasks: &TaskTracker,
) {
    owner.state.update(|state| {
        state
            .servers
            .insert(id.to_owned(), McpSourceState::Connecting);
    });
    tasks.spawn(discover(
        directory.clone(),
        owner.clone(),
        project,
        id.to_owned(),
        cancel.clone(),
        lease.clone(),
    ));
}

async fn reconcile(
    directory: Arc<McpService>,
    owner: Arc<McpSession>,
    project: Option<usize>,
    session: &SessionLifecycle,
    mut changes: Changes,
    tasks: TaskTracker,
) {
    let mut active: BTreeMap<String, Active> = BTreeMap::new();
    let mut force = false;
    let closing = session.closing();
    tokio::pin!(closing);
    loop {
        if owner.stopped.is_cancelled() {
            break;
        }
        let catalog = tokio::select! {
            biased;
            _ = &mut closing => break,
            catalog = directory.catalog(project, &owner.stopped) => catalog,
        };
        match catalog {
            Ok(catalog) => {
                owner.state.update(|state| state.catalog_error = None);
                let entries = catalog.servers;
                active.retain(|id, active| {
                    let keep = !active.cancel.is_cancelled()
                        && entries.get(id).is_some_and(|next| {
                            next.source == active.entry.source
                                && next.configuration == active.entry.configuration
                        });
                    if !keep {
                        active.cancel.cancel();
                        if !entries.get(id).is_some_and(McpServerEntry::enabled) {
                            owner.remove(id);
                        }
                    }
                    keep
                });
                if force {
                    for (id, active) in active.iter_mut() {
                        active.cancel.cancel();
                        active.cancel = owner.stopped.child_token();
                        if active.entry.enabled() {
                            start_discovery(
                                &directory,
                                &owner,
                                project,
                                id,
                                &active.cancel,
                                &active.lease,
                                &tasks,
                            );
                        }
                    }
                }
                for (id, entry) in entries {
                    if active.contains_key(&id) {
                        continue;
                    }
                    let cancel = owner.stopped.child_token();
                    let lease = Arc::new(Lease::default());
                    if entry.enabled() {
                        start_discovery(&directory, &owner, project, &id, &cancel, &lease, &tasks);
                    } else {
                        owner.publish(&id, None, McpSourceState::Disabled, &cancel);
                    }
                    active.insert(
                        id,
                        Active {
                            entry,
                            cancel,
                            lease,
                        },
                    );
                }
            }
            Err(error) => {
                owner
                    .state
                    .update(|state| state.catalog_error = Some(error.to_string()));
            }
        }
        force = tokio::select! {
            biased;
            _ = &mut closing => break,
            _ = owner.stopped.cancelled() => break,
            result = changes.refresh.changed() => { if result.is_err() { break; } true },
            result = changes.settings.changed() => { if result.is_err() { break; } false },
            result = changes.applied.changed() => { if result.is_err() { break; } false },
        };
    }
    for (_, active) in active {
        active.cancel.cancel();
    }
}

async fn discover(
    directory: Arc<McpService>,
    owner: Arc<McpSession>,
    project: Option<usize>,
    id: String,
    cancel: CancellationToken,
    lease: Arc<Lease>,
) {
    let mut held = None;
    let result = async {
        let connection = directory.server(owner.connections.clone(), project, id.clone(), &cancel).await?;
        lease.hold(&connection);
        held = Some(connection.clone());
        let mut changed = connection.tool_changes();
        let mut state = connection.subscribe();
        let mut credential = connection.credential_changes();
        let mut previous: Option<Vec<McpTool>> = None;
        let mut published: Option<Arc<ToolSourceSet>> = None;
        let mut generation = connection.generation();
        'discover: loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            if let Some(credential) = credential.as_mut() {
                credential.borrow_and_update();
            }
            // Mark the current state seen: a wake below then means something
            // changed while the attempt ran or after it.
            let _ = state.borrow_and_update();
            let refused = match connection.tools(&cancel).await {
                Err(McpServerError::AuthRequired { expired }) => Some(expired),
                // Refused for scope while discovering: sign in again (§5.7).
                Err(McpServerError::InsufficientScope(_)) => Some(false),
                result => {
                    let tools = match result {
                        Ok(tools) => tools,
                        Err(error) => {
                            if matches!(error, McpServerError::Closed) {
                                return Err(McpError::Unavailable);
                            }
                            if matches!(error, McpServerError::Cancelled) {
                                return Ok(());
                            }
                            let message = connection.failure().unwrap_or_else(|| error.to_string());
                            let set = published.clone().map(|set| (set, connection.clone()));
                            match connection.state() {
                                McpServerState::Failed => {
                                    // A start failure: keep the last tools,
                                    // and try again when the cooldown ends.
                                    owner.publish(
                                        &id,
                                        set,
                                        McpSourceState::Failed { message },
                                        &cancel,
                                    );
                                    wait_for_attempt(
                                        &cancel,
                                        &mut state,
                                        &mut changed,
                                        &mut credential,
                                        true,
                                    )
                                    .await?;
                                    continue 'discover;
                                }
                                McpServerState::Disconnected => {
                                    // The transport is gone. The last tools
                                    // stay callable; the next use reconnects.
                                    owner.publish(
                                        &id,
                                        set,
                                        McpSourceState::Disconnected { message },
                                        &cancel,
                                    );
                                    wait_for_attempt(
                                        &cancel,
                                        &mut state,
                                        &mut changed,
                                        &mut credential,
                                        false,
                                    )
                                    .await?;
                                    continue 'discover;
                                }
                                _ => return Err(error.into()),
                            }
                        }
                    };
                    if matches!(connection.state(), McpServerState::Closed) {
                        return Err(McpError::Unavailable);
                    }
                    // A new connection is a new discovery even when it declares
                    // the same tools: a client caches a view's document under
                    // this version (MCP Apps D3).
                    let current = connection.generation();
                    if previous.as_ref() != Some(&tools) || current != generation {
                        let version = uuid::Uuid::new_v4().to_string();
                        let set = Arc::new(ToolSourceSet {
                            key: format!("mcp:{id}"), version: version.clone(),
                            tools: tools.iter().filter(|tool| model_visible(tool)).map(|tool| contribution(&id, tool, connection.clone(), owner.stopped.clone(), owner.files.clone(), owner.session_id.clone())).collect(),
                        });
                        owner.publish(&id, Some((set.clone(), connection.clone())), McpSourceState::Ready { version }, &cancel);
                        published = Some(set);
                        previous = Some(tools);
                        generation = current;
                    }
                    loop {
                        let current = *state.borrow_and_update();
                        match current {
                            McpServerState::Closed => return Err(McpError::Unavailable),
                            McpServerState::AuthRequired { expired } => break Some(expired),
                            McpServerState::Failed => {
                                let message = connection.failure().unwrap_or_else(|| "MCP connection failed".into());
                                owner.publish(
                                    &id,
                                    published.clone().map(|set| (set, connection.clone())),
                                    McpSourceState::Failed { message },
                                    &cancel,
                                );
                                wait_for_attempt(
                                    &cancel,
                                    &mut state,
                                    &mut changed,
                                    &mut credential,
                                    true,
                                )
                                .await?;
                                continue 'discover;
                            }
                            McpServerState::Disconnected => {
                                let message = connection.failure().unwrap_or_else(|| "MCP connection closed".into());
                                owner.publish(
                                    &id,
                                    published.clone().map(|set| (set, connection.clone())),
                                    McpSourceState::Disconnected { message },
                                    &cancel,
                                );
                                wait_for_attempt(
                                    &cancel,
                                    &mut state,
                                    &mut changed,
                                    &mut credential,
                                    false,
                                )
                                .await?;
                                continue 'discover;
                            }
                            _ => {}
                        }
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => return Ok(()),
                            result = state.changed() => { if result.is_err() { return Err(McpError::Unavailable); } },
                            result = changed.changed() => { if result.is_err() { return Err(McpError::Unavailable); } continue 'discover; },
                            usable = credential_change(credential.as_mut()) => { if !usable? { continue 'discover; } },
                        }
                    }
                }
            };
            if let Some(expired) = refused {
                owner.publish(&id, None, McpSourceState::AuthRequired { expired }, &cancel);
                previous = None;
                published = None;
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    result = credential_change(credential.as_mut()) => { result?; }
                }
            }
        }
    }.await;
    let withdrawn = held.take().is_some_and(|held| held.withdrawn());
    // The replacement owns the slot once this discovery is cancelled; only a
    // discovery that ended on its own error releases the generation.
    if result.is_err() && !cancel.is_cancelled() {
        lease.release();
    }
    match result {
        Ok(()) => {}
        Err(error) if withdrawn || matches!(error, McpError::Disabled) => {
            owner.publish(&id, None, McpSourceState::Disabled, &cancel);
            cancel.cancel();
        }
        Err(error) => owner.publish(
            &id,
            None,
            McpSourceState::Failed {
                message: error.to_string(),
            },
            &cancel,
        ),
    }
}

/// Wait until another attempt can succeed: a start-failure cooldown ends, a
/// caller reconnects the server, or the credential changes. With
/// `after_cooldown`, only a state that is still a cooling failure waits; a
/// state that already left it retries at once.
async fn wait_for_attempt(
    cancel: &CancellationToken,
    state: &mut watch::Receiver<McpServerState>,
    changed: &mut watch::Receiver<u64>,
    credential: &mut Option<watch::Receiver<bool>>,
    after_cooldown: bool,
) -> std::result::Result<(), McpError> {
    if after_cooldown && !matches!(*state.borrow_and_update(), McpServerState::Failed) {
        return Ok(());
    }
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            result = state.changed() => {
                if result.is_err() {
                    return Err(McpError::Unavailable);
                }
                return Ok(());
            }
            result = changed.changed() => {
                if result.is_err() {
                    return Err(McpError::Unavailable);
                }
                return Ok(());
            }
            usable = credential_change(credential.as_mut()) => {
                if usable? {
                    return Ok(());
                }
            }
        }
    }
}

/// The next change of the server's OAuth credential, and whether it now
/// holds tokens; never resolves for a server without OAuth.
async fn credential_change(
    credential: Option<&mut watch::Receiver<bool>>,
) -> Result<bool, McpError> {
    let Some(credential) = credential else {
        return std::future::pending().await;
    };
    credential
        .changed()
        .await
        .map_err(|_| McpError::Unavailable)?;
    Ok(*credential.borrow_and_update())
}

/// What a mount read resolves a published contribution to: its entry and the
/// server's own tool name, as the producer recorded them.
fn published_tool(definition: &ToolDefinition) -> Option<PublishedTool> {
    let metadata = definition.metadata.as_ref()?;
    Some(PublishedTool {
        server: metadata.mcp_server_id.clone()?,
        name: metadata.mcp_original_tool_name.clone()?,
    })
}

/// A tool whose visibility excludes the model is published to no model: only
/// a view of its server may call it (MCP Apps D7).
fn model_visible(tool: &McpTool) -> bool {
    tool.ui
        .as_ref()
        .is_none_or(|ui| ui.allows(ToolScope::Model))
}

pub(super) fn contribution(
    id: &str,
    tool: &McpTool,
    connection: Arc<McpConnection>,
    stopped: CancellationToken,
    files: Option<Arc<dim_agent_capabilities::host::SessionFilesHost>>,
    session_id: String,
) -> Arc<ToolContribution> {
    let mut hash = Sha256::new();
    hash.update((id.len() as u64).to_be_bytes());
    hash.update(id.as_bytes());
    hash.update(tool.name.as_bytes());
    let label: String = format!("{id}_{}", tool.name)
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .take(15)
        .collect();
    let name = format!("mcp_{label}_{}", URL_SAFE_NO_PAD.encode(hash.finalize()));
    Arc::new(ToolContribution {
        definition: ToolDefinition {
            name,
            description: tool.description.clone().unwrap_or_default(),
            input_schema: tool.input_schema.clone(),
            source: ToolSource::Dynamic,
            risk: None,
            file_changes: None,
            metadata: Some(ToolMetadata {
                mcp_server_id: Some(id.into()),
                mcp_original_tool_name: Some(tool.name.clone()),
                ..Default::default()
            }),
        },
        handler: Arc::new(McpToolHandler {
            files,
            session_id,
            server: id.to_owned(),
            connection,
            original: tool.name.clone(),
            stopped,
        }),
    })
}

struct McpToolHandler {
    files: Option<Arc<dim_agent_capabilities::host::SessionFilesHost>>,
    session_id: String,
    server: String,
    connection: Arc<McpConnection>,
    original: String,
    stopped: CancellationToken,
}
impl ToolHandler for McpToolHandler {
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.stopped.is_cancelled() {
                return Err(HostError::Cancelled("MCP Session closed".into()));
            }
            let cancel = cancel.child_token();
            let operation = self
                .connection
                .call(self.original.clone(), call.input, &cancel);
            tokio::pin!(operation);
            let result = tokio::select! {
                biased;
                _ = self.stopped.cancelled() => { cancel.cancel(); operation.await },
                result = &mut operation => result,
            };
            let server = &self.server;
            let refused = |text: String| {
                Ok(ToolResult {
                    content: ToolContent::Text(text),
                    is_error: Some(true),
                    ..ToolResult::default()
                })
            };
            match result {
                Err(McpServerError::AuthRequired { .. }) => refused(format!(
                    "The MCP server \"{server}\" requires the user to sign in. Ask the user to sign in to it with /mcp."
                )),
                Err(McpServerError::InsufficientScope(scopes)) => refused(format!(
                    "The MCP server \"{server}\" needs additional permissions for this tool{}. Ask the user to sign in to it again with /mcp.",
                    if scopes.is_empty() {
                        String::new()
                    } else {
                        format!(" ({scopes})")
                    }
                )),
                Err(error @ (McpServerError::Cancelled | McpServerError::Closed)) => {
                    Err(HostError::Cancelled(error.to_string()))
                }
                Err(other) => Err(HostError::failed(other.to_string())),
                Ok(mut result) => {
                    if let Some(files) = &self.files {
                        dim_agent_capabilities::session_files::store_result(
                            files,
                            &self.session_id,
                            &mut result,
                            &cancel,
                        )
                        .await?;
                    }
                    Ok(result)
                }
            }
        })
    }
}
