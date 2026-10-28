//! Session visibility and versioned MCP contributions. No registry or prompt ownership.
use super::{
    McpConnection, McpError, McpServerEntry, McpService,
    server::{McpServerError, McpServerState, McpTool},
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpSourceState {
    Disabled,
    Connecting,
    Ready {
        version: String,
    },
    Failed {
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpSessionState {
    pub servers: BTreeMap<String, McpSourceState>,
    pub catalog_error: Option<String>,
    pub closed: bool,
}

/// Management view, without credentials, transport handles or registry access.
pub struct McpSession {
    connections: super::McpOwner,
    sets: Mutex<BTreeMap<String, Arc<ToolSourceSet>>>,
    state: StateCell<McpSessionState>,
    ctx: Ctx,
    /// The Session instance, which names its tool provider changes.
    session: InstanceId,
    refresh: watch::Sender<()>,
    stopped: CancellationToken,
}
impl PublicService for McpSession {}
impl McpSession {
    pub fn state(&self) -> McpSessionState {
        self.state.get()
    }
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
        set: Option<Arc<ToolSourceSet>>,
        status: McpSourceState,
        cancel: &CancellationToken,
    ) {
        let mut sets = self.sets.lock().unwrap();
        if cancel.is_cancelled() {
            return;
        }
        match set {
            Some(set) => {
                sets.insert(id.to_owned(), set);
            }
            None => {
                sets.remove(id);
            }
        }
        self.state.update(|state| {
            state.servers.insert(id.to_owned(), status);
        });
        drop(sets);
        self.tools_changed();
    }
    fn remove(&self, id: &str) {
        let mut sets = self.sets.lock().unwrap();
        sets.remove(id);
        self.state.update(|state| {
            state.servers.remove(id);
        });
        drop(sets);
        self.tools_changed();
    }
    fn tools_changed(&self) {
        if let Err(error) = self.ctx.emit(ToolProviderChanged {
            name: tool_provider_changes(self.session),
        }) {
            tracing::warn!(%error, "failed to publish tools.provider_changed");
        }
    }
}
struct Sources(Arc<McpSession>);
impl ToolProvider for Sources {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        self.0.sets.lock().unwrap().values().cloned().collect()
    }
}

pub struct McpSessionPlugin {
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
                connections: super::McpOwner::in_session(
                    ctx.clone(),
                    loader,
                    self.session,
                    (*session).clone(),
                ),
                sets: Mutex::default(),
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
                cleanup.sets.lock().unwrap().clear();
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
    applied: watch::Receiver<BTreeMap<crate::settings::SettingsScope, super::McpApplication>>,
    refresh: watch::Receiver<()>,
}

async fn reconcile(
    directory: Arc<McpService>,
    owner: Arc<McpSession>,
    project: Option<usize>,
    session: &SessionLifecycle,
    mut changes: Changes,
    tasks: TaskTracker,
) {
    let mut active: BTreeMap<String, (McpServerEntry, CancellationToken)> = BTreeMap::new();
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
                active.retain(|id, (entry, cancel)| {
                    let keep = !force
                        && !cancel.is_cancelled()
                        && entries.get(id).is_some_and(|next| {
                            next.source == entry.source && next.configuration == entry.configuration
                        });
                    if !keep {
                        cancel.cancel();
                        if !entries.get(id).is_some_and(McpServerEntry::enabled) {
                            owner.remove(id);
                        }
                    }
                    keep
                });
                for (id, entry) in entries {
                    if active.contains_key(&id) {
                        continue;
                    }
                    let cancel = owner.stopped.child_token();
                    if entry.enabled() {
                        owner.state.update(|state| {
                            state.servers.insert(id.clone(), McpSourceState::Connecting);
                        });
                        tasks.spawn(discover(
                            directory.clone(),
                            owner.clone(),
                            project,
                            id.clone(),
                            cancel.clone(),
                        ));
                    } else {
                        owner.publish(&id, None, McpSourceState::Disabled, &cancel);
                    }
                    active.insert(id, (entry, cancel));
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
    for (_, (_, cancel)) in active {
        cancel.cancel();
    }
}

async fn discover(
    directory: Arc<McpService>,
    owner: Arc<McpSession>,
    project: Option<usize>,
    id: String,
    cancel: CancellationToken,
) {
    let mut lease = None;
    let result = async {
        let connection = directory.server(owner.connections.clone(), project, id.clone(), &cancel).await?;
        lease = Some(connection.clone());
        let mut changed = connection.tool_changes();
        let mut state = connection.subscribe();
        let mut credential = connection.credential_changes();
        let mut previous: Option<Vec<McpTool>> = None;
        'discover: loop {
            if let Some(credential) = credential.as_mut() {
                credential.borrow_and_update();
            }
            let refused = match connection.tools(&cancel).await {
                Err(McpServerError::AuthRequired { expired }) => Some(expired),
                // Refused for scope while discovering: sign in again (§5.7).
                Err(McpServerError::InsufficientScope(_)) => Some(false),
                result => {
                    let tools = result?;
                    if matches!(connection.state(), McpServerState::Failed | McpServerState::Closed) {
                        return Err(McpError::Unavailable);
                    }
                    if previous.as_ref() != Some(&tools) {
                        let version = uuid::Uuid::new_v4().to_string();
                        let set = Arc::new(ToolSourceSet {
                            key: format!("mcp:{id}"), version: version.clone(),
                            tools: tools.iter().map(|tool| contribution(&id, tool, connection.clone(), owner.stopped.clone())).collect(),
                        });
                        owner.publish(&id, Some(set), McpSourceState::Ready { version }, &cancel);
                        previous = Some(tools);
                    }
                    loop {
                        let current = *state.borrow_and_update();
                        match current {
                            McpServerState::Failed | McpServerState::Closed => return Err(McpError::Unavailable),
                            McpServerState::AuthRequired { expired } => break Some(expired),
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
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    result = credential_change(credential.as_mut()) => { result?; }
                }
            }
        }
    }.await;
    let withdrawn = lease.take().is_some_and(|lease| lease.withdrawn());
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

pub(super) fn contribution(
    id: &str,
    tool: &McpTool,
    connection: Arc<McpConnection>,
    stopped: CancellationToken,
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
            server: id.to_owned(),
            connection,
            original: tool.name.clone(),
            stopped,
        }),
    })
}

struct McpToolHandler {
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
                Ok(result) => Ok(result),
            }
        })
    }
}
