//! Rows of other languages (Node, Python). Each is a process row: its plugin
//! registers its Session and branch parts with `partRegistry` as plugin
//! definitions, and the row lists the names those parts provide in their
//! instances (`instanceNames`), which the catalog needs before the runtime
//! starts (rutis usage §2).
use crate::{Result, ServerError, methods::ManagementMethods, settings::WorkflowPackages};
use dim_agent_capabilities::{
    methods::SessionMethods, prompt_guide::GuideEntries, tool_registry::ToolRegistry,
};
use dim_agent_runtime::{AgentBuilder, BRANCH_GROUP, PartRegistry, SESSION_GROUP, names::name_of};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, Key, PluginFactory, TypeKey};
use rutis::{Ctx, Error};
use rutis_bridge::{
    PeerId, Session,
    pipe::Processes,
    runtime::{Launcher, Stdio},
    session::host_key,
};
use rutis_loader::resolve::Resolved;
use rutis_loader::runtime::SessionResolver;
use rutis_loader::{LoaderError, Resolver, ServiceCatalog};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    io::BufRead,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// The Session and branch services marked `#[rutis::service]`: provided in
/// their instance, each is served by name there.
fn crossing_instance_names() -> [(String, &'static str); 8] {
    [
        (
            name_of::<dim_agent_capabilities::timeline_tree::TreeCommands>(),
            SESSION_GROUP,
        ),
        (
            name_of::<dim_agent_runtime::SessionIdentity>(),
            SESSION_GROUP,
        ),
        (
            name_of::<dim_agent_runtime::SessionLifecycle>(),
            SESSION_GROUP,
        ),
        (name_of::<dim_agent_runtime::SessionConfig>(), SESSION_GROUP),
        (name_of::<dim_agent_runtime::Dispatcher>(), BRANCH_GROUP),
        (
            name_of::<dim_agent_runtime::BranchLifecycle>(),
            BRANCH_GROUP,
        ),
        (name_of::<dim_agent_runtime::Timeline>(), BRANCH_GROUP),
        (
            name_of::<dim_agent_runtime::TimelineIdentity>(),
            BRANCH_GROUP,
        ),
    ]
}

/// The names a host of foreign parts shares across languages beyond those
/// the catalog already shares: the marked Session and branch services, the
/// tool, method and guide-entry registries, with which a part of any
/// language registers its contributions, and the part registry, with which a
/// plugin registers its parts. The generated catalog (`xtask graph`) shares
/// them the same way.
pub fn share_names(catalog: &mut ServiceCatalog) {
    for (name, group) in crossing_instance_names() {
        catalog.register_shared_instance(name, group);
    }
    catalog
        .register_shared_instance(name_of::<ToolRegistry>(), SESSION_GROUP)
        .register_shared_instance(name_of::<SessionMethods>(), SESSION_GROUP)
        .register_shared_instance(name_of::<GuideEntries>(), SESSION_GROUP)
        .register_shared(name_of::<ManagementMethods>())
        .register_shared(name_of::<PartRegistry>());
}

/// The runtimes of other languages and their loader rows, each with a
/// `RowSchema.provides` snapshot for names needed before its runtime starts.
#[derive(Clone, Debug)]
pub struct SessionTypeScriptConfig {
    pub node: PathBuf,
    pub js_runtime: PathBuf,
    pub anchor: PathBuf,
    /// The Python runtime of rows named `py:<module>`, if any.
    pub python: Option<PythonConfig>,
    /// Set when RuntimeManager acquires the runtimes at these paths: the
    /// runtime processes start only once it has installed them.
    pub acquire: Option<Arc<crate::runtime_manager::Runtimes>>,
    /// Loader rows, each a process row. A row may list `instanceNames`, the
    /// names its plugin's parts provide in each Session or branch, as
    /// `{"name": "session" | "branch"}`.
    pub rows: Vec<Value>,
}

impl SessionTypeScriptConfig {
    /// The runtimes RuntimeManager acquires into `home` (RuntimeManager
    /// §2.3), with `rows`. The Python runtime imports plugins from
    /// `<home>/plugins/.python/` (§4).
    pub fn acquired(
        home: &std::path::Path,
        runtimes: Arc<crate::runtime_manager::Runtimes>,
        rows: Vec<Value>,
    ) -> Self {
        use crate::runtime_manager::Kind;
        let python = runtimes.plan(Kind::Python);
        Self {
            node: runtimes.plan(Kind::Node).interpreter.clone(),
            js_runtime: runtimes.js_runtime(),
            anchor: runtimes.anchor(),
            python: Some(PythonConfig {
                interpreter: python.interpreter.clone(),
                project: home.join("plugins").join(".python"),
                packages: Some(python.packages.clone()),
            }),
            acquire: Some(runtimes),
            rows,
        }
    }
}

/// Python 3.12 or later with the `rutis` package, running the modules of
/// `project`.
#[derive(Clone, Debug)]
pub struct PythonConfig {
    pub interpreter: PathBuf,
    pub project: PathBuf,
    /// A directory put ahead on the runtime's `PYTHONPATH`: the installed
    /// `rutis` package.
    pub packages: Option<PathBuf>,
}

/// How a local runtime's process starts: its command, given its channel,
/// `--id <row>` and `anchor` as its last arguments. Its connection row
/// injects it, so the row starts once it is provided and starts again when
/// it is provided again.
pub struct LaunchCommand {
    launcher: Launcher,
    anchor: PathBuf,
}

/// A local runtime: its connection row and the launch command that row
/// injects.
#[derive(Clone)]
pub(crate) struct ForeignRuntime {
    /// The connection row, which its plugin rows run on (`session-node`,
    /// `py`).
    pub row: &'static str,
    /// The Rust-only process key of its launch command.
    pub command: Key<LaunchCommand>,
    pub launch: Arc<LaunchCommand>,
}

pub(crate) struct Parts {
    pub node: ForeignRuntime,
    pub python: Option<ForeignRuntime>,
    snapshots: HashMap<String, Map<String, Value>>,
    file_roots: Arc<[PathBuf]>,
    /// The rows, without the host's fields.
    pub rows: Vec<Value>,
}

impl Parts {
    pub fn new(
        config: &SessionTypeScriptConfig,
        builder: &mut AgentBuilder,
        file_roots: &[PathBuf],
    ) -> Result<Self> {
        let invalid = |message: &str| ServerError::Configuration(message.into());
        if [&config.node, &config.js_runtime, &config.anchor]
            .into_iter()
            .chain(
                config
                    .python
                    .iter()
                    .flat_map(|python| [&python.interpreter, &python.project]),
            )
            .any(|path| !path.is_absolute())
        {
            return Err(invalid("plugin runtime paths must be absolute"));
        }
        let mut ids = HashSet::new();
        let mut snapshots = HashMap::new();
        let mut rows = Vec::new();
        share_names(builder.catalog_mut());
        for row in &config.rows {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| invalid("TypeScript row requires id"))?;
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| invalid("TypeScript row requires name"))?;
            if !ids.insert(id) {
                return Err(invalid("duplicate TypeScript row id"));
            }
            if name.starts_with("py:") && config.python.is_none() {
                return Err(invalid("a py: row needs the Python runtime"));
            }
            if !matches!(row.get("disabled"), None | Some(Value::Bool(_))) {
                return Err(invalid("row disabled must be boolean"));
            }
            // A plugin registers its parts (rutis usage R5): no row is placed
            // in a Session or branch.
            if row.get("group").is_some() {
                return Err(invalid(
                    "a plugin row is a process row and has no group: its plugin registers \
                     its Session and branch parts with partRegistry",
                ));
            }
            let instance_names = match row.get("instanceNames") {
                None => Map::new(),
                Some(Value::Object(names)) => names.clone(),
                Some(_) => return Err(invalid("instanceNames must be an object")),
            };
            for (service, group) in &instance_names {
                let group = match group {
                    Value::String(group) if [SESSION_GROUP, BRANCH_GROUP].contains(&&**group) => {
                        group.as_str()
                    }
                    _ => {
                        return Err(invalid(
                            "an instanceNames entry must be \"session\" or \"branch\"",
                        ));
                    }
                };
                if !matches!(
                    dim_agent_runtime::names::registered_key(builder.catalog(), service),
                    Err(LoaderError::UnknownService(_))
                ) {
                    return Err(ServerError::Configuration(format!(
                        "instanceNames of row {id}: {service} is already a service"
                    )));
                }
                builder
                    .catalog_mut()
                    .register_shared_instance(service, group);
            }
            let provides = row
                .get("provides")
                .and_then(Value::as_object)
                .ok_or_else(|| invalid("TypeScript row requires RowSchema.provides"))?;
            if snapshots
                .get(name)
                .is_some_and(|snapshot| snapshot != provides)
            {
                return Err(invalid(
                    "rows of one module must share the same provides snapshot",
                ));
            }
            for service in provides.keys() {
                // R6 registers contributions through their owner, not through
                // a host-side name. A row still naming an old family would
                // load with its contributions silently missing.
                if let Some((family, owner)) = [
                    ("toolContribution:", "toolRegistry"),
                    ("sessionMethods:", "sessionMethods"),
                    ("managementMethods:", "managementMethods"),
                ]
                .into_iter()
                .find(|(family, _)| service.starts_with(family))
                {
                    return Err(invalid(&format!(
                        "TypeScript row provides the removed {family} name {service}; register through {owner} instead"
                    )));
                }
                if let Err(LoaderError::UnknownService(_)) =
                    dim_agent_runtime::names::registered_key(builder.catalog(), service)
                {
                    builder.catalog_mut().register_shared(service);
                }
            }
            snapshots.insert(name.to_owned(), provides.clone());
            let mut row = row.clone();
            if let Value::Object(fields) = &mut row {
                fields.remove("instanceNames");
            }
            rows.push(row);
        }
        // The commands rutis-host's launchers use for these packages.
        let node = ForeignRuntime::node(&config.node, &config.js_runtime, config.anchor.clone());
        let python = config.python.as_ref().map(|python| {
            ForeignRuntime::python(
                &python.interpreter,
                python.packages.as_deref(),
                python.project.clone(),
            )
        });
        Ok(Self {
            node,
            python,
            snapshots,
            file_roots: file_roots.to_vec().into(),
            rows,
        })
    }
    /// The runtimes the rows run in: Node, then Python if configured.
    pub fn runtimes(&self) -> impl Iterator<Item = &ForeignRuntime> {
        std::iter::once(&self.node).chain(&self.python)
    }
    /// The resolver of the rows of both runtimes, for a loader with
    /// `catalog`, last in its chain, and Dim's wrapper around it.
    pub fn resolver(&self, catalog: &ServiceCatalog) -> (SessionResolver, Arc<SnapshotResolver>) {
        let sessions = SessionResolver::npm(catalog, NODE_ROW);
        let snapshots = Arc::new(SnapshotResolver {
            sessions: sessions.clone(),
            snapshots: self.snapshots.clone(),
            file_roots: self.file_roots.clone(),
        });
        (sessions, snapshots)
    }
}

/// The Node runtime's connection row; rows not named `<row>:<plugin>` run
/// on its session.
const NODE_ROW: &str = "session-node";
/// The Python runtime's connection row; rows `py:<module>` run on its
/// session.
const PYTHON_ROW: &str = "py";

/// How long a runtime's process may take to greet once started.
const GREETING: Duration = Duration::from_secs(30);

impl ForeignRuntime {
    fn new(row: &'static str, command: &'static str, launcher: Launcher, anchor: PathBuf) -> Self {
        // Its standard error reaches the process log, line by line (ADR 005).
        let launcher = launcher
            .inherit_fd()
            .stdin(Stdio::Null)
            .stdout(Stdio::Null)
            .stderr(log_lines(row));
        Self {
            row,
            command: Key::dynamic(command),
            launch: Arc::new(LaunchCommand { launcher, anchor }),
        }
    }
    /// Its connection row's plugin.
    pub fn connection(&self) -> RuntimeConnection {
        RuntimeConnection {
            row: self.row,
            command: self.command.clone(),
        }
    }

    /// The Node runtime's launch command: `node --import tsx <packages>/src/runner.mjs`,
    /// resolving packages from `anchor`.
    pub(crate) fn node(interpreter: &Path, js_runtime: &Path, anchor: PathBuf) -> Self {
        Self::new(
            NODE_ROW,
            "nodeRuntime",
            Launcher::new(interpreter)
                .arg("--import")
                .arg("tsx")
                .arg(js_runtime.join("src/runner.mjs"))
                .cwd(js_runtime),
            anchor,
        )
    }

    /// The Python runtime's launch command: `python -m rutis`, with the
    /// installed packages and the project on its `PYTHONPATH`.
    pub(crate) fn python(interpreter: &Path, packages: Option<&Path>, project: PathBuf) -> Self {
        let inherited = std::env::var_os("PYTHONPATH").filter(|path| !path.is_empty());
        let entries: Vec<PathBuf> = packages
            .iter()
            .map(|path| path.to_path_buf())
            .chain([project.clone()])
            .chain(inherited.iter().flat_map(std::env::split_paths))
            .collect();
        let path =
            std::env::join_paths(&entries).unwrap_or_else(|_| project.clone().into_os_string());
        Self::new(
            PYTHON_ROW,
            "pythonRuntime",
            Launcher::new(interpreter)
                .arg("-m")
                .arg("rutis")
                .env("PYTHONPATH", path)
                .env("PYTHONUNBUFFERED", "1")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .cwd(&project),
            project,
        )
    }
}

/// A process output that `tracing` receives line by line.
fn log_lines(runtime: &'static str) -> Stdio {
    Stdio::Pipe(Arc::new(move || {
        let (reader, writer) = std::io::pipe()?;
        std::thread::Builder::new()
            .name(format!("{runtime} stderr"))
            .spawn(move || {
                let mut reader = std::io::BufReader::new(reader);
                let mut line = Vec::new();
                while matches!(reader.read_until(b'\n', &mut line), Ok(read) if read > 0) {
                    let text = String::from_utf8_lossy(&line);
                    tracing::info!(runtime, "{}", text.trim_end());
                    line.clear();
                }
            })?;
        Ok(writer.into())
    }))
}

/// A local runtime's state, as its connection row keeps it, at
/// [`RuntimeStatus::key`] of the row while the row runs.
#[derive(Debug)]
pub struct RuntimeStatus(Mutex<RuntimeState>);

/// What a [`RuntimeStatus`] says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    /// Its process is starting, or greeting.
    Starting,
    /// Its session is there: its rows run.
    Ready,
    /// It did not start, or its process ended, and why: its rows wait.
    Down(String),
}

impl RuntimeStatus {
    /// Where the connection row `row` keeps its runtime's status.
    pub fn key(row: &str) -> Key<RuntimeStatus> {
        Key::dynamic(row.to_owned())
    }
    pub fn state(&self) -> RuntimeState {
        self.0.lock().unwrap().clone()
    }
    fn set(&self, state: RuntimeState) {
        *self.0.lock().unwrap() = state;
    }
}

/// A local runtime as a connection row: starts the runtime's process with
/// its launch command, mounts the session on the process's channel, and
/// keeps the runtime's [`RuntimeStatus`]. The row stays Active when the
/// runtime does not start or its process ends.
pub(crate) struct RuntimeConnection {
    row: &'static str,
    command: Key<LaunchCommand>,
}

impl DynPlugin for RuntimeConnection {
    fn name(&self) -> &str {
        self.row
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.command.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            let status = Arc::new(RuntimeStatus(Mutex::new(RuntimeState::Starting)));
            ctx.provide_at(RuntimeStatus::key(self.row), status.clone())?;
            let command = ctx.require_at(&self.command)?;
            let processes = Processes::default();
            let watching: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::default();
            {
                let (processes, watching) = (processes.clone(), watching.clone());
                ctx.effect(move || async move {
                    if let Some(watcher) = watching.lock().unwrap().take() {
                        watcher.abort();
                    }
                    processes.kill();
                    processes.ended().await;
                    Ok(())
                })?;
            }
            let down = |why: String| {
                status.set(RuntimeState::Down(why));
                Ok(Effect::Done)
            };
            let id = PeerId::new(self.row).expect("a runtime row is a peer id");
            let started = tokio::select! {
                started = command.launcher.start(&id, &command.anchor, &processes) => started,
                () = ctx.cancelled() => return Ok(Effect::Done),
            };
            let channel = match started {
                Ok(channel) => channel,
                Err(error) => return down(error.to_string()),
            };
            let greeted = tokio::select! {
                greeted = rutis_bridge::pipe::greeted(channel, GREETING) => greeted,
                () = ctx.cancelled() => return Ok(Effect::Done),
            };
            let channel = match greeted {
                Ok(channel) => channel,
                Err(why) => return down(why),
            };
            let session = ctx.plugin(Session::new(channel));
            let mut states = session.watch();
            loop {
                let snapshot = states.borrow_and_update().clone();
                match snapshot.state {
                    FiberState::Active => break,
                    FiberState::Failed | FiberState::Disposed => {
                        return down(snapshot.error.map_or_else(
                            || "the runtime ended as it started".into(),
                            |error| error.to_string(),
                        ));
                    }
                    _ => {}
                }
                tokio::select! {
                    changed = states.changed() => if changed.is_err() {
                        return down("the runtime ended as it started".into());
                    },
                    () = ctx.cancelled() => return Ok(Effect::Done),
                }
            }
            status.set(RuntimeState::Ready);
            let unloading = ctx.cancellation_token();
            let watcher = tokio::spawn(async move {
                let _ = states
                    .wait_for(|snapshot| {
                        matches!(snapshot.state, FiberState::Failed | FiberState::Disposed)
                    })
                    .await;
                if unloading.is_cancelled() {
                    return;
                }
                processes.ended().await;
                status.set(RuntimeState::Down(
                    processes
                        .status()
                        .unwrap_or_else(|| "the runtime's session ended".into()),
                ));
            });
            *watching.lock().unwrap() = Some(watcher);
            Ok(Effect::Done)
        })
    }
}

/// Dim's wrapper around the rows' resolver: it checks each row's
/// `provides` snapshot and hands `workflowFiles` rows their packages.
pub(crate) struct SnapshotResolver {
    sessions: SessionResolver,
    snapshots: HashMap<String, Map<String, Value>>,
    file_roots: Arc<[PathBuf]>,
}
impl Resolver for SnapshotResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, std::result::Result<Arc<Resolved>, LoaderError>> {
        Box::pin(async move {
            let snapshot = self.snapshots.get(name);
            let original = self.sessions.resolve(name).await?;
            match (snapshot, original.meta.get("provides")) {
                (None, _) => {}
                (Some(snapshot), Some(Value::Object(actual))) if actual == snapshot => {}
                // Resolved while its runtime is not running.
                (Some(_), None)
                    if original.schema.is_none()
                        && original.meta.get("schema").is_some_and(Value::is_string) => {}
                _ => {
                    return Err(LoaderError::Resolve {
                        name: name.into(),
                        message: "row provides differs from its structural declaration".into(),
                    });
                }
            }
            let bound = |inner| -> Arc<dyn PluginFactory<Value>> {
                Arc::new(BoundFactory {
                    inner,
                    file_roots: self.file_roots.clone(),
                })
            };
            let mut resolved = Resolved::new(bound(original.factory.clone()))
                .with_schema(original.schema.clone())
                .with_meta(original.meta.clone());
            if original.foreign_scope {
                resolved = resolved.with_foreign_scope();
            }
            if let Some(scoped) = original.scoped.clone() {
                let file_roots = self.file_roots.clone();
                resolved = resolved.with_scoped(Arc::new(move |build| {
                    let factory: Arc<dyn PluginFactory<Value>> = Arc::new(BoundFactory {
                        inner: scoped(build)?,
                        file_roots: file_roots.clone(),
                    });
                    Ok(factory)
                }));
            }
            if let Some(declares) = &original.declares {
                resolved = resolved.with_declarations(declares.clone());
            }
            Ok(Arc::new(resolved))
        })
    }
}

/// A row of another language: one that uses `workflowFiles` is handed its
/// own Workflow packages.
struct BoundFactory {
    inner: Arc<dyn PluginFactory<Value>>,
    file_roots: Arc<[PathBuf]>,
}
impl PluginFactory<Value> for BoundFactory {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.inner.injects()
    }
    fn validate_config(&self, config: &Value) -> std::result::Result<(), Error> {
        self.inner.validate_config(config)
    }
    fn build(&self, config: &Value) -> std::result::Result<Box<dyn DynPlugin>, Error> {
        let packages = self
            .injects()
            .contains(&host_key("workflowFiles").erase())
            .then(|| WorkflowPackages::from_config(config, &self.file_roots).map(Arc::new))
            .transpose()?;
        Ok(Box::new(BoundRow {
            inner: self.inner.build(config)?,
            packages,
        }))
    }
}

struct BoundRow {
    inner: Box<dyn DynPlugin>,
    packages: Option<Arc<WorkflowPackages>>,
}
impl DynPlugin for BoundRow {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.inner.injects()
    }
    fn validate(&self) -> std::result::Result<(), Error> {
        self.inner.validate()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            if let Some(packages) = &self.packages {
                ctx.provide_at(
                    Key::<WorkflowPackages>::of().instance(ctx.instance()),
                    packages.clone(),
                )?;
            }
            self.inner.apply(ctx).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Parts, SessionTypeScriptConfig};
    use dim_agent_runtime::{AgentBuilder, AgentConfig, names::name_of};
    use rutis_loader::resolve::Build;
    use rutis_loader::{LoaderError, ServiceCatalog};

    /// A Rust part depends on a service that crosses languages by its
    /// type's name: each marked Session or branch service is shared in the
    /// group the Rust binding registers it in, and the marked process
    /// services are shared under their types' names.
    #[test]
    fn crossing_services_are_named_and_grouped_as_the_rust_binding_names_them() {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        dim_agent_capabilities::register_names(&mut builder);
        let rust = builder.catalog().clone();
        super::share_names(builder.catalog_mut());
        for (name, group) in super::crossing_instance_names() {
            let grouped = |catalog: &ServiceCatalog| match catalog.key_in(&name, &Build::default())
            {
                Err(LoaderError::OutsideInstance { group, .. }) => Some(group),
                _ => None,
            };
            assert_eq!(grouped(&rust).as_deref(), Some(group), "{name}");
            assert_eq!(grouped(builder.catalog()).as_deref(), Some(group), "{name}");
            assert!(builder.catalog().is_shared(&name), "{name}");
        }
        for name in [
            name_of::<crate::agents::SessionPermissions>(),
            name_of::<crate::settings::WorkflowFiles>(),
        ] {
            assert!(builder.catalog().is_shared(&name), "{name}");
            assert!(builder.catalog().key(&name).is_some(), "{name}");
        }
    }

    /// A runtime whose launch command cannot start is down with the launch
    /// error, and its connection row stays Active (M4).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_launch_command_that_cannot_start_is_down_with_its_error() {
        use super::{ForeignRuntime, RuntimeState, RuntimeStatus};
        use rutis::host::CtxHost;
        let runtime = ForeignRuntime::new(
            "session-node",
            "nodeRuntime",
            rutis_bridge::runtime::Launcher::new("/nonexistent/node"),
            "/nonexistent/package.json".into(),
        );
        let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
        root.provide_at(runtime.command.clone(), runtime.launch.clone())
            .unwrap();
        let row = root.plugin(runtime.connection());
        (&row).await.unwrap();
        let status = root.get_at(&RuntimeStatus::key("session-node")).unwrap();
        assert!(
            matches!(status.state(), RuntimeState::Down(why) if why.contains("/nonexistent/node")),
            "{:?}",
            status.state()
        );
        assert_eq!(row.state().state, rutis::host::FiberState::Active);
        root.shutdown().await.unwrap();
    }

    /// R6 replaced the host-side contribution names. A row that still
    /// provides one must fail at configuration instead of losing the
    /// contribution silently.
    #[test]
    fn a_row_providing_a_removed_contribution_family_is_refused() {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        let config = SessionTypeScriptConfig {
            node: "/nonexistent/node".into(),
            js_runtime: "/nonexistent/runtime.js".into(),
            anchor: "/nonexistent/package.json".into(),
            python: None,
            acquire: None,
            rows: vec![serde_json::json!({
                "id": "legacy",
                "name": "/nonexistent/legacy.js",
                "provides": {"toolContribution:ask_user": {}},
            })],
        };
        let error = match Parts::new(&config, &mut builder, &[]) {
            Ok(_) => panic!("a removed contribution family must be refused"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("removed toolContribution:"),
            "{error}"
        );
    }
}
