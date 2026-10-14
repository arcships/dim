//! Session visibility and versioned MCP contributions. No registry or prompt ownership.
use super::{
    McpConnection, McpError, McpServerEntry, McpService,
    server::{McpServerState, McpTool},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::{
    host::{HostError, HostFuture, ToolExecutionContext},
    tool_registry::{
        SessionToolSource, ToolContribution, ToolHandler, ToolProvider, ToolSourceSet,
    },
};
use dim_agent_runtime::{
    AgentBuilder, Capability, InSession, KeysOf, PublicService, SessionConfig, SessionScope,
    SessionService, StateCell, scoped,
};
use dim_agent_types::{ToolCall, ToolDefinition, ToolMetadata, ToolResult, ToolSource};
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// This registers only the Session part; the host registers McpPlugin at root.
pub fn install(builder: &mut AgentBuilder) {
    builder
        .session_tool_source("mcp")
        .session_plugin(McpSessionPlugin::new);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpSourceState {
    Disabled,
    Connecting,
    Ready { version: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpSessionState {
    pub servers: BTreeMap<String, McpSourceState>,
    pub catalog_error: Option<String>,
    pub closed: bool,
}

/// Management view, without credentials, transport handles or registry access.
pub struct McpSession {
    sets: Mutex<BTreeMap<String, Arc<ToolSourceSet>>>,
    state: StateCell<McpSessionState>,
    changed: watch::Sender<()>,
    refresh: watch::Sender<()>,
    stopped: CancellationToken,
}
impl SessionService for McpSession {}
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
        self.changed.send_replace(());
    }
    fn remove(&self, id: &str) {
        let mut sets = self.sets.lock().unwrap();
        sets.remove(id);
        self.state.update(|state| {
            state.servers.remove(id);
        });
        self.changed.send_replace(());
    }
}
// The public management service must not expose executable handlers.
struct Sources(Arc<McpSession>);
impl ToolProvider for Sources {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        self.0.sets.lock().unwrap().values().cloned().collect()
    }
    fn changed(&self) -> watch::Receiver<()> {
        self.0.changed.subscribe()
    }
}

pub struct McpSessionPlugin {
    scope: SessionScope,
}
impl McpSessionPlugin {
    pub fn new(scope: &SessionScope) -> Self {
        Self { scope: *scope }
    }
}
impl TypedPlugin for McpSessionPlugin {
    type Deps = (Arc<McpService>, InSession<SessionConfig>);
    fn name(&self) -> &str {
        "McpSession"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (directory, InSession(config)): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let project = directory.settings.project_root(Path::new(&config.cwd));
            // Subscribe before any file reads, including the first discovery.
            let settings = directory.settings.subscribe();
            let stopped = ctx.cancellation_token();
            let owner = Arc::new(McpSession {
                sets: Mutex::default(),
                state: StateCell::new(McpSessionState::default()),
                changed: watch::Sender::new(()),
                refresh: watch::Sender::new(()),
                stopped: stopped.clone(),
            });
            let refresh = owner.refresh.subscribe();
            let tasks = TaskTracker::new();
            let cleanup = owner.clone();
            let drain = tasks.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        drain.close();
                        drain.wait().await;
                        cleanup.sets.lock().unwrap().clear();
                        cleanup.state.set(McpSessionState {
                            closed: true,
                            ..Default::default()
                        });
                        cleanup.changed.send_replace(());
                        Ok(())
                    })
                }))
            })?;
            ctx.provide_as(self.scope.key::<McpSession>(), owner.clone())?;
            ctx.provide_as(
                self.scope.named_key::<SessionToolSource>("mcp"),
                Arc::new(SessionToolSource(Arc::new(Sources(owner.clone())))),
            )?;
            // No network await in apply, and all owned resources have a disposer.
            let workers = tasks.clone();
            tasks.spawn(reconcile(
                directory, owner, project, settings, refresh, workers,
            ));
            Ok(Effect::Done)
        })
    }
}
impl Capability for McpSessionPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

async fn reconcile(
    directory: Arc<McpService>,
    owner: Arc<McpSession>,
    project: Option<usize>,
    mut settings: watch::Receiver<u64>,
    mut refresh: watch::Receiver<()>,
    tasks: TaskTracker,
) {
    let mut active: BTreeMap<String, (McpServerEntry, CancellationToken)> = BTreeMap::new();
    let mut force = false;
    loop {
        if owner.stopped.is_cancelled() {
            break;
        }
        match directory.catalog(project, &owner.stopped).await {
            Ok(catalog) => {
                owner.state.update(|state| state.catalog_error = None);
                let entries = catalog.servers;
                active.retain(|id, (entry, cancel)| {
                    let keep = !force
                        && entries.get(id).is_some_and(|next| {
                            next.source == entry.source && next.configuration == entry.configuration
                        });
                    if !keep {
                        cancel.cancel();
                        // Keep a complete old set until a replacement is ready or
                        // fails. A deletion/disable, unlike refresh, withdraws now.
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
                // A failed read supplies no new configuration intent. Keep the
                // last accepted sources and their live connection subscriptions.
                owner
                    .state
                    .update(|state| state.catalog_error = Some(error.to_string()));
            }
        }
        force = tokio::select! {
            biased;
            _ = owner.stopped.cancelled() => break,
            result = refresh.changed() => { if result.is_err() { break; } true },
            result = settings.changed() => { if result.is_err() { break; } false },
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
    let result = async {
        let connection = directory.server(project, id.clone(), &cancel).await?;
        let mut changed = connection.tool_changes();
        let mut state = connection.subscribe();
        let mut previous: Option<Vec<McpTool>> = None;
        loop {
            let tools = connection.tools(&cancel).await?;
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
                if matches!(*state.borrow_and_update(), McpServerState::Failed | McpServerState::Closed) {
                    return Err(McpError::Unavailable);
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    result = state.changed() => { if result.is_err() { return Err(McpError::Unavailable); } },
                    result = changed.changed() => { if result.is_err() { return Err(McpError::Unavailable); } break; },
                }
            }
        }
    }.await;
    if let Err(error) = result {
        owner.publish(
            &id,
            None,
            McpSourceState::Failed {
                message: error.to_string(),
            },
            &cancel,
        );
    }
}

fn contribution(
    id: &str,
    tool: &McpTool,
    connection: Arc<McpConnection>,
    stopped: CancellationToken,
) -> Arc<ToolContribution> {
    // Length-delimited identity avoids ambiguous concatenations. A full digest
    // keeps punctuation, long names and Unicode distinct after label sanitizing.
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
            connection,
            original: tool.name.clone(),
            stopped,
        }),
    })
}

struct McpToolHandler {
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
            result.map_err(|error| match error {
                super::server::McpServerError::Cancelled
                | super::server::McpServerError::Closed => HostError::Cancelled(error.to_string()),
                other => HostError::failed(other.to_string()),
            })
        })
    }
}
