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
pub mod model_catalog;
mod outbound;
pub mod plugin_files;
pub mod protocol;
pub mod provider;
pub mod settings;
pub mod storage;
mod trace;
mod transport;
mod tree_protocol;
mod web_entry;

use agents::{AgentIndex, AgentsPlugin};
use collaboration::CollaborationPlugin;
use connection::{ConnectionHub, ConnectionPlugin, RpcPlugin};
use control::{ServerControl, ServerControlPlugin};
use dim_agent_runtime::{
    Agent, AgentBuilder, AgentError, SessionConfig, register_capability, wait_active,
};
use rutis::{Ctx, FiberView};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use transport::{ListenAddresses, TransportPlugin};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub unix_path: Option<PathBuf>,
    pub pipe_name: Option<String>,
    pub websocket_address: Option<SocketAddr>,
    pub trace_directory: PathBuf,
    pub database_path: Option<PathBuf>,
    pub target_home: Option<PathBuf>,
    pub product_home: Option<PathBuf>,
    pub managed_plugin: Option<managed_provider::ManagedPluginConfig>,
    pub daemon_profile: protocol::DaemonProfile,
    /// Admission is disabled until the product execution profile is installed.
    pub session_execution: bool,
    pub model_catalog: Option<Arc<dim_agent_aimux::AimuxModels>>,
    pub file_roots: Vec<PathBuf>,
    pub session_defaults: SessionConfig,
    pub hello_timeout: Duration,
    pub write_timeout: Duration,
    pub startup_timeout: Duration,
    /// Built Web workspace assets served by the local web entry.
    pub web_assets: Option<PathBuf>,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            unix_path: None,
            pipe_name: None,
            websocket_address: Some(([127, 0, 0, 1], 0).into()),
            trace_directory: std::env::temp_dir().join("dim-agent-traces"),
            database_path: None,
            target_home: None,
            product_home: None,
            managed_plugin: None,
            daemon_profile: protocol::DaemonProfile::Mock,
            session_execution: true,
            model_catalog: None,
            file_roots: vec![],
            session_defaults: SessionConfig::default(),
            hello_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(10),
            startup_timeout: Duration::from_secs(10),
            web_assets: None,
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
}
impl Processes {
    /// The loader and the fiber of every top-level row (the process plugins).
    fn views(&self) -> Vec<FiberView> {
        let rows = self
            .loader
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.parent.is_none()
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
            if ready {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
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

/// Builds the process tree, opens the readiness gate once every process
/// plugin is Active, and returns the bound listener addresses.
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
    trace::install(&mut builder);
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
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(home)?;
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
        std::fs::create_dir_all(&home)?;
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
    let mut node_resolver = None;
    let mut private_row = None;
    let mut rows = vec![register_capability(&mut builtins, move || control.clone())];
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
    if let Some(path) = &config.database_path {
        let path = path.clone();
        rows.push(register_capability(&mut builtins, move || {
            storage::SqliteStoragePlugin::new(path.clone())
        }));
        rows.push(register_capability(&mut builtins, || {
            storage::SessionStorePlugin
        }));
    }
    if let Some(home_root) = product_root {
        rows.push(register_capability(&mut builtins, || {
            managed_provider::ManagedProvidersPlugin
        }));
        if let Some(plugin) = &config.managed_plugin {
            rows.push(register_capability(&mut builtins, move || {
                plugin_files::PluginFilesPlugin(home_root)
            }));
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
                NodeRuntime(runtime.clone())
            }));
            let row_resolver = resolver.clone();
            rows.push(register_capability(&mut builtins, move || {
                NodeRows(row_resolver.clone())
            }));
            rows.push(register_capability(&mut builtins, || {
                managed_provider::ManagedProviderBinding
            }));
            node_resolver = Some(resolver);
            private_row = Some(
                serde_json::json!({"id":"ManagedProviderPlugin","name":entry,"config":{"home":config.product_home}}),
            );
        }
        rows.push(register_capability(&mut builtins, move || {
            codex_oauth::OpenAICodexOAuthPlugin::new(home_root)
        }));
        rows.push(register_capability(&mut builtins, move || {
            grok_oauth::GrokOAuthPlugin::new(home_root)
        }));
        rows.push(register_capability(
            &mut builtins,
            provider::ProviderPlugin::default,
        ));
    }
    if let Some(home_root) = execution_root.or(product_root) {
        let settings = settings::SettingsPlugin::native(home_root, vec![]);
        rows.push(register_capability(&mut builtins, move || settings.clone()));
    }
    let mut publication = None;
    if let Some(home_root) = execution_root {
        mcp::install(&mut builder);
        let (publisher, row) = mcp::McpPublication::register(&mut builtins);
        publication = Some(publisher);
        rows.push(row);
        rows.push(register_capability(&mut builtins, || mcp::McpPlugin));
        rows.push(register_capability(&mut builtins, move || {
            dim_agent_capabilities::host_platform::HostPlatformPlugin::new(
                dim_agent_capabilities::host_platform::HostPlatformConfig {
                    exec_shell_file: Some((home_root, "exec-shell.json".into())),
                },
            )
        }));
        rows.push(register_capability(
            &mut builtins,
            dim_agent_capabilities::skills::SkillsCatalogPlugin::new,
        ));
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
        rows.push(register_capability(&mut builtins, move || {
            RpcPlugin::new(config.clone())
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
        .flat_map(|row| [&row["id"], &row["name"]])
        .filter_map(serde_json::Value::as_str);
    let session_row = match builder.install(&mut builtins, &root, reserved_names).await {
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
    rows.push(agents::group_row(session_row));
    if let Some(row) = private_row {
        rows.push(row);
    }
    let mut resolver = rutis_loader::Chain::new().with(builtins);
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
    let started = async {
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
        if !report.failures.is_empty() || !report.issues.is_empty() {
            return Err(AgentError::Build(format!(
                "process assembly: {:?} {:?}",
                report.failures, report.issues
            )));
        }
        // The report omits rows still Pending; the deadline names them.
        processes.wait_active(&root, remaining()).await
    }
    .await;
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
    let publication = publication.map(|publisher| (publisher, processes.loader.clone(), assembly));
    let task = tokio::spawn(coordinate(root.clone(), handle.clone(), publication));
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

struct NodeRows(Arc<rutis_loader::RuntimeResolver>);
impl rutis::TypedPlugin for NodeRows {
    type Deps = (
        rutis::Keyed<rutis_bridge::runtime::Runtime>,
        Arc<rutis_loader::Loader>,
    );
    fn name(&self) -> &str {
        "NodeRows"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        _: Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            rutis::Plugin::apply(&rutis_loader::RuntimeRowsPlugin::new(self.0.clone()), ctx).await
        })
    }
}
impl dim_agent_runtime::Capability for NodeRows {
    fn keys(&self) -> dim_agent_runtime::KeysOf<Self> {
        (rutis::DepKey::dynamic("node"), ())
    }
}

/// Graceful shutdown: stop admission, settle Agent work, drain commands and
/// trace writers, close connections, then release the process tree.
async fn coordinate(
    root: Ctx,
    control: Arc<ServerControl>,
    publication: Option<(
        mcp::McpPublication,
        rutis_loader::Loader,
        rutis_loader::Layer,
    )>,
) -> Result<ShutdownReason> {
    let publication = publication.map(|(publisher, loader, assembly)| {
        tokio::spawn(publisher.run(root.clone(), loader, assembly, control.shutdown().clone()))
    });
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
    // One generation throughout: its archives finish after its Sessions close.
    let index = root.get::<AgentIndex>();
    if let Some(index) = &index {
        for error in index.settle_all().await {
            errors.push(error);
        }
    }
    if let Some(rpc) = rpc {
        rpc.unloading.cancel();
        rpc.commands.close();
        rpc.commands.wait().await;
    }
    if let Some(index) = index {
        index.finish_traces().await;
    }
    if let Some(hub) = hub {
        hub.close().await;
    }
    if let Some(publication) = publication
        && publication.await.is_err()
    {
        errors.push(AgentError::Build(
            "MCP configuration publisher failed".into(),
        ));
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

struct NodeRuntime(Arc<rutis_bridge::runtime::LocalRuntime>);
impl rutis::TypedPlugin for NodeRuntime {
    type Deps = ();
    fn name(&self) -> &str {
        "NodeRuntime"
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
