//! MCP configuration directory. Connections belong to McpServer, not Settings.
mod auth;
pub mod config;
mod discovery;
mod import;
pub(crate) mod management;
mod publication;
#[cfg(feature = "testing")]
pub use auth::testing;
pub use auth::{McpAuth, McpAuthError};
#[cfg(feature = "testing")]
pub use import::testing as import_testing;
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
    McpEntryError, McpPluginDocument, McpPluginServers, McpProjectEnablement, McpSecretEntry,
    McpSecrets, McpServerSettings, McpSettings,
};
use dim_agent_capabilities::file_storage::FileStorage;
use dim_agent_capabilities::host::CredentialVariablesHost;
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use server::{McpServer, McpServerError};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    #[error("{0}")]
    InvalidEntry(#[from] McpEntryError),
    #[error("MCP destination source already has this entry")]
    MoveConflict,
    #[error("MCP move is partial")]
    MovePartial { into_project: bool },
    #[error("MCP secret fields are stored in user scope")]
    CredentialsAreUserScoped,
    #[error("MCP withheld record has no project entry")]
    StaleSecretRecord,
    #[error("MCP import of {id}: {reason}")]
    ImportConflict { id: String, reason: String },
    #[error("MCP import is partial")]
    ImportPartial,
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

/// The sources in ascending precedence. The last source that holds a decoded
/// entry for an id is that id's server; a broken entry holds none, so it
/// shadows nothing.
pub(crate) fn sources_in_precedence(project: Option<usize>) -> impl Iterator<Item = McpSource> {
    [McpSource::User, McpSource::Plugins]
        .into_iter()
        .chain(project.map(McpSource::Project))
}

/// Server-side configuration, including secrets. Not a wire or logging type.
/// For a project entry the configuration is the effective view: the overlay
/// is merged, the workspace `cwd` is joined and the enablement comes from the
/// user's record (`config::effective`). Environment references stay as the
/// user wrote them; only the resolved entry's expanded copy is what a
/// connection runs.
#[derive(Clone)]
pub struct McpServerEntry {
    pub source: McpSource,
    pub configuration: McpServerSettings,
    /// Names the effective view references, as written.
    pub referenced_variables: BTreeSet<String>,
    /// Referenced names that are unset and have no default.
    pub unset_variables: BTreeSet<String>,
    /// The expanded credential key of an entry that can use OAuth, carried
    /// beside the entry so a shadowing entry never inherits the key of the
    /// entry it shadows.
    pub(crate) credential: Option<auth::CredentialKey>,
}
impl McpServerEntry {
    pub fn enabled(&self) -> bool {
        self.configuration.enabled()
    }
}

/// One source entry as a path that contacts the server sees it: the effective
/// view, the user overlay that supplied its withheld members, and the
/// configuration with environment references expanded.
pub struct McpResolvedEntry {
    pub entry: McpServerEntry,
    pub secret: Option<McpSecretEntry>,
    pub expanded: McpServerSettings,
}

/// The environment a connection expands its references from.
pub(crate) fn daemon_environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// One source's entry as that source declares it, for editing: the declared
/// configuration, the source revision a write takes (absent for a plugin
/// entry) and the enablement the entry runs with.
pub struct McpEditableEntry {
    pub configuration: McpServerSettings,
    /// Names the declared configuration references, as written.
    pub referenced_variables: BTreeSet<String>,
    /// Referenced names that are unset and have no default.
    pub unset_variables: BTreeSet<String>,
    pub revision: Option<String>,
    pub enabled: bool,
    /// The entry has a user-scope secret overlay; the withheld members are
    /// read-only in project scope.
    pub secrets_in_user_scope: bool,
    /// The overlay record, kept server-side for stripping a non-secret patch.
    pub secret: Option<McpSecretEntry>,
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
            message: Some(server::bounded_diagnostic(
                McpError::Server(error).to_string(),
            )),
            ..Self::base(ProbeStatus::Failed, latency)
        }
    }
    /// The URL still names references the daemon does not set, so it cannot
    /// address a server and the probe contacts nothing: it reports the names
    /// rather than a configuration error (D19).
    fn unresolved(names: &BTreeSet<String>) -> Self {
        let (noun, verb) = if names.len() == 1 {
            ("variable", "is")
        } else {
            ("variables", "are")
        };
        let names = names.iter().cloned().collect::<Vec<_>>().join(", ");
        Self {
            reason: Some(ProbeReason::Unavailable),
            message: Some(server::bounded_diagnostic(format!(
                "environment {noun} {names} {verb} not set"
            ))),
            ..Self::base(ProbeStatus::Failed, std::time::Duration::ZERO)
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
            server::McpServerError::Timeout { .. } => {
                Self::failed(ProbeReason::Timeout, error, latency)
            }
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
    /// Entries that did not decode, in the source that declares them.
    pub invalid: Vec<McpInvalidEntry>,
}

/// One entry the catalog could not decode, with the entry's own error.
pub struct McpInvalidEntry {
    pub id: String,
    pub source: McpSource,
    pub error: McpEntryError,
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
    /// The last start or transport failure; status only, never a model request.
    pub fn failure(&self) -> Option<String> {
        self.service.failure()
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

/// One source entry with every derived field: the effective view a read
/// shows, the overlay that supplied its withheld members, the expansion a
/// connection runs, and the credential key of that expansion. Every read and
/// every path that contacts a server uses this one order.
fn resolved_entry(
    settings: &SettingsService,
    source: McpSource,
    id: &str,
    configuration: McpServerSettings,
    secret: Option<&McpSecretEntry>,
    enablement: &McpProjectEnablement,
    protected: &BTreeSet<String>,
) -> Result<McpResolvedEntry> {
    let origin = match source {
        McpSource::User => config::Origin::User,
        McpSource::Plugins => config::Origin::Plugin,
        McpSource::Project(_) => config::Origin::Workspace { overlay: secret },
    };
    let expansion = config::connected(
        settings,
        configuration.clone(),
        config::Connection {
            scope: source.scope(),
            id,
            secret,
            origin,
            enablement,
            environment: &daemon_environment,
            protected,
        },
    )?;
    let configuration = config::viewed(
        settings,
        source.scope(),
        id,
        configuration,
        secret,
        enablement,
    )?;
    let credential = auth::credential_key(id, &expansion.configuration).map(|(key, _)| key);
    Ok(McpResolvedEntry {
        entry: McpServerEntry {
            source,
            configuration,
            referenced_variables: expansion.referenced,
            unset_variables: expansion.unset,
            credential,
        },
        secret: secret.cloned(),
        expanded: expansion.configuration,
    })
}

pub struct McpService {
    settings: Arc<SettingsService>,
    credentials: Arc<CredentialVariablesHost>,
    /// Why the last registration of a plugin was refused, by plugin id: the
    /// raw error, and its static reason when the cause has one
    /// (PluginManager A.2).
    refused: Mutex<BTreeMap<String, (String, Option<&'static str>)>>,
    publication: Arc<publication::Publication>,
    stopping: CancellationToken,
    gate: Mutex<()>,
    tasks: TaskTracker,
}
impl McpService {
    /// The credential variable names the daemon owns now.
    pub(crate) fn protected(&self) -> BTreeSet<String> {
        self.credentials.variables().variables
    }

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
        let valid = McpSettings::from_value(document).and_then(|mut document| {
            // A plugin registers its MCP content as one unit, and dim does
            // not manage a plugin entry on its own. Refuse the document
            // with the first broken entry's error rather than drop it.
            if let Some(error) = document.invalid_entries().values().next() {
                return Err(SettingsError::Document(error.to_string()));
            }
            // The wrapper owns its members: `dir` and the file's shape are
            // `McpPluginDocument`'s, and a plugin document's own top-level
            // members are not part of the registration, as the typed map
            // dropped them. Keeping one would write a second `dir` beside the
            // wrapper's and make the whole file unreadable for every plugin.
            document.extra.clear();
            let registration = McpPluginDocument {
                dir,
                servers: document,
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
            Err(McpError::Settings(error)) => {
                refused.insert(plugin, (error.to_string(), error.reason()));
            }
            Err(error) => {
                refused.insert(plugin, (error.to_string(), None));
            }
            Ok(()) => {
                refused.remove(&plugin);
            }
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

    /// Why Mcp refused the last document `plugin` registered: the raw error,
    /// and its static reason when the cause has one (PluginManager A.2).
    /// Both under one lock, so a registration between two reads cannot pair
    /// one cause's error with another's reason.
    pub fn plugin_refusal(&self, plugin: &str) -> Option<(String, Option<&'static str>)> {
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
        let credentials = self.credentials.clone();
        self.run(cancel, move |settings, cancel| async move {
            let protected = credentials.variables().variables;
            let mut catalog = McpCatalog {
                user_revision: String::new(),
                project_revision: None,
                servers: BTreeMap::new(),
                invalid: Vec::new(),
            };
            let mut invalid: BTreeMap<(McpSource, String), McpEntryError> = BTreeMap::new();
            let enablement = match project {
                Some(_) => settings
                    .read::<McpProjectEnablement>(SettingsScope::User, &cancel)
                    .await
                    .map(|snapshot| snapshot.document)
                    .unwrap_or_default(),
                None => McpProjectEnablement::default(),
            };
            let secrets = match project {
                Some(_) => {
                    settings
                        .read::<McpSecrets>(SettingsScope::User, &cancel)
                        .await?
                        .document
                }
                None => McpSecrets::default(),
            };
            for source in sources_in_precedence(project) {
                let Some(scope) = source.scope() else {
                    let plugins = settings
                        .read::<McpPluginServers>(SettingsScope::User, &cancel)
                        .await?;
                    let (entries, broken) = plugins.document.into_entries();
                    for (id, error) in broken {
                        invalid.insert((source, id), error);
                    }
                    for (id, configuration) in entries {
                        let entry = resolved_entry(
                            &settings,
                            source,
                            &id,
                            configuration,
                            None,
                            &enablement,
                            &protected,
                        )?
                        .entry;
                        catalog.servers.insert(id, entry);
                    }
                    continue;
                };
                let workspace = match scope {
                    SettingsScope::Project(_) => Some(config::workspace(&settings, scope)?),
                    SettingsScope::User => None,
                };
                let snapshot = settings.read::<McpSettings>(scope, &cancel).await?;
                match scope {
                    SettingsScope::User => catalog.user_revision = snapshot.revision,
                    SettingsScope::Project(_) => {
                        let overlays = workspace
                            .as_ref()
                            .map_or_else(BTreeMap::new, |workspace| secrets.overlays(workspace));
                        catalog.project_revision =
                            Some(source_revision(scope, &snapshot.revision, &overlays));
                    }
                }
                let document = snapshot.document;
                let (servers, invalid_entries) = document.into_parts();
                for (id, configuration) in servers {
                    let secret = workspace
                        .as_ref()
                        .and_then(|workspace| secrets.get(workspace, &id));
                    let entry = resolved_entry(
                        &settings,
                        source,
                        &id,
                        configuration,
                        secret,
                        &enablement,
                        &protected,
                    )?
                    .entry;
                    catalog.servers.insert(id, entry);
                }
                for (id, error) in invalid_entries {
                    invalid.insert((source, id.clone()), error);
                }
            }
            catalog.invalid = invalid
                .into_iter()
                .map(|((source, id), error)| McpInvalidEntry { id, source, error })
                .collect();
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
        Ok(self.resolved(source, id, cancel).await?.entry)
    }

    /// One source's entry for a path that contacts the server: the effective
    /// view, the overlay that supplied its withheld members, and the same
    /// expanded configuration the publisher gives a connection.
    pub async fn resolved(
        &self,
        source: McpSource,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<McpResolvedEntry> {
        let credentials = self.credentials.clone();
        self.run(cancel, move |settings, cancel| async move {
            let protected = credentials.variables().variables;
            let Some(scope) = source.scope() else {
                let (mut entries, mut invalid) = settings
                    .read::<McpPluginServers>(SettingsScope::User, &cancel)
                    .await?
                    .document
                    .into_entries();
                let Some(configuration) = entries.remove(&id) else {
                    return match invalid.remove(&id) {
                        Some(error) => Err(McpError::InvalidEntry(error)),
                        None => Err(McpError::UnknownServer),
                    };
                };
                return resolved_entry(
                    &settings,
                    source,
                    &id,
                    configuration,
                    None,
                    &McpProjectEnablement::default(),
                    &protected,
                );
            };
            let source = scope;
            let snapshot = settings.read::<McpSettings>(source, &cancel).await?;
            let document = snapshot.document;
            let Some(configuration) = document.get(&id).cloned() else {
                return match document.invalid_error(&id) {
                    Some(error) => Err(McpError::InvalidEntry(error.clone())),
                    None => Err(McpError::UnknownServer),
                };
            };
            let secret = secret(&settings, source, &id, &cancel).await?;
            let enablement = enablement(&settings, source, &cancel).await;
            resolved_entry(
                &settings,
                source.into(),
                &id,
                configuration,
                secret.as_ref(),
                &enablement,
                &protected,
            )
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
        let protected = self.protected();
        self.run(cancel, move |settings, cancel| async move {
            let Some(scope) = source.scope() else {
                let configuration = match settings
                    .read::<McpPluginServers>(SettingsScope::User, &cancel)
                    .await?
                    .document
                    .declared(&id)
                {
                    Ok(Some(configuration)) => configuration,
                    Ok(None) => return Err(McpError::UnknownServer),
                    Err(error) => return Err(McpError::InvalidEntry(error)),
                };
                let expansion = config::connected(
                    &settings,
                    configuration.clone(),
                    config::Connection {
                        scope: None,
                        id: &id,
                        secret: None,
                        origin: config::Origin::Plugin,
                        enablement: &McpProjectEnablement::default(),
                        environment: &daemon_environment,
                        protected: &protected,
                    },
                )?;
                return Ok(McpEditableEntry {
                    enabled: configuration.enabled(),
                    configuration,
                    referenced_variables: expansion.referenced,
                    unset_variables: expansion.unset,
                    revision: None,
                    secrets_in_user_scope: false,
                    secret: None,
                });
            };
            let snapshot = settings.read::<McpSettings>(scope, &cancel).await?;
            let Some(mut configuration) = snapshot.document.get(&id).cloned() else {
                return match snapshot.document.invalid_error(&id) {
                    Some(error) => Err(McpError::InvalidEntry(error.clone())),
                    None => Err(McpError::UnknownServer),
                };
            };
            let overlays = overlays(&settings, scope, &cancel).await?;
            let secret = overlays.get(&id).cloned();
            if let Some(secret) = &secret {
                config::merge_secret(&mut configuration, secret)?;
            }
            let origin = match scope {
                SettingsScope::User => config::Origin::User,
                SettingsScope::Project(_) => config::Origin::Workspace {
                    overlay: secret.as_ref(),
                },
            };
            let expansion = config::connected(
                &settings,
                configuration.clone(),
                config::Connection {
                    scope: None,
                    id: &id,
                    secret: None,
                    origin,
                    enablement: &McpProjectEnablement::default(),
                    environment: &daemon_environment,
                    protected: &protected,
                },
            )?;
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
                referenced_variables: expansion.referenced,
                unset_variables: expansion.unset,
                revision: Some(source_revision(scope, &snapshot.revision, &overlays)),
                enabled,
                secrets_in_user_scope: secret.is_some(),
                secret,
            })
        })
        .await
    }

    /// One project entry's withheld record, including a record whose project
    /// entry no longer exists.
    pub async fn overlay(
        &self,
        source: SettingsScope,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<Option<McpSecretEntry>> {
        if id.trim().is_empty() {
            return Err(McpError::InvalidId);
        }
        self.run(cancel, move |settings, cancel| async move {
            secret(&settings, source, &id, &cancel).await
        })
        .await
    }

    /// Deletes a project entry's withheld record only while it still carries
    /// the captured generation. A replacement entry for the same id writes a
    /// new generation, even with equal values, and that record is kept.
    pub async fn clear_secret_if(
        &self,
        source: SettingsScope,
        id: String,
        generation: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if id.trim().is_empty() {
            return Err(McpError::InvalidId);
        }
        self.run(cancel, move |settings, cancel| async move {
            let workspace = config::workspace(&settings, source)?;
            remove_secret_if(&settings, &workspace, &id, &generation, &cancel).await
        })
        .await
    }

    /// The configuration a connection runs, from the declared one a client
    /// reads and patches. The caller merges the stored overlay exactly once
    /// into that configuration; `overlay` answers only which members are the
    /// user's own text for the expansion guard.
    pub(crate) async fn connected(
        &self,
        scope: SettingsScope,
        id: String,
        configuration: McpServerSettings,
        overlay: Option<McpSecretEntry>,
        cancel: &CancellationToken,
    ) -> Result<config::Expanded> {
        let protected = self.protected();
        self.run(cancel, move |settings, cancel| async move {
            let enablement = enablement(&settings, scope, &cancel).await;
            let origin = match scope {
                SettingsScope::User => config::Origin::User,
                SettingsScope::Project(_) => config::Origin::Workspace {
                    overlay: overlay.as_ref(),
                },
            };
            Ok(config::connected(
                &settings,
                configuration,
                config::Connection {
                    scope: Some(scope),
                    id: &id,
                    secret: None,
                    origin,
                    enablement: &enablement,
                    environment: &daemon_environment,
                    protected: &protected,
                },
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
        if let McpServerSettings::Remote { url, .. } = &configuration {
            let unresolved = config::unresolved(url);
            if !unresolved.is_empty() {
                return Ok(McpProbe::unresolved(&unresolved));
            }
        }
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
                    server::McpServerError::Timeout {
                        millis: PROBE_DEADLINE.as_millis() as u64,
                        origin: "the probe deadline",
                    },
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
    /// Returns the source's revision token after the change, the same token
    /// `mcp.catalog` reports for it; a disable whose document could not be
    /// read returns the caller's `expected` unchanged, since nothing was
    /// read to compute a newer one.
    ///
    /// A project entry's enablement is the user's record (ADR 003 D3), never
    /// project files. Enabling checks `expected` against the project source and
    /// that the entry exists there. A disable withdraws the record without
    /// depending on the project file, so an entry the user turned off stays off
    /// even if the file is unreadable. A remove needs a readable source, and a
    /// remove whose `expectedVersion` is stale when it arrives writes nothing.
    /// A source change that lands while the remove runs, after that check, can
    /// still leave the record withdrawn: the entry is then off, which fails
    /// closed, and the reply is the conflict. A remove deletes the entry's
    /// withheld record only while it still carries the generation captured
    /// before the removal, so a replacement entry for the same id keeps its
    /// own. Disabling or removing returns once the publisher has retired the
    /// entry's generations in every Session.
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
        let cleanup_id = id.clone();
        let supplied = expected.clone();
        let written = self
            .run(cancel, move |settings, cancel| async move {
                write(&settings, source, expected, id, change, &cancel).await
            })
            .await?;
        if let Some(captured) = written.captured {
            self.clear_secret_if(source, cleanup_id, captured, cancel)
                .await?;
        }
        if written.entry_absent {
            return Err(McpError::UnknownServer);
        }
        if withdraws {
            let _ = self
                .run(cancel, move |_, cancel| async move {
                    publication.settle(&cancel).await
                })
                .await;
        }
        // The source's token names the document and, for a project source, the
        // records, which the removal's cleanup may have deleted. When the
        // document could not be read, the caller's token is the best answer.
        let revision = match &written.revision {
            Some(document) => read_source_revision(&self.settings, source, document, cancel).await,
            None => supplied,
        };
        Ok(revision)
    }

    /// Previews importing one `mcpServers` document into one source. The
    /// source is read with its revision; nothing is written or executed.
    pub(crate) async fn preview_import(
        &self,
        source: SettingsScope,
        document: McpSettings,
        cancel: &CancellationToken,
    ) -> Result<import::ImportPreview> {
        self.run(cancel, move |settings, cancel| async move {
            let snapshot = settings.read::<McpSettings>(source, &cancel).await?;
            let overlays = overlays(&settings, source, &cancel).await?;
            Ok(import::preview(
                &document,
                &snapshot.document,
                matches!(source, SettingsScope::Project(_)),
                &overlays,
                &source_revision(source, &snapshot.revision, &overlays),
            ))
        })
        .await
    }

    /// Imports the entries a selection accepts from one `mcpServers`
    /// document. The source is read once at `expected`; every accepted entry
    /// is written into that snapshot and committed with one Settings update,
    /// so one revision change covers the whole import.
    pub(crate) async fn import_entries(
        &self,
        source: SettingsScope,
        expected: String,
        document: McpSettings,
        resolutions: BTreeMap<String, import::ImportResolution>,
        cancel: &CancellationToken,
    ) -> Result<String> {
        self.run(cancel, move |settings, cancel| async move {
            write_import(&settings, source, expected, document, resolutions, &cancel).await
        })
        .await
    }

    /// Move one entry between the user and project sources. The destination is
    /// written first and the source removed last; a failure after the first
    /// destination write is reported as `MovePartial`, not success.
    pub async fn move_entry(
        &self,
        from: McpSource,
        to: McpSource,
        id: String,
        expected_from: String,
        expected_to: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if id.trim().is_empty() {
            return Err(McpError::InvalidId);
        }
        let from_scope = from.scope().ok_or_else(|| {
            McpError::Settings(SettingsError::Invalid("plugin entries are not movable"))
        })?;
        let to_scope = to.scope().ok_or_else(|| {
            McpError::Settings(SettingsError::Invalid("plugin entries are not movable"))
        })?;
        if from_scope == to_scope {
            return Err(McpError::Settings(SettingsError::Invalid(
                "source and destination must differ",
            )));
        }
        let into_project = matches!(to_scope, SettingsScope::Project(_));
        self.run(cancel, move |settings, cancel| async move {
            let from_snapshot = settings.read::<McpSettings>(from_scope, &cancel).await?;
            let to_snapshot = settings.read::<McpSettings>(to_scope, &cancel).await?;
            let from_overlays = overlays(&settings, from_scope, &cancel).await?;
            let to_overlays = overlays(&settings, to_scope, &cancel).await?;
            if source_revision(from_scope, &from_snapshot.revision, &from_overlays) != expected_from
                || source_revision(to_scope, &to_snapshot.revision, &to_overlays) != expected_to
            {
                return Err(SettingsError::Conflict.into());
            }
            if to_snapshot.document.contains(&id)
                || to_snapshot.document.invalid_error(&id).is_some()
            {
                return Err(McpError::MoveConflict);
            }
            let Some(mut entry) = from_snapshot.document.get(&id).cloned() else {
                return match from_snapshot.document.invalid_error(&id) {
                    Some(error) => Err(McpError::InvalidEntry(error.clone())),
                    None => Err(McpError::UnknownServer),
                };
            };
            let from_secret = from_overlays.get(&id);
            if let Some(secret) = from_secret {
                config::merge_secret(&mut entry, secret)?;
            }
            let enablement = enablement(&settings, from_scope, &cancel).await;
            let mut entry = config::effective(&settings, from_scope, &id, entry, &enablement)?;
            let mut document = McpSettings::default();
            document.insert(id.clone(), entry.clone());
            document.validate()?;

            match to_scope {
                SettingsScope::Project(_) => {
                    let workspace = config::workspace(&settings, to_scope)?;
                    let (skeleton, to_secret) = config::split_entry(&entry);
                    set_project_enabled(&settings, to_scope, &id, false, &cancel).await?;
                    let mut destination = to_snapshot.document;
                    destination.insert(id.clone(), skeleton);
                    settings
                        .update(to_scope, &to_snapshot.revision, destination, &cancel)
                        .await?;
                    if let Some(secret) = to_secret {
                        if let Err(error) =
                            write_secret(&settings, &workspace, &id, secret, &cancel).await
                        {
                            return Err(partial(error, into_project));
                        }
                    } else if let Err(error) =
                        remove_secret(&settings, &workspace, &id, &cancel).await
                    {
                        return Err(partial(error, into_project));
                    }
                    let mut source = from_snapshot.document;
                    source.remove(&id);
                    settings
                        .update(from_scope, &from_snapshot.revision, source, &cancel)
                        .await
                        .map_err(|error| partial(error.into(), into_project))?;
                }
                SettingsScope::User => {
                    let workspace = config::workspace(&settings, from_scope)?;
                    let enabled = enablement.contains(&workspace, &id);
                    let (McpServerSettings::Stdio { enabled: field, .. }
                    | McpServerSettings::Remote { enabled: field, .. }) = &mut entry;
                    *field = Some(enabled);
                    let mut destination = to_snapshot.document;
                    destination.insert(id.clone(), entry);
                    settings
                        .update(to_scope, &to_snapshot.revision, destination, &cancel)
                        .await?;
                    if let Err(error) =
                        set_project_enabled(&settings, from_scope, &id, false, &cancel).await
                    {
                        return Err(partial(error, into_project));
                    }
                    if from_secret.is_some()
                        && let Err(error) = remove_secret(&settings, &workspace, &id, &cancel).await
                    {
                        return Err(partial(error, into_project));
                    }
                    let mut source = from_snapshot.document;
                    source.remove(&id);
                    settings
                        .update(from_scope, &from_snapshot.revision, source, &cancel)
                        .await
                        .map_err(|error| partial(error.into(), into_project))?;
                }
            }
            Ok(())
        })
        .await
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

/// One project entry's user-scope secret overlay, if it has one.
async fn secret(
    settings: &SettingsService,
    scope: SettingsScope,
    id: &str,
    cancel: &CancellationToken,
) -> Result<Option<McpSecretEntry>> {
    let SettingsScope::Project(_) = scope else {
        return Ok(None);
    };
    let workspace = config::workspace(settings, scope)?;
    let snapshot = settings
        .read::<McpSecrets>(SettingsScope::User, cancel)
        .await?;
    Ok(snapshot.document.get(&workspace, id).cloned())
}

/// A probe gives one disposable connection this long to initialize and list
/// every tool page.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// One source mutation's committed result: the source document's revision,
/// absent when a disable could not read the file, the generation of the
/// withheld record captured before a project entry removal, and whether the
/// entry was already absent.
struct WriteOutcome {
    revision: Option<String>,
    captured: Option<String>,
    entry_absent: bool,
}

/// Writes one change to its source and returns the source's revision and the
/// removal's captured record generation.
async fn write(
    settings: &Arc<SettingsService>,
    source: SettingsScope,
    expected: String,
    id: String,
    change: McpChange,
    cancel: &CancellationToken,
) -> Result<WriteOutcome> {
    let project = matches!(source, SettingsScope::Project(_));
    if project && matches!(change, McpChange::SetEnabled(false)) {
        // A disable does not need the project file, so an unreadable file
        // still withdraws the record. A readable file names a broken entry,
        // whose disable is refused.
        let snapshot = settings.read::<McpSettings>(source, cancel).await;
        if let Ok(snapshot) = &snapshot
            && let Some(error) = snapshot.document.invalid_error(&id)
        {
            return Err(McpError::InvalidEntry(error.clone()));
        }
        set_project_enabled(settings, source, &id, false, cancel).await?;
        return Ok(WriteOutcome {
            revision: snapshot.map(|snapshot| snapshot.revision).ok(),
            captured: None,
            entry_absent: false,
        });
    }
    let snapshot = settings.read::<McpSettings>(source, cancel).await?;
    let overlays = overlays(settings, source, cancel).await?;
    if source_revision(source, &snapshot.revision, &overlays) != expected {
        return Err(SettingsError::Conflict.into());
    }
    let mut document = snapshot.document;
    if project && matches!(change, McpChange::SetEnabled(true)) {
        if let Some(error) = document.invalid_error(&id) {
            return Err(McpError::InvalidEntry(error.clone()));
        }
        if !document.contains(&id) {
            return Err(McpError::UnknownServer);
        }
        set_project_enabled(settings, source, &id, true, cancel).await?;
        return Ok(WriteOutcome {
            revision: Some(snapshot.revision),
            captured: None,
            entry_absent: false,
        });
    }
    let removed = match change {
        McpChange::Put(configuration) => {
            let configuration = configuration
                .canonical(&id)
                .map_err(McpError::InvalidEntry)?;
            document.insert(id.clone(), configuration);
            None
        }
        McpChange::Remove => {
            let present = document.contains(&id) || document.invalid_error(&id).is_some();
            document.remove(&id);
            // The record goes before the source is rewritten, and only once
            // the revision matched, so a stale remove withdraws nothing.
            if project && present {
                set_project_enabled(settings, source, &id, false, cancel).await?;
            }
            Some(present)
        }
        McpChange::SetEnabled(value) => {
            let Some(configuration) = document.configure_mut(&id) else {
                return match document.invalid_error(&id) {
                    Some(error) => Err(McpError::InvalidEntry(error.clone())),
                    None => Err(McpError::UnknownServer),
                };
            };
            let (McpServerSettings::Stdio { enabled, .. }
            | McpServerSettings::Remote { enabled, .. }) = configuration;
            *enabled = Some(value);
            None
        }
    };
    let Some(present) = removed else {
        let written = settings
            .update(source, &snapshot.revision, document, cancel)
            .await?;
        return Ok(WriteOutcome {
            revision: Some(written.revision),
            captured: None,
            entry_absent: false,
        });
    };
    // Capture the record generation this removal owns before the document
    // commit, so the cleanup cannot delete a replacement entry's record.
    let captured = overlays.get(&id).map(|secret| secret.generation.clone());
    let revision = if present {
        settings
            .update(source, &snapshot.revision, document, cancel)
            .await?
            .revision
    } else {
        snapshot.revision.clone()
    };
    Ok(WriteOutcome {
        revision: Some(revision),
        captured,
        entry_absent: !present,
    })
}

/// Writes every entry a resolution accepts with one document revision check
/// and one Settings update. A request that accepts no entry writes nothing,
/// so re-importing unchanged entries leaves the source revision as it was.
///
/// A project import writes what a move into project writes (D25): the public
/// skeleton enters `.mcp.json` and the withheld part the workspace's
/// user-scope record. The enablement records are withdrawn first, so an
/// imported configuration never runs enabled; a compare-and-set that then
/// fails writes no entry but leaves an entry the import would have replaced
/// disabled, and `mcp.set_enabled` enables it again. The source update
/// commits next and the withheld records last; a record failure after the
/// source commit answers `ImportPartial`, and the source entries stay as
/// written.
async fn write_import(
    settings: &Arc<SettingsService>,
    source: SettingsScope,
    expected: String,
    document: McpSettings,
    resolutions: BTreeMap<String, import::ImportResolution>,
    cancel: &CancellationToken,
) -> Result<String> {
    let snapshot = settings.read::<McpSettings>(source, cancel).await?;
    let project = matches!(source, SettingsScope::Project(_));
    let overlays = overlays(settings, source, cancel).await?;
    if source_revision(source, &snapshot.revision, &overlays) != expected {
        return Err(SettingsError::Conflict.into());
    }
    let changes = import::plan(
        &document,
        &snapshot.document,
        project,
        &overlays,
        &resolutions,
    )?;
    if changes.is_empty() {
        return Ok(source_revision(source, &snapshot.revision, &overlays));
    }
    let mut target = snapshot.document;
    if !project {
        for (id, configuration) in changes {
            target.insert(id, configuration);
        }
        let written = settings
            .update(source, &snapshot.revision, target, cancel)
            .await?;
        return Ok(source_revision(source, &written.revision, &overlays));
    }
    let workspace = config::workspace(settings, source)?;
    // Every id the import writes, an import or a replace, has its
    // `mcp-projects` record withdrawn before the source write, so an imported
    // configuration never runs enabled and a replaced entry cannot run while
    // it reads its new skeleton with its old record (D24, D25). Withdrawing
    // first also makes a failed compare-and-set fail closed: it writes no
    // entry, but an entry it would have replaced stays disabled, and
    // `mcp.set_enabled` enables it again. A failed withdrawal answers before
    // the source write, so `.mcp.json` is unchanged.
    for id in changes.keys() {
        set_project_enabled(settings, source, id, false, cancel).await?;
    }
    let mut records = Vec::with_capacity(changes.len());
    for (id, configuration) in changes {
        let (skeleton, record) = config::split_entry(&configuration);
        target.insert(id.clone(), skeleton);
        records.push((id, record));
    }
    let revision = settings
        .update(source, &snapshot.revision, target, cancel)
        .await?
        .revision;
    #[cfg(feature = "testing")]
    import::testing::pause(&workspace).await;
    write_import_secrets(settings, &workspace, &records, &overlays, cancel)
        .await
        .map_err(|_| McpError::ImportPartial)?;
    Ok(read_source_revision(settings, source, &revision, cancel).await)
}

/// Writes the withheld records of one import in one update. A record a
/// concurrent writer renewed since the plan is not ours to write: the import
/// answers `ImportPartial`, having already committed its skeletons, rather
/// than replacing a newer record with the one it planned (`mcp_import_partial`
/// reaches the caller through `write_import`). Other ids still commute, so a
/// lost race on one of them only re-runs the update. An entry with no withheld
/// part removes the destination's record, so the pair the import leaves is
/// exactly the configuration the document carried.
async fn write_import_secrets(
    settings: &Arc<SettingsService>,
    workspace: &str,
    records: &[(String, Option<McpSecretEntry>)],
    planned: &BTreeMap<String, McpSecretEntry>,
    cancel: &CancellationToken,
) -> Result<()> {
    loop {
        let snapshot = settings
            .read::<McpSecrets>(SettingsScope::User, cancel)
            .await?;
        let current = snapshot
            .document
            .entries
            .get(workspace)
            .cloned()
            .unwrap_or_default();
        if records.iter().any(|(id, _)| {
            current.get(id).map(|record| &record.generation)
                != planned.get(id).map(|record| &record.generation)
        }) {
            return Err(SettingsError::Conflict.into());
        }
        let mut document = snapshot.document;
        for (id, record) in records {
            match record {
                Some(record) => {
                    let mut record = record.clone();
                    record.generation = uuid::Uuid::new_v4().to_string();
                    document.set(workspace, id, record);
                }
                None => {
                    document.remove(workspace, id);
                }
            }
        }
        match settings
            .update(SettingsScope::User, &snapshot.revision, document, cancel)
            .await
        {
            Ok(_) => return Ok(()),
            Err(SettingsError::Conflict) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

/// The `mcp-secrets` records of one project source's workspace, by id. A user
/// source has no record.
async fn overlays(
    settings: &Arc<SettingsService>,
    source: SettingsScope,
    cancel: &CancellationToken,
) -> Result<BTreeMap<String, McpSecretEntry>> {
    let SettingsScope::Project(_) = source else {
        return Ok(BTreeMap::new());
    };
    let workspace = config::workspace(settings, source)?;
    Ok(settings
        .read::<McpSecrets>(SettingsScope::User, cancel)
        .await?
        .document
        .overlays(&workspace))
}

fn partial(_error: McpError, into_project: bool) -> McpError {
    McpError::MovePartial { into_project }
}

/// What separates this digest from any other digest of the same document.
const PROJECT_REVISION_TAG: &[u8] = b"dim-mcp-project-source-v1";

/// One project source's revision: its `.mcp.json` revision and this
/// workspace's withheld records as `(id, generation)` pairs, under one digest,
/// so the value a client holds covers both documents an entry is served from
/// (D23). A generation is random and renewed on every write (`config.rs`), so
/// the digest identifies a record version without deriving from its values.
fn project_revision(file: &str, overlays: &BTreeMap<String, McpSecretEntry>) -> String {
    let mut digest = Sha256::new();
    digest.update(PROJECT_REVISION_TAG);
    digest.update([0]);
    digest.update(file.as_bytes());
    for (id, record) in overlays {
        digest.update([0]);
        digest.update(id.as_bytes());
        digest.update([0]);
        digest.update(record.generation.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

/// The revision every method hands out and checks: a user source's document
/// revision unchanged, a project source's digest of that revision and its
/// withheld records.
fn source_revision(
    source: SettingsScope,
    file: &str,
    overlays: &BTreeMap<String, McpSecretEntry>,
) -> String {
    match source {
        SettingsScope::User => file.to_owned(),
        SettingsScope::Project(_) => project_revision(file, overlays),
    }
}

/// `source_revision` with the records read here, for the returns that do not
/// already hold them. A source whose records cannot be read keeps the document
/// revision: no caller can apply against it, so the fallback fails closed.
async fn read_source_revision(
    settings: &Arc<SettingsService>,
    source: SettingsScope,
    file: &str,
    cancel: &CancellationToken,
) -> String {
    match overlays(settings, source, cancel).await {
        Ok(overlays) => source_revision(source, file, &overlays),
        Err(_) => file.to_owned(),
    }
}

async fn write_secret(
    settings: &Arc<SettingsService>,
    workspace: &str,
    id: &str,
    mut secret: McpSecretEntry,
    cancel: &CancellationToken,
) -> Result<()> {
    secret.generation = uuid::Uuid::new_v4().to_string();
    let snapshot = settings
        .read::<McpSecrets>(SettingsScope::User, cancel)
        .await?;
    let mut document = snapshot.document;
    document.set(workspace, id, secret);
    settings
        .update(SettingsScope::User, &snapshot.revision, document, cancel)
        .await?;
    Ok(())
}

async fn remove_secret(
    settings: &Arc<SettingsService>,
    workspace: &str,
    id: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let snapshot = settings
        .read::<McpSecrets>(SettingsScope::User, cancel)
        .await?;
    let mut document = snapshot.document;
    if !document.remove(workspace, id) {
        return Ok(());
    }
    settings
        .update(SettingsScope::User, &snapshot.revision, document, cancel)
        .await?;
    Ok(())
}

/// Deletes the record only while it still carries `generation`; a replacement
/// move writes a new generation for the same id, and that record is kept.
async fn remove_secret_if(
    settings: &Arc<SettingsService>,
    workspace: &str,
    id: &str,
    generation: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    loop {
        let snapshot = settings
            .read::<McpSecrets>(SettingsScope::User, cancel)
            .await?;
        let Some(current) = snapshot.document.get(workspace, id) else {
            return Ok(());
        };
        if current.generation != generation {
            return Ok(());
        }
        let mut document = snapshot.document;
        document.remove(workspace, id);
        match settings
            .update(SettingsScope::User, &snapshot.revision, document, cancel)
            .await
        {
            Ok(_) => return Ok(()),
            Err(SettingsError::Conflict) => continue,
            Err(error) => return Err(error.into()),
        }
    }
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
    credentials: Key<CredentialVariablesHost>,
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
            credentials: names.key()?,
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
            self.credentials.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let publication = ctx.require_at(&self.publication)?;
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            let parts = ctx.require_at(&self.parts)?;
            let credentials = ctx.require_at(&self.credentials)?;
            settings.register::<McpSettings>(ctx)?;
            settings.register::<McpProjectEnablement>(ctx)?;
            settings.register::<McpPluginServers>(ctx)?;
            settings.register::<McpSecrets>(ctx)?;
            let auth = McpAuth::new(files, self.home_root).map_err(ErrorKind::PluginFailed)?;
            let service = Arc::new(McpService {
                settings,
                credentials,
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
