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
pub use session::{McpSession, McpSessionPlugin, McpSessionState, McpSourceState, install};

use crate::settings::{SettingsError, SettingsScope, SettingsService};
use config::{McpProjectEnablement, McpServerSettings, McpSettings};
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

/// Server-side configuration, including secrets. Not a wire or logging type.
/// For a project entry, the configuration is effective: its enablement comes
/// from the user's record (`config::effective`).
#[derive(Clone)]
pub struct McpServerEntry {
    pub source: SettingsScope,
    pub configuration: McpServerSettings,
}
impl McpServerEntry {
    pub fn enabled(&self) -> bool {
        match &self.configuration {
            McpServerSettings::Stdio { enabled, .. }
            | McpServerSettings::Remote { enabled, .. } => enabled.unwrap_or(true),
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
        source: SettingsScope,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let owner = self.clone();
        self.run(cancel, move |_, cancel| async move {
            owner.publication.disconnect(source, id, &cancel).await
        })
        .await
    }

    pub fn application(&self, source: SettingsScope) -> McpApplication {
        self.publication.application(source)
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
            for source in
                std::iter::once(SettingsScope::User).chain(project.map(SettingsScope::Project))
            {
                let snapshot = settings.read::<McpSettings>(source, &cancel).await?;
                match source {
                    SettingsScope::User => catalog.user_revision = snapshot.revision,
                    SettingsScope::Project(_) => {
                        catalog.project_revision = Some(snapshot.revision);
                    }
                }
                let document = snapshot.document;
                for (id, configuration) in document.mcp_servers {
                    let configuration =
                        config::effective(&settings, source, &id, configuration, &enablement)?;
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
        source: SettingsScope,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<McpServerEntry> {
        self.run(cancel, move |settings, cancel| async move {
            let configuration = settings
                .read::<McpSettings>(source, &cancel)
                .await?
                .document
                .mcp_servers
                .remove(&id)
                .ok_or(McpError::UnknownServer)?;
            let enablement = match source {
                SettingsScope::User => McpProjectEnablement::default(),
                SettingsScope::Project(_) => settings
                    .read::<McpProjectEnablement>(SettingsScope::User, &cancel)
                    .await
                    .map(|snapshot| snapshot.document)
                    .unwrap_or_default(),
            };
            Ok(McpServerEntry {
                source,
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
            let auth = McpAuth::new(files, self.home_root).map_err(ErrorKind::PluginFailed)?;
            let service = Arc::new(McpService {
                settings,
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
