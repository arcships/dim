mod agent_records;
mod agents;
mod approval;
pub mod codex_oauth;
pub mod collaboration;
mod connection;
mod control;
pub mod grok_oauth;
mod managed_oauth;
pub mod managed_provider;
pub mod mcp;
pub mod methods;
pub mod model_catalog;
mod outbound;
pub mod permission_rules;
pub mod plugin_files;
pub mod protocol;
pub mod provider;
pub mod session_typescript;
pub mod settings;
pub mod settings_methods;
mod skills_management;
pub mod storage;
mod transport;
mod tree_protocol;

use agents::{AgentIndex, AgentsPlugin};
use collaboration::CollaborationPlugin;
use connection::{ConnectionHub, ConnectionPlugin, RpcPlugin};
use control::{ServerControl, ServerControlPlugin};
use dim_agent_runtime::{
    Agent, AgentBuilder, AgentError, SessionConfig, register_capability, wait_active,
};
use rutis::{Ctx, FiberView};
use rutis_loader::RowsSource;
use std::{
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
    pub managed_plugin: Option<managed_provider::ManagedPluginConfig>,
    pub session_typescript: Option<session_typescript::SessionTypeScriptConfig>,
    pub daemon_profile: protocol::DaemonProfile,
    /// Admission is disabled until the product execution profile is installed.
    pub session_execution: bool,
    pub model_catalog: Option<Arc<dim_agent_aimux::AimuxModels>>,
    pub file_roots: Vec<PathBuf>,
    pub session_defaults: SessionConfig,
    pub hello_timeout: Duration,
    pub write_timeout: Duration,
    pub startup_timeout: Duration,
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
            managed_plugin: None,
            session_typescript: None,
            daemon_profile: protocol::DaemonProfile::Mock,
            session_execution: true,
            model_catalog: None,
            file_roots: vec![],
            session_defaults: SessionConfig::default(),
            hello_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(10),
            startup_timeout: Duration::from_secs(10),
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
        if plugin == "Agents"
            && self.config.session_execution
            && let Some(index) = self.root.get::<AgentIndex>()
            && let Err(error) = index.install_default().await
        {
            return Err(ServerError::Runtime(AgentError::Build(format!(
                "default Agent failed: {error}"
            ))));
        }
        // Dependents were evicted during the unload; settle their queued reloads.
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
    pub fn diagnostics(&self) -> rutis::RuntimeDiagnostics {
        self.root.diagnostics()
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
    runtime_rows: Vec<rutis::TypeKey>,
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
                    && entry.owner == rutis_loader::Owner::Layer(0)
            })
            .filter_map(|entry| entry.view);
        std::iter::once(self.view.clone()).chain(rows).collect()
    }

    async fn wait_active(&self, root: &Ctx, timeout: Duration) -> dim_agent_runtime::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // RuntimeRows refreshes offline JS declarations after Node starts.
            // New injections replace the row's fiber; a saved view can become
            // Disposed even though the current row is loading successfully.
            let views = self.views();
            let mut ready = true;
            for view in &views {
                let snapshot = view.watch().borrow().clone();
                match snapshot.state {
                    rutis::FiberState::Active => {}
                    rutis::FiberState::Failed => {
                        return Err(snapshot.error.map(AgentError::from).unwrap_or_else(|| {
                            AgentError::Build(format!("{}: Failed", view.name()))
                        }));
                    }
                    _ => ready = false,
                }
            }
            // RuntimeRows publishes its catalog after the process plugin's apply returns.
            let unpublished = self.runtime_rows.iter().find(|key| {
                root.get_as::<rutis_loader::RuntimeRows>((*key).clone())
                    .is_none()
            });
            ready &= unpublished.is_none();
            if ready {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                if let Some(key) = unpublished {
                    return Err(AgentError::Build(format!(
                        "runtime row declarations were not published: {key:?}"
                    )));
                }
                return wait_active(root, &views, Duration::ZERO, &CancellationToken::new()).await;
            }
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + Duration::from_millis(10)),
            )
            .await;
        }
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
            // rutis owns the shutdown once requested; the task only reports it.
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
/// and runtime declarations are published, and returns the listener addresses.
pub async fn serve(mut builder: AgentBuilder, mut config: ServerConfig) -> Result<RunningServer> {
    if config.managed_plugin.is_some() && config.product_home.is_none() {
        return Err(ServerError::Configuration(
            "managed plugins require a product home".into(),
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
    builder
        .session_plugin(agents::AgentsSessionPlugin::new)
        .session_methods(agents::SESSION_NAMESPACE);
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
    // Process plugins are rows of one loader, the only plugin on the root.
    // Each load builds a new plugin; state that outlives a generation is
    // shared through the plugin the factory clones.
    let mut builtins = rutis_loader::Builtins::new();
    let mut service_catalog = rutis_loader::ServiceCatalog::new();
    service_catalog.register_shared("managedProvider");
    service_catalog.register_shared("pluginFiles");
    service_catalog.register_shared("sessions");
    service_catalog.register_shared("sessionMetadata");
    service_catalog.register_shared("auxiliaryModel");
    service_catalog.register_shared("settings");
    let mut node_resolver = None;
    let mut private_row = None;
    let mut rows = vec![register_capability(&mut builtins, move || control.clone())];
    // Account-only hosts do not consume user execution configuration.
    let native_home = execution_root.filter(|_| config.database_path.is_some());
    let files =
        dim_agent_capabilities::file_storage::FileStoragePlugin::new(config.file_roots.clone());
    rows.push(register_capability(&mut builtins, move || files.clone()));
    let file_writes =
        dim_agent_capabilities::workspace_tools::FileWriteCoordinatorPlugin::default();
    rows.push(register_capability(&mut builtins, move || {
        file_writes.clone()
    }));
    if config.model_catalog.is_some() || product_root.is_some() {
        rows.push(register_capability(&mut builtins, || {
            dim_agent_aimux::AimuxPlugin
        }));
    }
    if let Some(models) = &config.model_catalog {
        let models = models.clone();
        rows.push(register_capability(&mut builtins, move || {
            dim_agent_aimux::ConfiguredModelsPlugin(models.clone())
        }));
    }
    if let (Some(path), Some(files_root)) = (&config.database_path, session_files_root) {
        let path = path.clone();
        rows.push(register_capability(&mut builtins, move || {
            storage::SqliteStoragePlugin::new(path.clone())
        }));
        rows.push(register_capability(&mut builtins, move || {
            storage::SessionStorePlugin::new(files_root)
        }));
    }
    // Each management namespace with the row of the owner that provides it;
    // Rpc reads the row's status to tell a reloading owner from an absent one.
    let mut owners: Vec<(String, String)> = Vec::new();
    let mut owner = |namespace: &str, row: String| {
        owners.push((namespace.to_owned(), row.clone()));
        row
    };
    if let Some(home_root) = product_root {
        rows.push(register_capability(&mut builtins, move || {
            plugin_files::PluginFilesPlugin(home_root)
        }));
        rows.push(owner(
            "account",
            register_capability(&mut builtins, || managed_provider::ManagedProvidersPlugin),
        ));
        if let Some(plugin) = &config.managed_plugin {
            let plugin = plugin.clone();
            let manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(plugin.plugin_dir.join("dim-plugin.json"))?)
                    .map_err(|_| {
                        ServerError::Configuration("invalid managed plugin manifest".into())
                    })?;
            if manifest["schemaVersion"] != 1
                || manifest["hostApiVersion"] != 2
                || manifest["rutisVersion"] != "0.10.0"
            {
                return Err(ServerError::Configuration(
                    "incompatible managed plugin manifest".into(),
                ));
            }
            let entry = manifest["entry"].as_str().ok_or_else(|| {
                ServerError::Configuration("managed plugin entry is missing".into())
            })?;
            let entry = plugin.plugin_dir.join(entry).canonicalize()?;
            if !entry.starts_with(plugin.plugin_dir.canonicalize()?) {
                return Err(ServerError::Configuration(
                    "managed plugin entry escapes its directory".into(),
                ));
            }
            if !plugin.node.is_absolute() || !plugin.js_runtime.is_absolute() {
                return Err(ServerError::Configuration(
                    "Node and JS runtime paths must be absolute".into(),
                ));
            }
            let runtime = rutis_bridge::runtime::LocalRuntime::node(
                &plugin.js_runtime,
                plugin.plugin_dir.join("package.json"),
            )
            .interpreter(plugin.node.clone())
            .stdin(rutis_bridge::runtime::Stdio::Null)
            .stdout(rutis_bridge::runtime::Stdio::Null)
            .stderr(rutis_bridge::runtime::Stdio::Null);
            let resolver = Arc::new(
                rutis_loader::RuntimeResolver::node(runtime.handle())
                    .with_catalog(&service_catalog),
            );
            let runtime = Arc::new(runtime);
            rows.push(register_capability(&mut builtins, move || {
                NodeRuntime(runtime.clone(), "NodeRuntime")
            }));
            let row_resolver = resolver.clone();
            rows.push(register_capability(&mut builtins, move || {
                NodeRows(row_resolver.clone(), "node", "NodeRows", false, false)
            }));
            rows.push(register_capability(&mut builtins, || {
                managed_provider::ManagedProviderBinding
            }));
            node_resolver = Some(resolver);
            private_row = Some(
                serde_json::json!({"id":"ManagedProviderPlugin","name":entry,"config":{"home":config.product_home}}),
            );
        }
        rows.push(owner(
            "account.codex",
            register_capability(&mut builtins, move || {
                codex_oauth::OpenAICodexOAuthPlugin::new(home_root)
            }),
        ));
        rows.push(owner(
            "account.grok",
            register_capability(&mut builtins, move || {
                grok_oauth::GrokOAuthPlugin::new(home_root)
            }),
        ));
        rows.push(owner(
            "provider",
            register_capability(&mut builtins, provider::ProviderPlugin::default),
        ));
    }
    let settings_sources = config
        .session_typescript
        .as_ref()
        .map(session_typescript::settings_sources)
        .transpose()?
        .unwrap_or_default();
    let settings_root = execution_root.or(product_root);
    if settings_root.is_none() && !settings_sources.is_empty() {
        return Err(ServerError::Configuration(
            "settings sources need a product or target home".into(),
        ));
    }
    if let Some(home_root) = settings_root {
        let settings = if native_home.is_some() {
            settings::SettingsPlugin::native(home_root, vec![])
        } else {
            settings::SettingsPlugin::new(home_root, vec![])
        }
        .reserve::<mcp::config::McpSettings>()
        .reserve::<mcp::config::McpProjectEnablement>()
        .reserve::<skills_management::SkillDocument>()
        .reserve::<permission_rules::PermissionSettings>()
        .declare(settings_sources);
        rows.push(register_capability(&mut builtins, move || settings.clone()));
        rows.push(owner(
            "settings",
            register_capability(&mut builtins, || settings_methods::SettingsMethodsPlugin),
        ));
    }
    if let Some(home_root) = execution_root {
        mcp::install(&mut builder);
        // Session files need SessionStore (native storage); `read` needs
        // local execution.
        if native_home.is_some() {
            dim_agent_attachments::install(&mut builder);
        }
        rows.push(mcp::McpPublication::register(&mut builtins));
        rows.push(owner(
            "mcp",
            register_capability(&mut builtins, move || mcp::McpPlugin::new(home_root)),
        ));
        rows.push(register_capability(&mut builtins, move || {
            dim_agent_capabilities::host_platform::HostPlatformPlugin::new(
                dim_agent_capabilities::host_platform::HostPlatformConfig {
                    exec_shell_file: Some((home_root, "exec-shell.json".into())),
                    isolation_file: Some((home_root, "host-isolation.json".into())),
                },
            )
        }));
        rows.push(owner(
            "skills",
            register_capability(&mut builtins, || skills_management::SkillsCatalogPlugin),
        ));
        rows.push(owner(
            "permission",
            register_capability(&mut builtins, || permission_rules::PermissionRulesPlugin),
        ));
    }
    let session_parts = config
        .session_typescript
        .as_ref()
        .map(|parts| {
            session_typescript::Parts::new(
                parts,
                &mut builder,
                &mut service_catalog,
                &owners,
                &config.file_roots,
            )
        })
        .transpose()?;
    if let Some(parts) = &session_parts {
        owners.extend(parts.management.clone());
        let runtime = parts.runtime.clone();
        rows.push(register_capability(&mut builtins, move || {
            NodeRuntime(runtime.clone(), "SessionNodeRuntime")
        }));
        let resolver = parts.rows_runtime.clone();
        let session_events = parts.session_events;
        rows.push(register_capability(&mut builtins, move || {
            NodeRows(
                resolver.clone(),
                "session-node",
                "SessionNodeRows",
                true,
                session_events,
            )
        }));
    }
    agents::register(&mut builtins);
    rows.push(register_capability(&mut builtins, || {
        agents::AgentDirectoryPlugin
    }));
    rows.push(register_capability(&mut builtins, || {
        collaboration::CollaborationDirectoryPlugin
    }));
    {
        let (builder, config, root) = (builder.clone(), config.clone(), root.clone());
        rows.push(register_capability(&mut builtins, move || {
            AgentsPlugin::new(builder.clone(), config.clone(), root.clone())
        }));
    }
    rows.push(register_capability(&mut builtins, CollaborationPlugin::new));
    {
        let config = config.clone();
        rows.push(register_capability(&mut builtins, move || {
            ConnectionPlugin::new(config.clone())
        }));
    }
    {
        let config = config.clone();
        let methods: Arc<[(String, String)]> = owners.into();
        let sessions: Arc<[String]> = builder.session_method_namespaces().into();
        let session_rows: Arc<[(String, String)]> = session_parts
            .as_ref()
            .map(|parts| parts.namespaces.clone())
            .unwrap_or_default()
            .into();
        let root = root.clone();
        rows.push(register_capability(&mut builtins, move || {
            RpcPlugin::new(
                config.clone(),
                methods.clone(),
                sessions.clone(),
                session_rows.clone(),
                root.clone(),
            )
        }));
    }
    {
        let config = config.clone();
        rows.push(register_capability(&mut builtins, move || {
            TransportPlugin::new(config.clone())
        }));
    }
    // Register every host module before allocating scoped module names.
    // Agents, Sessions and branches are instances of this same loader.
    let reserved_names = private_row
        .iter()
        .chain(session_parts.iter().flat_map(|parts| {
            parts
                .rows
                .iter()
                .chain(&parts.branch_rows)
                .chain(&parts.process_rows)
        }))
        .flat_map(|row| [&row["id"], &row["name"]])
        .filter_map(serde_json::Value::as_str);
    let mut session_row = match builder.install(&mut builtins, &root, reserved_names).await {
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
    if let Some(parts) = &session_parts {
        let session = session_row["config"]
            .as_array_mut()
            .expect("installed Session rows");
        // Branch parts join the existing nested branch group.
        session
            .iter_mut()
            .find(|row| row["id"] == dim_agent_runtime::BRANCH_GROUP)
            .and_then(|branch| branch["config"].as_array_mut())
            .expect("installed branch rows")
            .extend(parts.branch_rows.clone());
        session.extend(parts.rows.clone());
    }
    let mut rows: Vec<_> = rows
        .iter()
        .map(|name| serde_json::json!({ "id": name, "name": name }))
        .collect();
    rows.push(agents::group_row(session_row));
    if let Some(row) = private_row {
        rows.push(row);
    }
    let required = rows
        .iter()
        .map(|row| row["id"].as_str().expect("host row id").to_owned())
        .collect();
    if let Some(parts) = &session_parts {
        for row in parts
            .process_rows
            .iter()
            .chain(&parts.rows)
            .chain(&parts.branch_rows)
        {
            if rows
                .iter()
                .any(|host| row["id"] == host["id"] || row["name"] == host["name"])
            {
                return Err(ServerError::Configuration(
                    "TypeScript row collides with a host row".into(),
                ));
            }
        }
        rows.extend(parts.process_rows.clone());
    }
    let runtime_rows = session_parts
        .iter()
        .map(|parts| rutis_loader::RuntimeRows::key(parts.rows_runtime.runtime_name()))
        .chain(
            node_resolver
                .iter()
                .map(|resolver| rutis_loader::RuntimeRows::key(resolver.runtime_name())),
        )
        .collect();
    let mut resolver = rutis_loader::Chain::new().with(builtins);
    if let Some(parts) = session_parts {
        resolver = resolver.with_shared(parts.resolver);
    }
    if let Some(node) = node_resolver {
        resolver = resolver.with_shared(node);
    }
    let plugin = rutis_loader::LoaderPlugin::new(
        resolver,
        rutis_loader::LoaderOptions {
            catalog: service_catalog,
            ..Default::default()
        },
    );
    let loader = plugin.handle();
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
    // One deadline covers the loader, the reconcile and the rows: reconcile
    // waits while a row is Loading, so a hung `apply` must not outlive it.
    let deadline = tokio::time::Instant::now() + config.startup_timeout;
    let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());
    let started = tokio::time::timeout_at(deadline, async {
        wait_active(
            &root,
            std::slice::from_ref(&processes.view),
            remaining(),
            &CancellationToken::new(),
        )
        .await?;
        let reconcile = processes.loader.reconcile(vec![assembly.clone()], None);
        let report = match tokio::time::timeout_at(deadline, reconcile).await {
            Ok(report) => report.map_err(|error| AgentError::Build(error.to_string()))?,
            // The deadline has passed: this names the rows not yet Active.
            Err(_) => {
                wait_active(
                    &root,
                    &processes.views(),
                    Duration::ZERO,
                    &CancellationToken::new(),
                )
                .await?;
                return Err(AgentError::Build("process assembly timed out".into()));
            }
        };
        // A skipped row (a duplicate module name) would only show as a
        // startup timeout.
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
        // The report omits rows still Pending; the deadline names them.
        processes.wait_active(&root, remaining()).await
    })
    .await;
    let started = match started {
        Ok(result) => result,
        Err(_) => match wait_active(
            &root,
            &processes.views(),
            Duration::ZERO,
            &CancellationToken::new(),
        )
        .await
        {
            Err(error) => Err(error),
            Ok(()) => Err(AgentError::Build("process assembly timed out".into())),
        },
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
    // The default Agent loads once its provider is Active; a scope injects
    // the index, so it cannot load while `AgentsPlugin` itself is Loading.
    if config.session_execution
        && let Some(index) = root.get::<AgentIndex>()
        && let Err(error) = index.install_default().await
    {
        startup.armed = false;
        handle.shutdown().cancel();
        let cleanup = root.shutdown().await;
        let cause =
            ServerError::Runtime(AgentError::Build(format!("default Agent failed: {error}")));
        return Err(match cleanup {
            Ok(()) => cause,
            Err(cleanup) => ServerError::Cleanup {
                cause: Box::new(cause),
                cleanup: cleanup.into(),
            },
        });
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

/// A runtime's rows part; the flags select Settings and Session observation.
struct NodeRows(
    Arc<rutis_loader::RuntimeResolver>,
    &'static str,
    &'static str,
    bool,
    bool,
);
impl rutis::TypedPlugin for NodeRows {
    type Deps = (
        rutis::Keyed<rutis_bridge::runtime::Runtime>,
        Arc<rutis_loader::Loader>,
    );
    fn name(&self) -> &str {
        self.2
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (rutis::Keyed(runtime), _): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            if self.3 {
                session_typescript::forward_settings_events(ctx, runtime.process().clone())?;
            }
            if self.4 {
                session_typescript::forward_session_events(ctx, runtime.process().clone())?;
            }
            rutis::Plugin::apply(&rutis_loader::RuntimeRowsPlugin::new(self.0.clone()), ctx).await
        })
    }
}
impl dim_agent_runtime::Capability for NodeRows {
    fn keys(&self) -> dim_agent_runtime::KeysOf<Self> {
        (rutis::DepKey::dynamic(self.1), ())
    }
}

/// Graceful shutdown: stop admission, settle Agent work, drain commands,
/// close connections, then release the process tree.
async fn coordinate(root: Ctx, control: Arc<ServerControl>) -> Result<ShutdownReason> {
    control.shutdown().cancelled().await;
    let hub = root.get::<ConnectionHub>();
    // Taking RPC admission fences the shutdown-request tracker. Every accepted
    // graceful request installs its reply ID, and every force acknowledgement
    // settles, before we choose a mode or close the connection writers.
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

struct NodeRuntime(Arc<rutis_bridge::runtime::LocalRuntime>, &'static str);
impl rutis::TypedPlugin for NodeRuntime {
    type Deps = ();
    fn name(&self) -> &str {
        self.1
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): (),
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        rutis::Plugin::apply(self.0.as_ref(), ctx)
    }
}
impl dim_agent_runtime::Capability for NodeRuntime {
    fn keys(&self) -> dim_agent_runtime::KeysOf<Self> {}
}
