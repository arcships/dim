mod agent_records;
mod agents;
mod approval;
pub mod cli;
pub mod codex_oauth;
pub mod collaboration;
mod collaboration_model_policy;
mod connection;
mod control;
mod exec_shell_settings;
mod execution_settings;
pub mod grok_oauth;
pub mod hooks_settings;
mod managed_oauth;
pub mod managed_provider;
pub mod mcp;
pub mod methods;
pub mod modality;
pub mod model_catalog;
pub mod model_log;
mod outbound;
pub mod permission_rules;
#[cfg(windows)]
mod pipe_client;
pub mod plugin_files;
pub mod plugin_manager;
pub mod prompt_suggestion_settings;
pub mod protocol;
pub mod provider;
pub mod runtime_manager;
pub mod session_grants;
pub mod session_typescript;
pub mod settings;
pub mod settings_methods;
mod skills_management;
pub mod storage;
mod transport;
mod tree_protocol;
mod ui_host;
mod ui_leaf;
pub mod web;
mod web_entry;

use agents::{AgentIndex, AgentsPlugin};
use collaboration::CollaborationPlugin;
use connection::{ConnectionHub, ConnectionPlugin, RpcPlugin};
use control::{ServerControl, ServerControlPlugin};
use dim_agent_runtime::{
    Agent, AgentBuilder, AgentError, Modules, SessionConfig, StartupProgress, register_part,
    wait_active,
};
use rutis::Ctx;
use rutis::host::{CtxHost, FiberView, Key};
use rutis_loader::runtime::SessionRows;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use transport::{ListenAddresses, TransportPlugin};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub unix_path: Option<PathBuf>,
    pub pipe_name: Option<String>,
    pub websocket_address: Option<SocketAddr>,
    pub database_path: Option<PathBuf>,
    pub target_home: Option<PathBuf>,
    pub product_home: Option<PathBuf>,
    /// The user home the process reports, `$HOME` by default. A host that
    /// runs with an isolated home sets it; every capability then resolves
    /// `~` and the user Skill root inside it.
    pub platform_home: Option<PathBuf>,
    /// A managed provider plugin built outside this repository.
    pub managed_provider: Option<managed_provider::ManagedProviderRow>,
    pub session_typescript: Option<session_typescript::SessionTypeScriptConfig>,
    pub daemon_profile: protocol::DaemonProfile,
    /// Admission is disabled until the product execution profile is installed.
    pub session_execution: bool,
    pub model_catalog: Option<Arc<dim_agent_aimux::AimuxModels>>,
    pub file_roots: Vec<PathBuf>,
    pub session_defaults: SessionConfig,
    pub hello_timeout: Duration,
    pub write_timeout: Duration,
    /// Maximum time without an opening process row becoming Active.
    pub startup_timeout: Duration,
    /// Grace period `G` of idle unload: with a native store, Agents unloads a
    /// loaded Session that no lease has held for at least this long.
    /// Positive: `serve` refuses zero, which would run the pass in a loop.
    pub idle_unload_after: Duration,
    /// Built Web workspace assets served by the local web entry.
    pub web_assets: Option<PathBuf>,
    /// The default plugin configuration file (PluginManager §2.3).
    pub plugin_defaults: Option<PathBuf>,
    /// A default plugin's row configuration (PluginManager §2.1): the
    /// manifest's `config` with this plugin name's patch deep-merged over
    /// it. The product sets none, so a shipped plugin reads its manifest's
    /// value; the evaluation tooling sets the member a candidate text
    /// replaces, so both arms run the same plugin code.
    pub plugin_configs: BTreeMap<String, Value>,
    /// The plugin files root (PluginManager §2.1): `<root>/<plugin id>/` is
    /// a plugin's data directory. The product sets it through a product
    /// home; a host without one names it here.
    pub plugin_files: Option<PathBuf>,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            unix_path: None,
            pipe_name: None,
            websocket_address: Some(([127, 0, 0, 1], 0).into()),
            database_path: None,
            target_home: None,
            product_home: None,
            platform_home: None,
            managed_provider: None,
            session_typescript: None,
            daemon_profile: protocol::DaemonProfile::Mock,
            session_execution: true,
            model_catalog: None,
            file_roots: vec![],
            session_defaults: SessionConfig::default(),
            hello_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(10),
            startup_timeout: Duration::from_secs(10),
            idle_unload_after: Duration::from_secs(60),
            web_assets: None,
            plugin_defaults: None,
            plugin_configs: BTreeMap::new(),
            plugin_files: None,
        }
    }
}
impl ServerConfig {
    /// Public execution capabilities do not depend on an account provider.
    pub(crate) fn local_execution(&self) -> bool {
        self.session_execution && self.protected_profile()
    }

    pub(crate) fn protected_profile(&self) -> bool {
        self.product_home.is_some() || self.model_catalog.is_some()
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("server I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("session cleanup: {0}")]
    Runtime(#[from] dim_agent_runtime::AgentError),
    #[error("server task: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("invalid server configuration: {0}")]
    Configuration(String),
    #[error("{cause}; cleanup: {cleanup}")]
    Cleanup {
        cause: Box<ServerError>,
        cleanup: dim_agent_runtime::AgentError,
    },
}
pub type Result<T> = std::result::Result<T, ServerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    Graceful,
    Forced,
}

pub struct RunningServer {
    pub unix_path: Option<PathBuf>,
    pub pipe_name: Option<String>,
    pub websocket_address: Option<SocketAddr>,
    root: Ctx,
    config: Arc<ServerConfig>,
    processes: Processes,
    control: Arc<ServerControl>,
    task: Option<JoinHandle<Result<ShutdownReason>>>,
}
impl RunningServer {
    /// Starts owned cleanup; dropping the returned future does not stop it.
    pub fn shutdown(&self) {
        self.control.shutdown().cancel();
    }
    pub fn shutdown_signal(&self) -> CancellationToken {
        self.control.shutdown().clone()
    }
    /// Explicitly reloads one process plugin; plugins that inject its services
    /// reload with it, and this returns once every process plugin is Active.
    /// Only plugins in [`RESTARTABLE`] are accepted. Unloading `Rpc` (alone or
    /// with a provider it injects) ends commands waiting on client input with
    /// `service_reloading` and waits for the others to settle, without deadline.
    /// Windows named pipe transport reloads also close connections so all pipe
    /// handles are released before reclaiming the endpoint exclusively.
    pub async fn restart(&self, plugin: &str) -> Result<()> {
        #[cfg(windows)]
        let plugin = if plugin == "Transport" && self.pipe_name.is_some() {
            "Connection"
        } else {
            plugin
        };
        if !RESTARTABLE.contains(&plugin) {
            return Err(ServerError::Configuration(format!(
                "process plugin {plugin} cannot be restarted"
            )));
        }
        match self.processes.loader.restart(plugin).await {
            Err(rutis_loader::LoaderError::UnknownEntry(_)) => {
                return Err(ServerError::Configuration(format!(
                    "process plugin {plugin} is not registered"
                )));
            }
            Err(rutis_loader::LoaderError::Rejected { error, .. }) => {
                return Err(ServerError::Runtime(error.into()));
            }
            Err(error) => return Err(ServerError::Runtime(AgentError::Build(error.to_string()))),
            Ok(()) => {}
        }
        self.processes
            .wait_active(&self.root, self.config.startup_timeout)
            .await?;
        Ok(())
    }
    /// The default Agent of the current Agents generation.
    pub fn agent(&self) -> Option<Agent> {
        self.root
            .get::<AgentIndex>()
            .and_then(|index| index.default_host())
            .map(|host| host.agent.clone())
    }
    /// Live diagnostics of the process tree.
    pub fn diagnostics(&self) -> rutis::host::diagnostics::RuntimeDiagnostics {
        self.root.diagnostics()
    }
    /// A process service of the served profile; the `summary-eval` runner reads
    /// the provider catalog, the model host and file storage this way.
    pub fn service<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.root.get::<T>()
    }
    /// Emits a Session notice from the process root, standing in for an
    /// owner of process-level state after its commit. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn notify(
        &self,
        session_id: &str,
        notice: dim_agent_capabilities::session_broadcast::SessionNotice,
    ) {
        dim_agent_capabilities::session_broadcast::notify(&self.root, session_id, notice);
    }
    /// The `sessions.idle` admission signal of the current Agents
    /// generation, notified once a call is waiting on its branch. Feature
    /// `testing` only.
    #[cfg(feature = "testing")]
    pub fn idle_admitted(&self) -> Option<Arc<tokio::sync::Notify>> {
        self.root
            .get::<AgentIndex>()
            .map(|index| index.idle_admitted())
    }
    pub async fn join(mut self) -> Result<ShutdownReason> {
        self.task.take().expect("server task").await?
    }
}
impl Drop for RunningServer {
    fn drop(&mut self) {
        self.control.shutdown().cancel();
    }
}

/// Process plugins whose reload keeps Session state consistent: reloaded
/// Session capabilities follow the capability reload protocol; `ServerControl`
/// is owned by `RunningServer`.
pub const RESTARTABLE: [&str; 5] = ["Rpc", "Connection", "Transport", "Agents", "Mcp"];

/// The process loader and its rows.
struct Processes {
    loader: rutis_loader::Loader,
    view: FiberView,
    runtime_rows: Vec<Key<SessionRows>>,
    required: Vec<String>,
}
impl Processes {
    /// The loader and required host rows; extension readiness stays local.
    fn views(&self) -> Vec<FiberView> {
        let rows = self
            .loader
            .entries()
            .into_iter()
            .filter(|entry| {
                self.required.contains(&entry.id)
                    && entry.parent.is_none()
                    && entry.instance.is_none()
            })
            .filter_map(|entry| entry.view);
        std::iter::once(self.view.clone()).chain(rows).collect()
    }

    fn roots(&self) -> Vec<rutis::host::PluginId> {
        std::iter::once(self.view.id)
            .chain(
                self.loader
                    .entries()
                    .into_iter()
                    .filter(|entry| entry.parent.is_none() && entry.instance.is_none())
                    .filter_map(|entry| entry.view.map(|view| view.id)),
            )
            .collect()
    }

    async fn wait_active(&self, root: &Ctx, timeout: Duration) -> dim_agent_runtime::Result<()> {
        let mut progress = StartupProgress::new(root, timeout)?;
        match progress.wait(self.ready(root), || self.roots()).await {
            Some(result) => result,
            None => Err(self.timed_out(root).await),
        }
    }

    async fn ready(&self, root: &Ctx) -> dim_agent_runtime::Result<()> {
        loop {
            let views = self.views();
            let mut ready = true;
            for view in &views {
                let snapshot = view.watch().borrow().clone();
                match snapshot.state {
                    rutis::host::FiberState::Active => {}
                    rutis::host::FiberState::Failed => {
                        return Err(snapshot.error.map(AgentError::from).unwrap_or_else(|| {
                            AgentError::Build(format!("{}: Failed", view.name()))
                        }));
                    }
                    _ => ready = false,
                }
            }
            ready &= self
                .runtime_rows
                .iter()
                .all(|key| root.get_at(key).is_some());
            if ready {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn timed_out(&self, root: &Ctx) -> AgentError {
        if let Some(key) = self
            .runtime_rows
            .iter()
            .find(|key| root.get_at(key).is_none())
        {
            return AgentError::Build(format!("runtime rows were not released: {:?}", key.erase()));
        }
        wait_active(
            root,
            &self.views(),
            Duration::ZERO,
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap_or_else(|| AgentError::Build("process assembly timed out".into()))
    }
}

/// Shuts the process tree down if `serve` ends before handing it to a
/// `RunningServer`, including when the `serve` future is dropped.
struct Startup {
    root: Ctx,
    control: Arc<ServerControl>,
    armed: bool,
}
impl Drop for Startup {
    fn drop(&mut self) {
        if self.armed {
            self.control.shutdown().cancel();
            let shutdown = self.root.shutdown();
            self.root.handle().spawn(async move {
                if let Err(error) = shutdown.await {
                    tracing::error!(%error, "Cancelled App Server startup cleanup failed");
                }
            });
        }
    }
}

/// Builds the process tree, opens admission once required host rows are Active
/// and the runtimes have released their rows, and returns the listener addresses.
pub async fn serve(mut builder: AgentBuilder, mut config: ServerConfig) -> Result<RunningServer> {
    if config.managed_provider.is_some() && config.product_home.is_none() {
        return Err(ServerError::Configuration(
            "managed providers require a product home".into(),
        ));
    }
    if config.model_catalog.is_some() && config.product_home.is_some() {
        return Err(ServerError::Configuration(
            "configured and product model providers are mutually exclusive".into(),
        ));
    }
    if config.model_catalog.is_some()
        && config.target_home.is_none()
        && config.database_path.is_none()
    {
        return Err(ServerError::Configuration(
            "configured execution requires --home or --database for persistent settings".into(),
        ));
    }
    if config.product_home.is_some() {
        config.daemon_profile = protocol::DaemonProfile::Product;
    }
    builder.require_part("AgentsSession");
    #[cfg(not(unix))]
    if config.unix_path.is_some() {
        return Err(ServerError::Configuration(
            "Unix sockets are unavailable on this platform".into(),
        ));
    }
    #[cfg(not(windows))]
    if config.pipe_name.is_some() {
        return Err(ServerError::Configuration(
            "Windows named pipes are unavailable on this platform".into(),
        ));
    }
    if config.unix_path.is_none()
        && config.pipe_name.is_none()
        && config.websocket_address.is_none()
    {
        return Err(ServerError::Configuration(
            "at least one listener is required".into(),
        ));
    }
    if config.idle_unload_after.is_zero() {
        return Err(ServerError::Configuration(
            "the idle unload grace period must be positive".into(),
        ));
    }
    if config.file_roots.is_empty() {
        config
            .file_roots
            .push(PathBuf::from(&config.session_defaults.cwd));
    }
    if let Some(home) = &config.target_home {
        if config.database_path.is_some() {
            return Err(ServerError::Configuration(
                "target_home and database_path are mutually exclusive".into(),
            ));
        }
        create_private_directory(home)?;
        let home = std::fs::canonicalize(home)?;
        config.database_path = Some(home.join("dimcode.sqlite"));
        config.file_roots.push(home.clone());
        config.target_home = Some(home);
    }
    let mut file_roots = Vec::new();
    for root in &config.file_roots {
        let root = std::fs::canonicalize(root)?;
        if !file_roots.contains(&root) {
            file_roots.push(root);
        }
    }
    config.file_roots = file_roots;
    let product_root = if let Some(home) = &config.product_home {
        std::fs::create_dir_all(home)?;
        let home = std::fs::canonicalize(home)?;
        let index = match config.file_roots.iter().position(|root| root == &home) {
            Some(index) => index,
            None => {
                config.file_roots.push(home.clone());
                config.file_roots.len() - 1
            }
        };
        if config.database_path.is_none() {
            return Err(ServerError::Configuration(
                "product home requires native storage".into(),
            ));
        }
        config.product_home = Some(home);
        Some(index)
    } else {
        None
    };
    let plugin_root = match &config.plugin_files {
        Some(root) => {
            create_private_directory(root)?;
            let root = std::fs::canonicalize(root)?;
            let index = match config
                .file_roots
                .iter()
                .position(|candidate| candidate == &root)
            {
                Some(index) => index,
                None => {
                    config.file_roots.push(root);
                    config.file_roots.len() - 1
                }
            };
            Some(index)
        }
        None => product_root,
    };
    let execution_root = if config.local_execution() {
        let cwd = std::env::current_dir()?;
        let home = config
            .product_home
            .clone()
            .or_else(|| config.target_home.clone())
            .or_else(|| {
                cwd.join(config.database_path.as_ref()?)
                    .parent()
                    .map(PathBuf::from)
            })
            .ok_or_else(|| ServerError::Configuration("database must name a file".into()))?;
        create_private_directory(&home)?;
        let home = std::fs::canonicalize(home)?;
        Some(
            match config.file_roots.iter().position(|root| root == &home) {
                Some(index) => index,
                None => {
                    config.file_roots.push(home);
                    config.file_roots.len() - 1
                }
            },
        )
    } else {
        None
    };
    if let Some(index) = execution_root {
        let legacy = config.file_roots[index].join("host-isolation.json");
        if legacy.is_file() {
            eprintln!(
                "Ignoring {}: command isolation is a user setting in /settings > Process isolation; delete the file.",
                legacy.display()
            );
        }
    }
    // SessionStore keeps Session files next to the native database, below
    // the database directory as a FileStorage root (session-files §3.1).
    let session_files_root = match &config.database_path {
        Some(path) => {
            let directory = std::env::current_dir()?
                .join(path)
                .parent()
                .map(PathBuf::from)
                .ok_or_else(|| ServerError::Configuration("database must name a file".into()))?;
            create_private_directory(&directory)?;
            let directory = std::fs::canonicalize(directory)?;
            Some(
                match config.file_roots.iter().position(|root| root == &directory) {
                    Some(index) => index,
                    None => {
                        config.file_roots.push(directory);
                        config.file_roots.len() - 1
                    }
                },
            )
        }
        None => None,
    };
    let config = Arc::new(config);
    let root = Ctx::root_with_sink(
        tokio::runtime::Handle::current(),
        Arc::new(|error| tracing::error!(%error, "App Server runtime error")),
    );
    let control = ServerControlPlugin::new();
    let handle = control.0.clone();
    let mut startup = Startup {
        root: root.clone(),
        control: handle.clone(),
        armed: true,
    };
    root.provide(rutis_bridge::LocalId(
        rutis_bridge::PeerId::new("dim-daemon").expect("a peer id"),
    ))
    .map_err(|error| ServerError::Runtime(error.into()))?;
    let mut builtins = Modules::new();
    dim_agent_capabilities::register_names(&mut builder);
    register_names(builder.catalog_mut());
    dim_agent_runtime::names::register::<dim_agent_capabilities::methods::ReservedMethods>(
        builder.catalog_mut(),
        None,
    );
    builder.process_value(Arc::new(dim_agent_capabilities::methods::ReservedMethods(
        methods::reserved_for_sessions(),
    )));
    let mut rows = vec![register_part(
        &mut builtins,
        builder.catalog(),
        move |_| Ok(Box::new(control.clone())),
    )?];
    let native_home = execution_root.filter(|_| config.database_path.is_some());
    let files =
        dim_agent_capabilities::file_storage::FileStoragePlugin::new(config.file_roots.clone());
    rows.push(register_part(
        &mut builtins,
        builder.catalog(),
        move |_| Ok(Box::new(files.clone())),
    )?);
    if config.model_catalog.is_some() || product_root.is_some() {
        rows.push(register_part(&mut builtins, builder.catalog(), |_| {
            Ok(Box::new(dim_agent_aimux::AimuxPlugin))
        })?);
    }
    if let Some(models) = &config.model_catalog {
        dim_agent_aimux::register_names(&mut builder);
        let models = models.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |_| {
                Ok(Box::new(dim_agent_aimux::ConfiguredModelsPlugin(
                    models.clone(),
                )))
            },
        )?);
    }
    if let (Some(path), Some(files_root)) = (&config.database_path, session_files_root) {
        let path = path.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |_| Ok(Box::new(storage::SqliteStoragePlugin::new(path.clone()))),
        )?);
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(storage::SessionStorePlugin::new(
                    names, files_root,
                )?))
            },
        )?);
    }
    rows.push(register_part(&mut builtins, builder.catalog(), |names| {
        Ok(Box::new(methods::ManagementMethodsPlugin::new(names)?))
    })?);
    rows.push(register_part(&mut builtins, builder.catalog(), |_| {
        Ok(Box::new(managed_provider::ManagedProvidersPlugin))
    })?);
    if let Some(home_root) = plugin_root {
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(plugin_files::PluginFilesPlugin::new(
                    names, home_root,
                )?))
            },
        )?);
    }
    if let Some(home_root) = product_root {
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(
                managed_provider::ManagedProviderMethodsPlugin::new(names)?,
            ))
        })?);
        if let Some(provider) = &config.managed_provider {
            rows.push((provider.0)(&mut builtins, home_root));
            rows.push(register_part(&mut builtins, builder.catalog(), |names| {
                Ok(Box::new(managed_provider::ManagedProviderBinding::new(
                    names,
                )?))
            })?);
        }
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(codex_oauth::OpenAICodexOAuthPlugin::new(
                    names, home_root,
                )?))
            },
        )?);
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(grok_oauth::GrokOAuthPlugin::new(
                    names, home_root,
                )?))
            },
        )?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(provider::ProviderPlugin::with_session_lifecycle(
                names,
            )?))
        })?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(modality::ModalityPlugin::new(names)?))
        })?);
    }
    if let Some(home_root) = execution_root.or(product_root) {
        let build = rutis_loader::resolve::Build::default();
        let names = dim_agent_runtime::Names::copy(builder.catalog(), &build);
        let settings = if native_home.is_some() {
            settings::SettingsPlugin::native(&names, home_root, vec![])
        } else {
            settings::SettingsPlugin::new(&names, home_root, vec![])
        }
        .map_err(|error| ServerError::Configuration(error.to_string()))?;
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |_| Ok(Box::new(settings.clone())),
        )?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(settings_methods::SettingsMethodsPlugin::new(
                names,
            )?))
        })?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(web::WebPlugin::new(names)?))
        })?);
        if config.model_catalog.is_some() || product_root.is_some() {
            rows.push(register_part(
                &mut builtins,
                builder.catalog(),
                move |names| Ok(Box::new(model_log::ModelLogPlugin::new(names, home_root)?)),
            )?);
        }
    }
    if let Some(home_root) = execution_root {
        let platform_home = config.platform_home.clone();
        mcp::install(&mut builder);
        if native_home.is_some() {
            dim_agent_attachments::install(&mut builder);
        }
        rows.push(mcp::McpPublication::register(
            &mut builtins,
            builder.catalog(),
        )?);
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| Ok(Box::new(mcp::McpPlugin::new(names, home_root)?)),
        )?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(execution_settings::ExecutionSettingsPlugin::new(
                names,
            )?))
        })?);
        dim_agent_runtime::names::register::<
            dim_agent_capabilities::prompt_suggestion::PromptSuggestionSettingsHost,
        >(builder.catalog_mut(), None);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(
                prompt_suggestion_settings::PromptSuggestionSettingsPlugin::new(names)?,
            ))
        })?);
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(
                    dim_agent_capabilities::host_platform::HostPlatformPlugin::new(
                        names,
                        dim_agent_capabilities::host_platform::HostPlatformConfig {
                            execution: names.key()?,
                            exec_shell_file: Some((home_root, "exec-shell.json".into())),
                            home: platform_home.clone(),
                        },
                    )?,
                ))
            },
        )?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(exec_shell_settings::ExecShellSettingsPlugin::new(
                names,
            )?))
        })?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(execution_settings::ExecutionBackendPlugin::new(
                names,
            )?))
        })?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(skills_management::SkillsCatalogPlugin::new(
                names,
            )?))
        })?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(permission_rules::PermissionRulesPlugin::new(
                names,
            )?))
        })?);
        let product_home = config.product_home.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(hooks_settings::HooksSettingsPlugin::new(
                    names,
                    product_home.clone(),
                )?))
            },
        )?);
        rows.push(register_part(&mut builtins, builder.catalog(), |names| {
            Ok(Box::new(
                collaboration_model_policy::CollaborationModelPolicyPlugin::new(names)?,
            ))
        })?);
    }
    let plugin_home = config
        .product_home
        .clone()
        .or_else(|| config.target_home.clone());
    let plugins = match &plugin_home {
        Some(home) => Some(
            plugin_manager::State::read(
                home,
                plugin_root.and(
                    config
                        .plugin_files
                        .as_deref()
                        .or(config.product_home.as_deref()),
                ),
                config.plugin_defaults.as_deref(),
                &config.plugin_configs,
                builder.catalog_mut(),
            )
            .map_err(ServerError::Configuration)?,
        ),
        None if config.plugin_defaults.is_some() => {
            return Err(ServerError::Configuration(
                "default plugins need a product or target home".into(),
            ));
        }
        None => None,
    };
    let plugin_content = match &plugins {
        Some(state) => {
            let build = rutis_loader::resolve::Build::default();
            let names = dim_agent_runtime::Names::copy(builder.catalog(), &build);
            let owners = plugin_manager::Owners::new(root.clone(), &names, state.refusals.clone())
                .map_err(|error| ServerError::Configuration(error.to_string()))?;
            Some((state.clone(), owners))
        }
        None => None,
    };
    let session_parts = config
        .session_typescript
        .as_ref()
        .map(|parts| session_typescript::Parts::new(parts, &mut builder, &config.file_roots))
        .transpose()?;
    if let Some(state) = &plugins {
        let state = state.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(plugin_manager::PluginManagerPlugin::new(
                    names,
                    state.clone(),
                )?))
            },
        )?);
    }
    let acquire = config
        .session_typescript
        .as_ref()
        .and_then(|parts| parts.acquire.clone());
    let mut optional = Vec::new();
    if let Some(parts) = &session_parts {
        if let Some(runtimes) = &acquire {
            let python = parts.python.as_ref().ok_or_else(|| {
                ServerError::Configuration("RuntimeManager needs the Python runtime".into())
            })?;
            if let Some(project) = config
                .session_typescript
                .as_ref()
                .and_then(|parts| parts.python.as_ref())
                .map(|python| &python.project)
            {
                std::fs::create_dir_all(project)?;
            }
            let (runtimes, node, python) = (runtimes.clone(), parts.node.clone(), python.clone());
            rows.push(register_part(
                &mut builtins,
                builder.catalog(),
                move |names| {
                    Ok(Box::new(runtime_manager::RuntimeManagerPlugin::new(
                        names,
                        runtimes.clone(),
                        node.clone(),
                        python.clone(),
                    )?))
                },
            )?);
        }
        for foreign in parts.runtimes() {
            if acquire.is_some() {
                optional.push(foreign.row);
            } else {
                root.provide_at(foreign.command.clone(), foreign.launch.clone())
                    .map_err(|error| ServerError::Runtime(error.into()))?;
            }
            let runtime = foreign.clone();
            rows.push(register_part(
                &mut builtins,
                builder.catalog(),
                move |_| Ok(Box::new(runtime.connection())),
            )?);
        }
    }
    agents::register(&mut builtins);
    rows.push(register_part(&mut builtins, builder.catalog(), |_| {
        Ok(Box::new(agents::AgentDirectoryPlugin))
    })?);
    rows.push(register_part(&mut builtins, builder.catalog(), |_| {
        Ok(Box::new(collaboration::CollaborationDirectoryPlugin))
    })?);
    {
        let (assembly, config, root) = (builder.clone(), config.clone(), root.clone());
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(AgentsPlugin::new(
                    names,
                    assembly.clone(),
                    config.clone(),
                    root.clone(),
                )?))
            },
        )?);
    }
    {
        let local_execution = config.local_execution();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| Ok(Box::new(CollaborationPlugin::new(names, local_execution)?)),
        )?);
    }
    rows.push(register_part(&mut builtins, builder.catalog(), |_| {
        Ok(Box::new(ui_leaf::UiHostPlugin))
    })?);
    {
        let config = config.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| Ok(Box::new(ConnectionPlugin::new(names, config.clone())?)),
        )?);
    }
    {
        let config = config.clone();
        let root = root.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| {
                Ok(Box::new(RpcPlugin::new(
                    names,
                    config.clone(),
                    root.clone(),
                )?))
            },
        )?);
    }
    {
        let config = config.clone();
        rows.push(register_part(
            &mut builtins,
            builder.catalog(),
            move |names| Ok(Box::new(TransportPlugin::new(names, config.clone())?)),
        )?);
    }
    match builder.plugin_rows(&mut builtins).await {
        Ok(plugins) => rows.extend(
            plugins
                .iter()
                .map(|row| row["id"].as_str().expect("plugin row id").to_owned()),
        ),
        Err(error) => {
            startup.armed = false;
            handle.shutdown().cancel();
            return Err(match root.shutdown().await {
                Ok(()) => error,
                Err(cleanup) => error.with_cleanup(cleanup.into()),
            }
            .into());
        }
    }
    let reserved_names = session_parts
        .iter()
        .flat_map(|parts| &parts.rows)
        .flat_map(|row| [&row["id"], &row["name"]])
        .filter_map(serde_json::Value::as_str);
    let session_row = match builder.install(&mut builtins, reserved_names).await {
        Ok(row) => row,
        Err(error) => {
            startup.armed = false;
            handle.shutdown().cancel();
            return Err(match root.shutdown().await {
                Ok(()) => error,
                Err(cleanup) => error.with_cleanup(cleanup.into()),
            }
            .into());
        }
    };
    let mut rows: Vec<_> = rows
        .iter()
        .map(|name| serde_json::json!({ "id": name, "name": name }))
        .collect();
    rows.push(agents::group_row(&builder, session_row));
    let mut required: Vec<String> = rows
        .iter()
        .map(|row| row["id"].as_str().expect("host row id").to_owned())
        .filter(|id| !optional.contains(&id.as_str()))
        .collect();
    if let Some(parts) = &session_parts {
        for row in &parts.rows {
            if rows
                .iter()
                .any(|host| row["id"] == host["id"] || row["name"] == host["name"])
            {
                return Err(ServerError::Configuration(
                    "TypeScript row collides with a host row".into(),
                ));
            }
        }
        rows.extend(parts.rows.clone());
    }
    let runtime_rows = session_parts
        .iter()
        .filter(|_| acquire.is_none())
        .flat_map(|parts| parts.runtimes())
        .map(|foreign| SessionRows::key(foreign.row))
        .collect();
    let plugin_layer = match &plugins {
        Some(state) => {
            let builtins = rows
                .iter()
                .filter(|row| row.get("group").is_none())
                .map(|row| row["id"].as_str().expect("host row id").to_owned())
                .filter(|id| required.contains(id))
                .collect();
            let layer = state
                .layer(builtins, builder.catalog().clone())
                .map_err(ServerError::Configuration)?;
            required.retain(|row| !layer.off.contains(row));
            required.extend(layer.locked);
            Some(rutis_loader::Layer::new(
                plugin_manager::LAYER,
                layer.patches,
            ))
        }
        None => None,
    };
    let sessions = session_parts.map(|parts| {
        let (sessions, snapshots) = parts.resolver(builder.catalog());
        let rows: Vec<_> = parts.runtimes().map(|foreign| foreign.row).collect();
        (sessions, snapshots, rows)
    });
    let mut resolver = rutis_loader::Chain::new().with(builtins);
    if let Some((_, snapshots, _)) = &sessions {
        resolver = resolver.with_shared(snapshots.clone());
    }
    let plugin = rutis_loader::LoaderPlugin::new(
        resolver,
        rutis_loader::LoaderOptions {
            catalog: builder.catalog().clone(),
            ..Default::default()
        },
    );
    let loader = plugin.handle();
    if let Some((sessions, _, _)) = &sessions {
        sessions.attach(&loader);
    }
    let loader_view = root.plugin(plugin);
    let processes = Processes {
        loader,
        view: loader_view,
        runtime_rows,
        required,
    };
    let assembly = rutis_loader::Layer::new(
        "assembly",
        vec![rutis_loader::Patch {
            insert: Some(rows),
            ..Default::default()
        }],
    );
    let mut progress = StartupProgress::new(&root, config.startup_timeout)?;
    let started = progress
        .wait(
            async {
                wait_active(
                    &root,
                    std::slice::from_ref(&processes.view),
                    config.startup_timeout,
                    &CancellationToken::new(),
                )
                .await?;
                if let Some((sessions, _, rows)) = &sessions {
                    for row in rows {
                        sessions.follow_row(row);
                    }
                }
                let layers = std::iter::once(assembly.clone())
                    .chain(plugin_layer.clone())
                    .collect();
                let report = processes
                    .loader
                    .reconcile(layers, None)
                    .await
                    .map_err(|error| AgentError::Build(error.to_string()))?;
                let failures: Vec<_> = report
                    .failures
                    .iter()
                    .filter(|failure| processes.required.contains(&failure.id))
                    .collect();
                if !failures.is_empty() || !report.issues.is_empty() {
                    return Err(AgentError::Build(format!(
                        "process assembly: {:?} {:?}",
                        failures, report.issues
                    )));
                }
                processes.ready(&root).await
            },
            || processes.roots(),
        )
        .await;
    let started = match started {
        Some(result) => result,
        None => Err(processes.timed_out(&root).await),
    };
    if let Err(error) = started {
        startup.armed = false;
        handle.shutdown().cancel();
        let cleanup = root.shutdown().await;
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => error.with_cleanup(cleanup.into()),
        }
        .into());
    }
    if let Some((state, owners)) = &plugin_content {
        state.startup_content(owners).await;
    }
    let addresses = root
        .get::<ListenAddresses>()
        .ok_or_else(|| ServerError::Configuration("listener addresses missing".into()))?;
    handle.open();
    let task = tokio::spawn(coordinate(root.clone(), handle.clone()));
    startup.armed = false;
    Ok(RunningServer {
        unix_path: addresses.unix_path.clone(),
        pipe_name: addresses.pipe_name.clone(),
        websocket_address: addresses.websocket_address,
        root,
        config,
        processes,
        control: handle,
        task: Some(task),
    })
}

/// The names the App Server's parts provide or read (rutis usage §3.4), and
/// the names its process parts share across languages. Registering them again
/// does nothing, so each installer applies them.
pub(crate) fn register_names(catalog: &mut dim_agent_runtime::names::ServiceCatalog) {
    use dim_agent_capabilities::host::{
        AuxiliaryModelHost, ModelSelectionHost, SelectableModelsHost,
    };
    use dim_agent_runtime::{SESSION_GROUP, names::register};
    register::<ServerControl>(catalog, None);
    register::<storage::SqliteStorage>(catalog, None);
    register::<storage::SessionStore>(catalog, None);
    register::<agents::AgentDirectory>(catalog, None);
    register::<AgentIndex>(catalog, None);
    register::<rutis_loader::Loader>(catalog, None);
    register::<collaboration::CollaborationDirectory>(catalog, None);
    register::<collaboration::Collaboration>(catalog, None);
    register::<ConnectionHub>(catalog, None);
    register::<ListenAddresses>(catalog, None);
    register::<ui_host::UiHost>(catalog, None);
    register::<settings::SettingsService>(catalog, None);
    register::<managed_provider::ManagedProviders>(catalog, None);
    register::<dyn managed_provider::ManagedProvider>(catalog, None);
    register::<codex_oauth::OpenAICodexOAuthService>(catalog, None);
    register::<grok_oauth::GrokOAuthService>(catalog, None);
    register::<provider::ProviderSecretHost>(catalog, None);
    register::<provider::ProviderService>(catalog, None);
    register::<ModelSelectionHost>(catalog, None);
    register::<AuxiliaryModelHost>(catalog, None);
    register::<SelectableModelsHost>(catalog, None);
    register::<mcp::McpService>(catalog, None);
    register::<mcp::McpAuth>(catalog, None);
    register::<mcp::Publication>(catalog, None);
    register::<hooks_settings::HooksService>(catalog, None);
    register::<methods::ManagementMethods>(catalog, None);
    register::<modality::ModalityAdapterService>(catalog, None);
    register::<runtime_manager::RuntimeService>(catalog, None);
    register::<collaboration::CollaborationSession>(catalog, Some(SESSION_GROUP));
    register::<mcp::McpSession>(catalog, Some(SESSION_GROUP));
    for name in [
        "pluginFiles",
        "sessions",
        "sessionMetadata",
        "auxiliaryModel",
        "settings",
    ] {
        catalog.register_shared(name);
    }
}

/// The capabilities' catalog with the App Server's names, for mounting its
/// parts outside [`serve`] ([`dim_agent_runtime::names::process_plugin`]).
pub fn catalog() -> dim_agent_runtime::names::ServiceCatalog {
    let mut catalog = dim_agent_capabilities::catalog();
    register_names(&mut catalog);
    catalog
}

/// Creates a directory that holds the native store, and any missing parents,
/// owner-only (0700) on Unix. On Windows they inherit the parent's DACL.
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(path)
}

/// Graceful shutdown: stop admission, settle Agent work, drain commands,
/// close connections, then release the process tree.
async fn coordinate(root: Ctx, control: Arc<ServerControl>) -> Result<ShutdownReason> {
    control.shutdown().cancelled().await;
    let hub = root.get::<ConnectionHub>();
    let rpc = hub.as_ref().and_then(|hub| hub.take_rpc());
    if let Some(hub) = &hub {
        hub.shutdown_requests.close();
        hub.shutdown_requests.wait().await;
    }
    if control.forced() {
        if let Some(hub) = hub {
            hub.close().await;
        }
        control.complete_shutdown(false).await;
        return Ok(ShutdownReason::Forced);
    }
    let mut errors = vec![];
    if let Some(providers) = root.get::<managed_provider::ManagedProviders>() {
        providers.quiesce().await;
    }
    if let Some(index) = root.get::<AgentIndex>() {
        for error in index.settle_all().await {
            errors.push(error);
        }
    }
    if let Some(rpc) = rpc {
        rpc.unloading.cancel();
        rpc.commands.close();
        rpc.commands.wait().await;
    }
    if let Some(hub) = hub {
        hub.close().await;
    }
    if let Err(error) = root.shutdown().await {
        errors.push(error.into());
    }
    let cleanup = AgentError::aggregate(errors);
    let result = if let Some(fault) = control.take_fault() {
        Err(match cleanup {
            Ok(()) => fault,
            Err(cleanup) => ServerError::Cleanup {
                cause: Box::new(fault),
                cleanup,
            },
        })
    } else {
        cleanup
            .map(|()| ShutdownReason::Graceful)
            .map_err(ServerError::from)
    };
    control.complete_shutdown(result.is_ok()).await;
    result
}
