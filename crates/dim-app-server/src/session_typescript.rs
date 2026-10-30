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
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, PluginFactory, TypeKey};
use rutis::{Ctx, Error};
use rutis_bridge::{
    runtime::{LocalRuntime, Stdio},
    session::host_key,
};
use rutis_loader::resolve::{Build, Resolved};
use rutis_loader::runtime::RuntimeResolver;
use rutis_loader::{LoaderError, Resolver, ServiceCatalog};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
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

/// Python 3.10 or later with the `rutis` package, running the modules of
/// `project`.
#[derive(Clone, Debug)]
pub struct PythonConfig {
    pub interpreter: PathBuf,
    pub project: PathBuf,
    /// A directory put ahead on the runtime's `PYTHONPATH`: the installed
    /// `rutis` package.
    pub packages: Option<PathBuf>,
}

/// A runtime of other languages and the resolver of its rows.
pub(crate) struct ForeignRuntime {
    /// Its name, which its rows gate on.
    pub name: &'static str,
    pub runtime: Arc<LocalRuntime>,
    pub rows: Arc<RuntimeResolver>,
}

pub(crate) struct Parts {
    pub node: ForeignRuntime,
    pub python: Option<ForeignRuntime>,
    pub resolver: Arc<SessionResolver>,
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
                    builder.catalog().key_in(service, &Build::default()),
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
                    builder.catalog().key_in(service, &Build::default())
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
        let node = LocalRuntime::node(&config.js_runtime, &config.anchor)
            .interpreter(config.node.clone())
            .named("session-node");
        let node = ForeignRuntime::new("session-node", node, RuntimeResolver::node, builder);
        let python = config.python.as_ref().map(|python| {
            let mut runtime =
                LocalRuntime::python(&python.project).interpreter(python.interpreter.clone());
            if let Some(packages) = &python.packages {
                runtime = runtime.python_path(packages);
            }
            ForeignRuntime::new("py", runtime, RuntimeResolver::modules, builder)
        });
        let resolver = Arc::new(SessionResolver {
            node: node.rows.clone(),
            python: python.as_ref().map(|python| python.rows.clone()),
            snapshots,
            file_roots: file_roots.to_vec().into(),
        });
        Ok(Self {
            node,
            python,
            resolver,
            rows,
        })
    }
    /// The runtimes the rows run in: Node, then Python if configured.
    pub fn runtimes(&self) -> impl Iterator<Item = &ForeignRuntime> {
        std::iter::once(&self.node).chain(&self.python)
    }
}

impl ForeignRuntime {
    fn new(
        name: &'static str,
        runtime: LocalRuntime,
        rows: impl FnOnce(rutis_bridge::runtime::RuntimeHandle) -> RuntimeResolver,
        builder: &AgentBuilder,
    ) -> Self {
        // Its standard error reaches the process log, line by line (ADR 005).
        let runtime = Arc::new(
            runtime
                .stdin(Stdio::Null)
                .stdout(Stdio::Null)
                .stderr(Stdio::Lines),
        );
        let rows = Arc::new(rows(runtime.handle()).with_catalog(builder.catalog()));
        Self {
            name,
            runtime,
            rows,
        }
    }
}

pub(crate) struct SessionResolver {
    node: Arc<RuntimeResolver>,
    python: Option<Arc<RuntimeResolver>>,
    snapshots: HashMap<String, Map<String, Value>>,
    file_roots: Arc<[PathBuf]>,
}
impl Resolver for SessionResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, std::result::Result<Arc<Resolved>, LoaderError>> {
        Box::pin(async move {
            let snapshot = self.snapshots.get(name);
            let runtime = match &self.python {
                Some(python) if name.starts_with("py:") => python,
                _ => &self.node,
            };
            let original = runtime.resolve(name).await?;
            match (snapshot, original.meta.get("provides")) {
                (None, _) => {}
                (Some(snapshot), Some(Value::Object(actual))) if actual == snapshot => {}
                (Some(_), None)
                    if original.meta["source"] == "runtime"
                        && original.meta["schema"]
                            == "unavailable: the Cordis runtime is not running" => {}
                _ => {
                    return Err(LoaderError::Resolve {
                        name: name.into(),
                        message: "row provides differs from its structural declaration".into(),
                    });
                }
            }
            let mut resolved = Resolved::new(Arc::new(BoundFactory {
                inner: original.factory.clone(),
                file_roots: self.file_roots.clone(),
            }))
            .with_schema(original.schema.clone())
            .with_meta(original.meta.clone());
            if original.foreign_scope {
                resolved = resolved.with_foreign_scope();
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
