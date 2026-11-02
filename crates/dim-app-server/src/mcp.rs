//! MCP configuration directory. Connections belong to McpServer, not Settings.
mod auth;
pub mod config;
pub(crate) mod management;
mod publication;
#[cfg(feature = "testing")]
pub use auth::testing;
pub use auth::{McpAuth, McpAuthError};
pub mod server;
mod session;
mod temporary;
pub(crate) use publication::Publication;
pub use publication::{McpApplication, McpOwner, McpPublication};
pub use session::{
    McpAppCallResult, McpAppError, McpSession, McpSessionPlugin, McpSessionState, McpSourceState,
    McpWidgetResult, install,
};

use crate::settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService};
use config::{
    McpPluginDocument, McpPluginServers, McpProjectEnablement, McpServerSettings, McpSettings,
};
use dim_agent_capabilities::file_storage::FileStorage;
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use server::{McpServer, McpServerError};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("empty MCP server id")]
    InvalidId,
    #[error("MCP server is not configured in this source")]
    UnknownServer,
    #[error("MCP server is disabled")]
    Disabled,
    #[error("MCP server Plugin could not be loaded")]
    Unavailable,
    #[error(transparent)]
    Server(#[from] McpServerError),
    #[error(transparent)]
    Auth(#[from] McpAuthError),
    #[error("MCP directory is closed")]
    Closed,
    #[error("MCP operation cancelled")]
    Cancelled,
    #[error("MCP worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}
pub type Result<T> = std::result::Result<T, McpError>;

/// Where an entry comes from. Plugin entries come after the user's and
/// before a project's, which shadows both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpSource {
    User,
    Plugins,
    Project(usize),
}
impl McpSource {
    /// The Settings source a user or project entry is written in; plugin
    /// entries change only through their plugin.
    pub fn scope(self) -> Option<SettingsScope> {
        match self {
            Self::User => Some(SettingsScope::User),
            Self::Project(root) => Some(SettingsScope::Project(root)),
            Self::Plugins => None,
        }
    }
}
impl From<SettingsScope> for McpSource {
    fn from(scope: SettingsScope) -> Self {
        match scope {
            SettingsScope::User => Self::User,
            SettingsScope::Project(root) => Self::Project(root),
        }
    }
}

/// Server-side configuration, including secrets. Not a wire or logging type.
/// For a project entry, the configuration is effective: its enablement comes
/// from the user's record (`config::effective`).
#[derive(Clone)]
pub struct McpServerEntry {
    pub source: McpSource,
    pub configuration: McpServerSettings,
}
impl McpServerEntry {
    pub fn enabled(&self) -> bool {
        self.configuration.enabled()
    }
}

/// One source's entry as that source declares it, for editing: the declared
/// configuration, the source revision a write takes (absent for a plugin
/// entry) and the enablement the entry runs with.
pub struct McpEditableEntry {
    pub configuration: McpServerSettings,
    pub revision: Option<String>,
    pub enabled: bool,
}

/// The terminal outcome of one probe. `mcp.test` exercises a configuration
/// with a disposable connection and reports what happened rather than failing
/// the call.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpProbe {
    pub status: ProbeStatus,
    /// Milliseconds from opening the disposable connection to this outcome,
    /// excluding cleanup.
    pub latency_ms: u64,
    /// Every discovered tool, or `null` unless the probe connected.
    pub tools: Option<Vec<server::McpTool>>,
    /// The refusal is a credential Dim held and the server rejected, not one
    /// Dim lacks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expired: Option<bool>,
    /// The scopes a `403 insufficient_scope` named while discovering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ProbeReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeStatus {
    Connected,
    AuthRequired,
    Failed,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeReason {
    Timeout,
    Unavailable,
    Response,
    Cleanup,
}

impl McpProbe {
    fn base(status: ProbeStatus, latency: std::time::Duration) -> Self {
        Self {
            status,
            latency_ms: latency.as_millis() as u64,
            tools: None,
            expired: None,
            missing_scopes: None,
            reason: None,
            message: None,
        }
    }
    fn connected(tools: Vec<server::McpTool>, latency: std::time::Duration) -> Self {
        Self {
            tools: Some(tools),
            ..Self::base(ProbeStatus::Connected, latency)
        }
    }
    fn failed(
        reason: ProbeReason,
        error: server::McpServerError,
        latency: std::time::Duration,
    ) -> Self {
        Self {
            reason: Some(reason),
            message: Some(McpError::Server(error).to_string()),
            ..Self::base(ProbeStatus::Failed, latency)
        }
    }
    /// The server answered, and its answer ends the probe.
    fn ended(error: server::McpServerError, latency: std::time::Duration) -> Self {
        match error {
            server::McpServerError::AuthRequired { expired } => Self {
                expired: Some(expired),
                ..Self::base(ProbeStatus::AuthRequired, latency)
            },
            server::McpServerError::InsufficientScope(scopes) => Self {
                missing_scopes: Some(scopes.split_whitespace().map(str::to_owned).collect()),
                ..Self::base(ProbeStatus::AuthRequired, latency)
            },
            server::McpServerError::Timeout => Self::failed(ProbeReason::Timeout, error, latency),
            server::McpServerError::InvalidResponse(_) => {
                Self::failed(ProbeReason::Response, error, latency)
            }
            server::McpServerError::Cleanup => Self::failed(ProbeReason::Cleanup, error, latency),
            _ => Self::failed(ProbeReason::Unavailable, error, latency),
        }
    }
}

/// Source CAS tokens are distinct from connection and tool-source versions.
/// Sources are read independently, not in a cross-source transaction.
pub struct McpCatalog {
    pub user_revision: String,
    pub project_revision: Option<String>,
    pub servers: BTreeMap<String, McpServerEntry>,
}

pub enum McpChange {
    Put(McpServerSettings),
    Remove,
    SetEnabled(bool),
}

/// A lease on one connection generation. Tool handlers retain this lease,
/// not a lookup into the directory's newest generation.
pub struct McpConnection {
    service: Arc<McpServer>,
    released: Option<tokio::sync::watch::Sender<()>>,
    withdrawn: Arc<AtomicBool>,
}
impl McpConnection {
    // Temporary children belong directly to their binding, outside the directory.
    fn temporary(service: Arc<McpServer>) -> Self {
        Self {
            service,
            released: None,
            withdrawn: Default::default(),
        }
    }
    pub fn state(&self) -> server::McpServerState {
        self.service.state()
    }
    /// The connection generation this lease names; a new one starts whenever
    /// the server establishes a connection.
    pub fn generation(&self) -> u64 {
        self.service.generation()
    }
    /// The tools of this generation's last complete discovery, including the
    /// ones no model sees.
    pub fn discovered(&self) -> Option<Vec<server::McpTool>> {
        self.service.discovered()
    }
    /// The generation was retired because its entry was disabled or removed,
    /// not because the connection failed.
    pub fn withdrawn(&self) -> bool {
        self.withdrawn.load(Ordering::SeqCst)
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<server::McpServerState> {
        self.service.subscribe()
    }
    pub fn tool_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.service.tool_changes()
    }
    /// Changes when the server's OAuth credential changes; `None` without OAuth.
    pub fn credential_changes(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.service.credential_changes()
    }
    pub async fn tools(&self, cancel: &CancellationToken) -> server::Result<Vec<server::McpTool>> {
        self.service.tools(cancel).await
    }
    pub async fn call(
        &self,
        name: String,
        arguments: dim_agent_types::JsonObject,
        cancel: &CancellationToken,
    ) -> server::Result<dim_agent_types::ToolResult> {
        self.service.call(name, arguments, cancel).await
    }
    pub async fn call_result(
        &self,
        name: String,
        arguments: dim_agent_types::JsonObject,
        cancel: &CancellationToken,
    ) -> std::result::Result<server::McpCall, server::ViewFailure> {
        self.service.call_result(name, arguments, cancel).await
    }
    pub async fn resource(
        &self,
        uri: String,
        cancel: &CancellationToken,
    ) -> std::result::Result<server::McpResource, server::ViewFailure> {
        self.service.resource(uri, cancel).await
    }
}
impl Drop for McpConnection {
    fn drop(&mut self) {
        if let Some(released) = &self.released {
            released.send_replace(());
        }
    }
}

pub struct McpService {
    settings: Arc<SettingsService>,
    /// Why the last registration of a plugin was refused, by plugin id.
    refused: Mutex<BTreeMap<String, String>>,
    publication: Arc<publication::Publication>,
    stopping: CancellationToken,
    gate: Mutex<()>,
    tasks: TaskTracker,
}
impl McpService {
    /// Acquire a generation in an explicit owner's loader subtree.
    pub async fn server(
        self: &Arc<Self>,
        instance_owner: McpOwner,
        project: Option<usize>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<Arc<McpConnection>> {
        let owner = self.clone();
        self.run(cancel, move |_, cancel| async move {
            owner
                .publication
                .acquire(instance_owner, project, id, &cancel)
                .await
        })
        .await
    }

    /// Explicitly close one source, including a source removed from settings.
    /// Old handles remain closed; a later server() creates a new generation.
    pub async fn disconnect(
        self: &Arc<Self>,
        source: impl Into<McpSource>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let source = source.into();
        let owner = self.clone();
        self.run(cancel, move |_, cancel| async move {
            owner.publication.disconnect(source, id, &cancel).await
        })
        .await
    }

    pub fn application(&self, source: impl Into<McpSource>) -> McpApplication {
        self.publication.application(source.into())
    }

    /// Adds or replaces the MCP document of `plugin` (a plugin id, PluginManager
    /// §3.7), whose stdio entries run in `dir` unless they name another `cwd`
    /// inside it; a `cwd` that is absolute, uses `..` or resolves outside `dir`
    /// through a symbolic link refuses the document. Its entries run as `<plugin>:<server>`. The registration is
    /// kept in `mcp-plugins.json` and survives Mcp's reload. A document that
    /// fails validation is refused: the plugin then has no entries, and
    /// [`Self::plugin_refusal`] reports why.
    pub async fn register_plugin(
        self: &Arc<Self>,
        plugin: String,
        dir: std::path::PathBuf,
        document: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let valid = serde_json::from_value::<McpSettings>(document)
            .map_err(|_| SettingsError::Invalid("invalid MCP configuration"))
            .and_then(|document| {
                let registration = McpPluginDocument {
                    dir,
                    mcp_servers: document.mcp_servers,
                };
                McpPluginServers {
                    plugins: BTreeMap::from([(plugin.clone(), registration.clone())]),
                    extra: BTreeMap::new(),
                }
                .validate()
                .and_then(|()| registration.confine())
                .map(|()| registration)
            });
        let result = match valid {
            Ok(registration) => {
                self.set_plugin(plugin.clone(), Some(registration), cancel)
                    .await
            }
            Err(error) => {
                let _ = self.set_plugin(plugin.clone(), None, cancel).await;
                Err(error.into())
            }
        };
        let mut refused = self.refused.lock().unwrap();
        match &result {
            Err(error) => refused.insert(plugin, error.to_string()),
            Ok(()) => refused.remove(&plugin),
        };
        result
    }

    /// Withdraws the MCP document of `plugin` and retires its entries in
    /// every Session; nothing when it registered none.
    pub async fn withdraw_plugin(
        self: &Arc<Self>,
        plugin: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.refused.lock().unwrap().remove(&plugin);
        self.set_plugin(plugin, None, cancel).await
    }

    /// Why Mcp refused the last document `plugin` registered.
    pub fn plugin_refusal(&self, plugin: &str) -> Option<String> {
        self.refused.lock().unwrap().get(plugin).cloned()
    }

    /// The plugin ids whose MCP documents Mcp holds.
    pub async fn plugins(&self, cancel: &CancellationToken) -> Result<Vec<String>> {
        self.run(cancel, move |settings, cancel| async move {
            Ok(settings
                .read::<McpPluginServers>(SettingsScope::User, &cancel)
                .await?
                .document
                .plugins
                .into_keys()
                .collect())
        })
        .await
    }

    async fn set_plugin(
        self: &Arc<Self>,
        plugin: String,
        registration: Option<McpPluginDocument>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let changed = self
            .run(cancel, move |settings, cancel| async move {
                loop {
                    let current = settings
                        .read::<McpPluginServers>(SettingsScope::User, &cancel)
                        .await?;
                    let mut document = current.document;
                    if document.plugins.get(&plugin) == registration.as_ref() {
                        return Ok(false);
                    }
                    match &registration {
                        Some(registration) => {
                            document
                                .plugins
                                .insert(plugin.clone(), registration.clone());
                        }
                        None => {
                            document.plugins.remove(&plugin);
                        }
                    }
                    match settings
                        .update(SettingsScope::User, &current.revision, document, &cancel)
                        .await
                    {
                        Ok(_) => return Ok(true),
                        Err(SettingsError::Conflict) => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
            })
            .await?;
        if changed {
            let publication = self.publication.clone();
            let _ = self
                .run(cancel, move |_, cancel| async move {
                    publication.settle(&cancel).await
                })
                .await;
        }
        Ok(())
    }

    /// Project ids are host-authorized FileStorage indices, never client paths.
    /// Project entries shadow user entries even when disabled.
    pub async fn catalog(
        &self,
        project: Option<usize>,
        cancel: &CancellationToken,
    ) -> Result<McpCatalog> {
        self.publication.refresh();
        self.run(cancel, move |settings, cancel| async move {
            let mut catalog = McpCatalog {
                user_revision: String::new(),
                project_revision: None,
                servers: BTreeMap::new(),
            };
            let enablement = match project {
                Some(_) => settings
                    .read::<McpProjectEnablement>(SettingsScope::User, &cancel)
                    .await
                    .map(|snapshot| snapshot.document)
                    .unwrap_or_default(),
                None => McpProjectEnablement::default(),
            };
            for source in [McpSource::User, McpSource::Plugins]
                .into_iter()
                .chain(project.map(McpSource::Project))
            {
                let Some(scope) = source.scope() else {
                    let plugins = settings
                        .read::<McpPluginServers>(SettingsScope::User, &cancel)
                        .await?;
                    for (id, configuration) in plugins.document.entries() {
                        catalog.servers.insert(
                            id,
                            McpServerEntry {
                                source,
                                configuration,
                            },
                        );
                    }
                    continue;
                };
                let snapshot = settings.read::<McpSettings>(scope, &cancel).await?;
                match scope {
                    SettingsScope::User => catalog.user_revision = snapshot.revision,
                    SettingsScope::Project(_) => {
                        catalog.project_revision = Some(snapshot.revision);
                    }
                }
                let document = snapshot.document;
                for (id, configuration) in document.mcp_servers {
                    let configuration =
                        config::effective(&settings, scope, &id, configuration, &enablement)?;
                    catalog.servers.insert(
                        id,
                        McpServerEntry {
                            source,
                            configuration,
                        },
                    );
                }
            }
            Ok(catalog)
        })
        .await
    }

    /// One source's entry, with its effective configuration, whether or not
    /// another source shadows it.
    pub async fn entry(
        &self,
        source: McpSource,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<McpServerEntry> {
        self.run(cancel, move |settings, cancel| async move {
            let Some(scope) = source.scope() else {
                let configuration = settings
                    .read::<McpPluginServers>(SettingsScope::User, &cancel)
                    .await?
                    .document
                    .entries()
                    .find(|(entry, _)| *entry == id)
                    .map(|(_, configuration)| configuration)
                    .ok_or(McpError::UnknownServer)?;
                return Ok(McpServerEntry {
                    source,
                    configuration,
                });
            };
            let source = scope;
            let configuration = settings
                .read::<McpSettings>(source, &cancel)
                .await?
                .document
                .mcp_servers
                .remove(&id)
                .ok_or(McpError::UnknownServer)?;
            let enablement = enablement(&settings, source, &cancel).await;
            Ok(McpServerEntry {
                source: source.into(),
                configuration: config::effective(
                    &settings,
                    source,
                    &id,
                    configuration,
                    &enablement,
                )?,
            })
        })
        .await
    }

    /// One source's entry as that source declares it, for reading and patching.
    pub async fn editable(
        &self,
        source: McpSource,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<McpEditableEntry> {
        if id.trim().is_empty() {
            return Err(McpError::InvalidId);
        }
        self.run(cancel, move |settings, cancel| async move {
            let Some(scope) = source.scope() else {
                let configuration = settings
                    .read::<McpPluginServers>(SettingsScope::User, &cancel)
                    .await?
                    .document
                    .declared(&id)
                    .ok_or(McpError::UnknownServer)?;
                return Ok(McpEditableEntry {
                    enabled: configuration.enabled(),
                    configuration,
                    revision: None,
                });
            };
            let mut snapshot = settings.read::<McpSettings>(scope, &cancel).await?;
            let configuration = snapshot
                .document
                .mcp_servers
                .remove(&id)
                .ok_or(McpError::UnknownServer)?;
            let enabled = match scope {
                SettingsScope::User => configuration.enabled(),
                SettingsScope::Project(_) => {
                    let enablement = enablement(&settings, scope, &cancel).await;
                    let workspace = config::workspace(&settings, scope)?;
                    enablement.contains(&workspace, &id)
                }
            };
            Ok(McpEditableEntry {
                configuration,
                revision: Some(snapshot.revision),
                enabled,
            })
        })
        .await
    }

    /// The configuration a Session runs, from the declared one a client reads
    /// and patches.
    pub async fn effective(
        &self,
        scope: SettingsScope,
        id: String,
        configuration: McpServerSettings,
        cancel: &CancellationToken,
    ) -> Result<McpServerSettings> {
        self.run(cancel, move |settings, cancel| async move {
            let enablement = enablement(&settings, scope, &cancel).await;
            Ok(config::effective(
                &settings,
                scope,
                &id,
                configuration,
                &enablement,
            )?)
        })
        .await
    }

    async fn test(
        &self,
        ctx: Ctx,
        id: String,
        configuration: McpServerSettings,
        cancel: &CancellationToken,
    ) -> Result<McpProbe> {
        self.run(cancel, move |_, cancel| async move {
            let key = Key::<McpServer>::dynamic(format!("mcp-test:{}", uuid::Uuid::new_v4()))
                .instance(ctx.instance());
            let plugin = server::McpServerPlugin::new(&id, configuration, key.clone())?;
            let started = std::time::Instant::now();
            let view = ctx.plugin(plugin);
            let operation = async {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(McpError::Cancelled),
                    loaded = &view => loaded.map_err(|_| McpError::Unavailable)?,
                }
                let service = ctx.get_at(&key).ok_or(McpError::Unavailable)?;
                service.tools(&cancel).await.map_err(McpError::from)
            };
            let outcome = tokio::time::timeout(PROBE_DEADLINE, operation).await;
            let latency = started.elapsed();
            let probe = match outcome {
                Ok(Ok(tools)) => Ok(McpProbe::connected(tools, latency)),
                Ok(Err(McpError::Server(error))) => Ok(McpProbe::ended(error, latency)),
                Ok(Err(error)) => Err(error),
                Err(_deadline) => Ok(McpProbe::failed(
                    ProbeReason::Timeout,
                    server::McpServerError::Timeout,
                    latency,
                )),
            };
            if view.shutdown().await.is_err() || view.state().error.is_some() {
                return Ok(McpProbe::failed(
                    ProbeReason::Cleanup,
                    server::McpServerError::Cleanup,
                    latency,
                ));
            }
            probe
        })
        .await
    }

    /// Mutate exactly one persisted source, not the merged effective view.
    /// Concurrent edits fail CAS rather than overwriting another server.
    /// Returns the revision of `source` after the change.
    ///
    /// A project entry's enablement is the user's record (ADR 003 D3), never
    /// project files. Enabling checks `expected` against the project source and
    /// that the entry exists there. Disabling or removing first withdraws the
    /// record without depending on the project file, so an entry the user
    /// turned off stays off even if the file is unreadable. Disabling or
    /// removing returns once the publisher has retired the entry's generations
    /// in every Session.
    pub async fn update(
        &self,
        source: SettingsScope,
        expected: String,
        id: String,
        change: McpChange,
        cancel: &CancellationToken,
    ) -> Result<String> {
        if id.trim().is_empty() {
            return Err(McpError::InvalidId);
        }
        let withdraws = matches!(change, McpChange::SetEnabled(false) | McpChange::Remove);
        let publication = self.publication.clone();
        let revision = self
            .run(cancel, move |settings, cancel| async move {
                write(&settings, source, expected, id, change, &cancel).await
            })
            .await?;
        if withdraws {
            let _ = self
                .run(cancel, move |_, cancel| async move {
                    publication.settle(&cancel).await
                })
                .await;
        }
        Ok(revision)
    }

    async fn run<T, F, Fut>(&self, cancel: &CancellationToken, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<SettingsService>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let task = {
            let _guard = self.gate.lock().unwrap();
            if self.stopping.is_cancelled() {
                return Err(McpError::Closed);
            }
            if cancel.is_cancelled() {
                return Err(McpError::Cancelled);
            }
            let stopping = self.stopping.clone();
            let cancel = cancel.child_token();
            let settings = self.settings.clone();
            self.tasks.spawn(async move {
                let operation = operation(settings, cancel.clone());
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
        task.await?
    }
}

/// The user's enablement of project entries; empty for the user source, whose
/// entries carry their own field.
async fn enablement(
    settings: &SettingsService,
    scope: SettingsScope,
    cancel: &CancellationToken,
) -> McpProjectEnablement {
    match scope {
        SettingsScope::User => McpProjectEnablement::default(),
        SettingsScope::Project(_) => settings
            .read::<McpProjectEnablement>(SettingsScope::User, cancel)
            .await
            .map(|snapshot| snapshot.document)
            .unwrap_or_default(),
    }
}

/// A probe gives one disposable connection this long to initialize and list
/// every tool page.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Writes one change to its source and returns the source's revision.
async fn write(
    settings: &Arc<SettingsService>,
    source: SettingsScope,
    expected: String,
    id: String,
    change: McpChange,
    cancel: &CancellationToken,
) -> Result<String> {
    let project = matches!(source, SettingsScope::Project(_));
    if project && matches!(change, McpChange::SetEnabled(false) | McpChange::Remove) {
        set_project_enabled(settings, source, &id, false, cancel).await?;
        if matches!(change, McpChange::SetEnabled(false)) {
            return Ok(settings
                .read::<McpSettings>(source, cancel)
                .await
                .map_or(expected, |snapshot| snapshot.revision));
        }
    }
    let snapshot = settings.read::<McpSettings>(source, cancel).await?;
    if snapshot.revision != expected {
        return Err(SettingsError::Conflict.into());
    }
    let mut document = snapshot.document;
    if project && matches!(change, McpChange::SetEnabled(true)) {
        if !document.mcp_servers.contains_key(&id) {
            return Err(McpError::UnknownServer);
        }
        set_project_enabled(settings, source, &id, true, cancel).await?;
        return Ok(snapshot.revision);
    }
    match change {
        McpChange::Put(configuration) => {
            document.mcp_servers.insert(id, configuration);
        }
        McpChange::Remove => {
            document
                .mcp_servers
                .remove(&id)
                .ok_or(McpError::UnknownServer)?;
        }
        McpChange::SetEnabled(value) => {
            let configuration = document
                .mcp_servers
                .get_mut(&id)
                .ok_or(McpError::UnknownServer)?;
            let (McpServerSettings::Stdio { enabled, .. }
            | McpServerSettings::Remote { enabled, .. }) = configuration;
            *enabled = Some(value);
        }
    }
    Ok(settings
        .update(source, &expected, document, cancel)
        .await?
        .revision)
}

/// Records the user's enablement of one project entry. Toggles of different
/// entries commute, so a lost race on the shared record is retried rather
/// than reported as a conflict the caller cannot observe.
async fn set_project_enabled(
    settings: &Arc<SettingsService>,
    source: SettingsScope,
    id: &str,
    enabled: bool,
    cancel: &CancellationToken,
) -> Result<()> {
    let workspace = config::workspace(settings, source)?;
    loop {
        let current = settings
            .read::<McpProjectEnablement>(SettingsScope::User, cancel)
            .await?;
        let mut record = current.document;
        if !record.set(&workspace, id, enabled) {
            return Ok(());
        }
        match settings
            .update(SettingsScope::User, &current.revision, record, cancel)
            .await
        {
            Ok(_) => return Ok(()),
            Err(SettingsError::Conflict) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

/// The plugin: the directory, its management methods, the OAuth credential
/// authority, whose store `mcp-oauth.json` lives in the home root, and the
/// Session part `McpSession`, which it registers.
pub struct McpPlugin {
    home_root: usize,
    settings: Key<SettingsService>,
    publication: Key<publication::Publication>,
    files: Key<FileStorage>,
    methods: Key<crate::methods::ManagementMethods>,
    parts: Key<dim_agent_runtime::PartRegistry>,
}
impl McpPlugin {
    pub fn new(names: &Names<'_>, home_root: usize) -> rutis::Result<Self> {
        Ok(Self {
            home_root,
            settings: names.key()?,
            publication: names.key()?,
            files: names.key()?,
            methods: names.key()?,
            parts: names.key()?,
        })
    }
}
impl DynPlugin for McpPlugin {
    fn name(&self) -> &str {
        "Mcp"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.settings.erase(),
            self.publication.erase(),
            self.files.erase(),
            self.methods.erase(),
            self.parts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let publication = ctx.require_at(&self.publication)?;
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            let parts = ctx.require_at(&self.parts)?;
            settings.register::<McpSettings>(ctx)?;
            settings.register::<McpProjectEnablement>(ctx)?;
            settings.register::<McpPluginServers>(ctx)?;
            let auth = McpAuth::new(files, self.home_root).map_err(ErrorKind::PluginFailed)?;
            let service = Arc::new(McpService {
                settings,
                refused: Mutex::default(),
                publication,
                stopping: ctx.cancellation_token(),
                gate: Mutex::new(()),
                tasks: TaskTracker::new(),
            });
            let (owner, closing) = (service.clone(), auth.clone());
            ctx.effect(move || async move {
                {
                    let _guard = owner.gate.lock().unwrap();
                    owner.tasks.close();
                }
                owner.tasks.wait().await;
                closing.close().await;
                Ok(())
            })?;
            management::provide(
                ctx,
                &methods,
                service.clone(),
                service.settings.clone(),
                auth.clone(),
            )?;
            ctx.provide_as::<McpAuth>(auth)?;
            ctx.provide_as::<McpService>(service)?;
            parts.register(
                ctx,
                dim_agent_runtime::PartScope::Session,
                "McpSession",
                |mount| Ok(Some(Box::new(McpSessionPlugin::new(mount.names)?))),
            )?;
            parts.register(
                ctx,
                dim_agent_runtime::PartScope::Session,
                "TemporaryMcp",
                |mount| Ok(Some(Box::new(temporary::TemporaryMcp::new(mount.names)?))),
            )?;
            Ok(Effect::Done)
        })
    }
}
