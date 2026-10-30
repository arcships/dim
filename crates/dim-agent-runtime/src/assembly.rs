use crate::core::OpeningCount;
use crate::*;
use futures_util::FutureExt;
use rutis::host::{
    BoxFuture as RutisFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberState,
    FiberStatusChanged, FiberView, Key, PluginFactory, PluginId, TypeKey,
};
use rutis::{Ctx, Error};
use rutis_loader::edit::{CreateInstance, InstanceResult};
use rutis_loader::report::EntryInfo;
use rutis_loader::resolve::Build;
use rutis_loader::resolve::{Resolved, ScopedFactory};
use rutis_loader::{Builtins, Loader, LoaderError, Resolver};
use serde_json::{Value, json};
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, Weak},
};
use tokio_util::sync::CancellationToken;

/// Instanced loader group of one Agent; its rows are the host's.
pub const AGENT_GROUP: &str = "agent";
/// Instanced loader group of one Session, nested in [`AGENT_GROUP`].
pub const SESSION_GROUP: &str = "session";
/// Instanced loader group of one branch, nested in [`SESSION_GROUP`].
pub const BRANCH_GROUP: &str = "branch";

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct OpeningAdvanced(pub PluginId);
impl rutis::Event for OpeningAdvanced {
    const NAME: &'static str = "dim/opening-advanced";
    type Value = ();
}

/// Bounds an opening by inactivity in its own fiber subtrees, including rows
/// created while the opening is in flight. Other scopes cannot extend it.
pub struct StartupProgress {
    ctx: Ctx,
    started: tokio::time::Instant,
    timeout: Duration,
    active: Arc<Mutex<HashMap<PluginId, tokio::time::Instant>>>,
    listeners: Vec<rutis::Disposer>,
}

impl StartupProgress {
    pub fn new(ctx: &Ctx, timeout: Duration) -> Result<Self> {
        let active = Arc::new(Mutex::new(HashMap::new()));
        let changes = active.clone();
        let started = tokio::time::Instant::now();
        let listener = ctx.on_sync::<FiberStatusChanged>(move |event, _| {
            if event.to == FiberState::Active {
                changes
                    .lock()
                    .unwrap()
                    .entry(event.plugin_id)
                    .or_insert_with(tokio::time::Instant::now);
            }
            Ok(None)
        })?;
        let mut progress = Self {
            ctx: ctx.clone(),
            started,
            timeout,
            active,
            listeners: vec![listener],
        };
        let changes = progress.active.clone();
        progress
            .listeners
            .push(ctx.on_sync::<OpeningAdvanced>(move |event, _| {
                changes
                    .lock()
                    .unwrap()
                    .insert(event.0, tokio::time::Instant::now());
                Ok(None)
            })?);
        Ok(progress)
    }

    /// Returns `None` after no first activation, opening dispatch or receipt
    /// completion in `roots` for the timeout.
    /// The root identities are read when the inactivity interval expires.
    pub async fn wait<F: std::future::Future>(
        &mut self,
        future: F,
        roots: impl Fn() -> Vec<PluginId>,
    ) -> Option<F::Output> {
        tokio::pin!(future);
        let mut deadline = self.started + self.timeout;
        loop {
            tokio::select! {
                biased;
                result = &mut future => return Some(result),
                _ = tokio::time::sleep_until(deadline) => {}
            }
            let snapshot = self.ctx.diagnostics();
            let members: HashSet<_> = roots()
                .into_iter()
                .flat_map(|id| subtree(&snapshot, id))
                .collect();
            let last = self
                .active
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| members.contains(id))
                .map(|(_, time)| *time)
                .max()
                .unwrap_or(self.started);
            deadline = last + self.timeout;
            if deadline <= tokio::time::Instant::now() {
                return None;
            }
        }
    }
}

impl Drop for StartupProgress {
    fn drop(&mut self) {
        let listeners = std::mem::take(&mut self.listeners);
        if !listeners.is_empty() {
            let sink = self.ctx.error_sink();
            self.ctx.handle().spawn(async move {
                for listener in listeners {
                    if let Err(error) = listener.dispose().await {
                        sink(error);
                    }
                }
            });
        }
    }
}

/// A Rust plugin whose dependencies are names ([`Names`]), built for one copy.
type PartMake =
    Arc<dyn Fn(&Names<'_>) -> std::result::Result<Box<dyn DynPlugin>, Error> + Send + Sync>;
/// An extension capability: its plugin name and how a copy reads its value.
/// Reads an extension's value from where its copy runs.
type ExtensionMake = Arc<
    dyn Fn(&Build, &Arc<rutis_loader::ServiceCatalog>) -> std::result::Result<ExtensionPart, Error>
        + Send
        + Sync,
>;
/// Builds one plugin of an extension copy.
type ExtensionPart = Box<dyn Fn() -> std::result::Result<Box<dyn DynPlugin>, Error> + Send + Sync>;
type Provide = Arc<dyn Fn(&Ctx, &SessionScope) -> std::result::Result<(), Error> + Send + Sync>;
/// A value registered with [`AgentBuilder::process_value`]: its name, and
/// how a copy provides it at the key the name has there.
#[derive(Clone)]
struct ProcessValue {
    name: String,
    bind: BindValue,
}
type BindValue = Arc<dyn Fn(&Names<'_>) -> std::result::Result<ProvideValue, Error> + Send + Sync>;
type ProvideValue = Box<dyn Fn(&Ctx) -> std::result::Result<(), Error> + Send + Sync>;
/// The module of the part providing an Agent's process values.
const PROCESS_VALUES: &str = "ProcessValues";
#[derive(Clone)]
pub(crate) struct Assembly {
    pub config: AgentConfig,
    /// Maximum inactivity interval while opening a Session or branch.
    pub startup_timeout: Duration,
    pub store: Arc<dyn TimelineStore>,
    pub ephemeral_store: Option<Arc<MemoryTimelineStore>>,
    /// This Agent's branch openings that are alive.
    pub openings: OpeningCount,
    values: Vec<Provide>,
    /// The names of `values`, each a Session name.
    value_names: Vec<String>,
    process_values: Vec<ProcessValue>,
    /// The process plugins the builder adds ([`AgentBuilder::plugin`]).
    plugins: Vec<PartMake>,
    /// Where the plugin of each feature is in `plugins`, by name.
    features: std::collections::HashMap<String, usize>,
    /// The parts plugins register: the builder's for a host that places
    /// its Agents (`build_at`), a fresh one in each Agent placing itself.
    pub parts: Arc<crate::parts::PartRegistry>,
    /// Registered parts the profile requires, by name.
    required_parts: Arc<HashSet<String>>,
    /// Nested instanced groups created per Session on request.
    extensions: Vec<(String, Vec<(String, ExtensionMake)>)>,
    /// The builder's catalog, as its Agents were built: the facades read a
    /// name in the group it registers.
    pub catalog: Arc<rutis_loader::ServiceCatalog>,
}
#[derive(Clone)]
pub struct AgentBuilder {
    assembly: Assembly,
    custom_store: bool,
    /// The assembly's service names and their groups ([`names`](crate::names)).
    catalog: rutis_loader::ServiceCatalog,
}
impl AgentBuilder {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            custom_store: false,
            catalog: crate::names::catalog(),
            assembly: Assembly {
                config,
                startup_timeout: Duration::from_secs(10),
                store: Arc::new(MemoryTimelineStore::default()),
                ephemeral_store: None,
                openings: OpeningCount::default(),
                values: vec![],
                value_names: vec![],
                process_values: vec![],
                plugins: vec![],
                features: Default::default(),
                parts: Arc::default(),
                required_parts: Arc::default(),
                extensions: vec![],
                catalog: Arc::default(),
            },
        }
    }
    /// Uses one store for every Session built by this Agent.
    pub fn timeline_store(&mut self, store: Arc<dyn TimelineStore>) -> &mut Self {
        self.assembly.store = store;
        self.custom_store = true;
        self
    }
    /// Maximum interval without first activation, opening dispatch advancement
    /// or receipt completion in an opening scope.
    pub fn startup_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.assembly.startup_timeout = timeout;
        self
    }
    /// The assembly's service names: hosts register theirs before adding
    /// parts that depend on them, and their loader uses this catalog.
    pub fn catalog(&self) -> &rutis_loader::ServiceCatalog {
        &self.catalog
    }
    pub fn catalog_mut(&mut self) -> &mut rutis_loader::ServiceCatalog {
        &mut self.catalog
    }
    /// The registry where plugins register Session and branch parts
    /// ([`PartRegistry::register`](crate::PartRegistry::register)) in a host
    /// that places its Agents ([`Self::build_at`]): every such Agent mounts
    /// from it, and [`Self::plugin_rows`] provides it to the host's process
    /// plugins as `partRegistry`. An Agent placing itself ([`Self::build`],
    /// [`Self::build_in`]) has a registry of its own, where only its own
    /// plugins register.
    pub fn parts(&self) -> Arc<crate::PartRegistry> {
        self.assembly.parts.clone()
    }
    /// Adds a process plugin: `make` builds it from the names its
    /// dependencies resolve to at the process, usually
    /// `|names| Ok(Box::new(MyPlugin::new(names)?))`. An Agent placing itself mounts it in
    /// its own loader; a host placing its Agents places it with
    /// [`Self::plugin_rows`]. It reaches the registry by the process name
    /// `partRegistry`.
    pub fn plugin(
        &mut self,
        make: impl Fn(&Names<'_>) -> std::result::Result<Box<dyn DynPlugin>, Error>
        + Send
        + Sync
        + 'static,
    ) -> &mut Self {
        self.assembly.plugins.push(Arc::new(make));
        self
    }
    /// Adds the plugin of `feature` ([`Self::plugin`]). A feature of the
    /// same name added before is replaced.
    pub fn feature(&mut self, feature: crate::Feature) -> &mut Self {
        let name = feature.name().to_owned();
        let make: PartMake = Arc::new(move |names| feature.plugin(names));
        match self.assembly.features.get(&name) {
            Some(&index) => self.assembly.plugins[index] = make,
            None => {
                self.assembly
                    .features
                    .insert(name, self.assembly.plugins.len());
                self.assembly.plugins.push(make);
            }
        }
        self
    }
    /// Adds `feature` ([`Self::feature`]) and makes each of its parts part
    /// of the profile ([`Self::require_part`]): what an install function of
    /// the profile does for the features it installs.
    pub fn require_feature(&mut self, feature: crate::Feature) -> &mut Self {
        for name in feature.part_names() {
            Arc::make_mut(&mut self.assembly.required_parts).insert(name.to_owned());
        }
        self.feature(feature)
    }
    /// Registers in `builtins` the module providing [`Self::parts`] at
    /// `partRegistry` and those of the plugins added with [`Self::plugin`],
    /// and returns their rows, for a host that places its Agents. Register
    /// them before [`Self::install`]; a module of the same name registered
    /// already fails.
    pub async fn plugin_rows(&self, builtins: &mut Modules) -> Result<Vec<Value>> {
        let registry = self.assembly.parts.clone();
        let registry: PartMake = Arc::new(move |names| {
            Ok(Box::new(crate::parts::PartRegistryPlugin {
                registry: registry.clone(),
                key: names.key()?,
            }))
        });
        let mut rows = vec![];
        for make in std::iter::once(&registry).chain(&self.assembly.plugins) {
            let name = make(&Names::check(&self.catalog, &[], None))
                .map_err(|error| AgentError::Build(format!("a process plugin: {error}")))?
                .name()
                .to_owned();
            if builtins.contains(&name).await {
                return Err(AgentError::Build(format!(
                    "module {name} is registered already"
                )));
            }
            let make = make.clone();
            register_part(builtins, &self.catalog, move |names| make(names))?;
            rows.push(row(&name));
        }
        Ok(rows)
    }
    /// Makes the registered part `name` part of the profile: a Session or
    /// branch publishes only once its copy is Active, and a copy that fails
    /// fails the instance. Other registered parts are optional.
    pub fn require_part(&mut self, name: impl Into<String>) -> &mut Self {
        Arc::make_mut(&mut self.assembly.required_parts).insert(name.into());
        self
    }
    /// A Rust part of the Session extension `group`: a nested instanced
    /// group created only for Sessions that ask for it, with a value of type
    /// `V` (see [`Agent::create_session_with_extension`]). `make` builds a
    /// copy from the names its dependencies resolve to there and the
    /// extension's value. The extension loads alongside the Session's parts
    /// after its scope is ready, and every part must be Active before main
    /// initialization. `name` is the plugin's name, which its fiber is
    /// registered under before any value exists; a plugin with another name
    /// fails to load. Its names are checked when a copy is built, since a copy
    /// needs the value.
    pub fn session_extension_part<V: Send + Sync + 'static>(
        &mut self,
        group: impl Into<String>,
        name: impl Into<String>,
        make: impl Fn(&Names<'_>, &V) -> std::result::Result<Box<dyn DynPlugin>, Error>
        + Send
        + Sync
        + 'static,
    ) -> &mut Self {
        let make = Arc::new(make);
        let make: ExtensionMake = Arc::new(move |build, catalog| {
            let value = extension_value::<V>(build)?;
            let (make, catalog, build) = (make.clone(), catalog.clone(), build.clone());
            Ok(Box::new(move || {
                make(&Names::copy(&catalog, &build), &value)
            }))
        });
        self.extension(group.into(), name.into(), make)
    }
    fn extension(&mut self, group: String, name: String, make: ExtensionMake) -> &mut Self {
        match self
            .assembly
            .extensions
            .iter_mut()
            .find(|(g, _)| *g == group)
        {
            Some((_, makes)) => makes.push((name, make)),
            None => self.assembly.extensions.push((group, vec![(name, make)])),
        }
        self
    }
    /// Provides a host-configured value under its Session name in every
    /// Session; the Session scope provides it before any part loads. The
    /// catalog must register the name in the Session group; installing the
    /// assembly fails otherwise.
    pub fn session_value<T: ?Sized + Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
        self.assembly.value_names.push(crate::names::name_of::<T>());
        self.assembly.values.push(Arc::new(move |ctx, scope| {
            ctx.provide_at::<T>(scope.key::<T>(), value.clone())?;
            Ok(())
        }));
        self
    }
    /// Provides a host-configured value under its process name to this
    /// Agent's Sessions: each Agent instance provides it at the name's
    /// process key, in a scope of its own ([`Self::agent_row`]). An Agent
    /// hosting itself ([`Self::build`], [`Self::build_in`]) places that row;
    /// under [`Self::build_at`] the host does. The name must be registered
    /// at the process.
    pub fn process_value<T: ?Sized + Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
        self.assembly.process_values.push(ProcessValue {
            name: crate::names::name_of::<T>(),
            bind: Arc::new(move |names| {
                let (key, value) = (names.key::<T>()?, value.clone());
                Ok(Box::new(move |ctx: &Ctx| {
                    ctx.provide_at(key.clone(), value.clone())?;
                    Ok(())
                }))
            }),
        });
        self
    }
    /// The [`AGENT_GROUP`] row a host places in its loader, holding `rows`:
    /// the host's Agent plugin and the Session row [`Self::install`]
    /// returned. With values registered with [`Self::process_value`] it also
    /// holds their provider and isolates their names, so each Agent instance
    /// has them in a scope of its own: Agents sharing a host do not collide,
    /// and the host's own providers of those names stay the host's. The
    /// loader's catalog must be this builder's.
    pub fn agent_row(&self, rows: impl IntoIterator<Item = Value>) -> Value {
        let values = &self.assembly.process_values;
        let provider = (!values.is_empty()).then(|| row(PROCESS_VALUES));
        let mut agent = group(AGENT_GROUP, provider.into_iter().chain(rows).collect());
        if !values.is_empty() {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let label = Value::String(format!("dim-agent-values/{number}"));
            agent["isolate"] = values
                .iter()
                .map(|value| (value.name.clone(), label.clone()))
                .collect::<serde_json::Map<_, _>>()
                .into();
        }
        agent
    }
    /// Registers the loader modules of this assembly in `builtins`: the
    /// Session and branch scopes, their part mounters and every extension,
    /// and returns the [`SESSION_GROUP`] row holding them and the nested
    /// [`BRANCH_GROUP`]; a host puts it inside its [`AGENT_GROUP`] row.
    /// These rows are required for readiness. Additional host rows remain
    /// owned by the group without being required to become Active. The
    /// builder's plugins are placed separately ([`Self::plugin_rows`]).
    ///
    /// Register host modules first: names already in `builtins` or used by
    /// another row get a `#n` suffix. Extension group names cannot be renamed.
    /// Pass host identities absent from `builtins` in `reserved_names`: both
    /// module names resolved by another runtime and row IDs that differ from
    /// their module names.
    /// Construction and metadata panics are returned as build errors.
    pub async fn install<'a>(
        &self,
        builtins: &mut Modules,
        reserved_names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Value> {
        AssertUnwindSafe(async {
            let assembly = &self.assembly;
            let mut modules = ModuleNames(HashSet::from_iter(
                [
                    AGENT_GROUP,
                    SESSION_GROUP,
                    BRANCH_GROUP,
                    "AgentAnchor",
                    "AgentScope",
                    PROCESS_VALUES,
                ]
                .map(str::to_owned),
            ));
            modules
                .0
                .extend(reserved_names.into_iter().map(str::to_owned));
            for (name, _) in &assembly.extensions {
                if !modules.0.insert(name.clone()) || builtins.contains(name).await {
                    return Err(AgentError::Build(format!(
                        "reserved extension group: {name}"
                    )));
                }
            }
            let session_scope_module = modules.name("SessionScope", builtins).await;
            let branch_scope_module = modules.name("BranchScope", builtins).await;
            let mut session = vec![row(&session_scope_module)];
            if !assembly.extensions.is_empty() {
                let module = modules.name("SessionExtension", builtins).await;
                session.push(row(&module));
                builtins.register(module, |build| {
                    let owner = build.value::<Arc<SessionAssembly>>().ok_or_else(|| {
                        rutis::Error::from(ErrorKind::PluginFailed(
                            "Session extension created without its assembly".into(),
                        ))
                    })?;
                    let scope = session_scope(build)?;
                    Factory::new(move || {
                        Box::new(SessionExtensionPlugin {
                            owner: (*owner).clone(),
                            identity: Key::of().instance(scope.instance),
                        }) as Box<dyn DynPlugin>
                    })
                });
            }
            let catalog = Arc::new(self.catalog.clone());
            for name in &assembly.value_names {
                crate::names::named_in_group(&catalog, name, SESSION_GROUP)
                    .map_err(|error| AgentError::Build(format!("a Session value: {error}")))?;
            }
            if !assembly.process_values.is_empty() {
                register_process_values(&assembly.process_values, &catalog, builtins).await?;
            }
            let [session_parts, session_optional] = mounters(
                crate::PartScope::Session,
                assembly,
                &catalog,
                &mut modules,
                builtins,
            )
            .await;
            session.push(row(&session_parts));
            let mut branch = vec![row(&branch_scope_module)];
            let [branch_parts, branch_optional] = mounters(
                crate::PartScope::Branch,
                assembly,
                &catalog,
                &mut modules,
                builtins,
            )
            .await;
            branch.push(row(&branch_parts));
            let required_branch: Arc<[String]> = branch
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned())
                .collect();
            let mut required_session: Vec<String> = session
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned())
                .collect();
            branch.push(row(&branch_optional));
            session.push(row(&session_optional));
            session.push(group(BRANCH_GROUP, branch));
            for (name, makes) in &assembly.extensions {
                let mut rows = vec![];
                for (plugin, make) in makes {
                    let module = modules.name(plugin, builtins).await;
                    required_session.push(module.clone());
                    rows.push(row(&module));
                    let (make, plugin, catalog) = (make.clone(), plugin.clone(), catalog.clone());
                    builtins.register(module, move |build| {
                        let make = make(build, &catalog)?;
                        let factory = Factory::try_new(make)?;
                        if factory.name != plugin {
                            return Err(rutis::Error::from(ErrorKind::PluginFailed(
                                format!("extension plugin {} registered as {plugin}", factory.name)
                                    .into(),
                            )));
                        }
                        Ok(factory)
                    });
                }
                session.push(group(name, rows));
            }
            let required_session: Arc<[String]> = required_session.into();
            builtins.register(session_scope_module, move |build| {
                let owner = build.value::<Arc<SessionAssembly>>().ok_or_else(|| {
                    rutis::Error::from(ErrorKind::PluginFailed(
                        "Session created without its assembly".into(),
                    ))
                })?;
                let scope = session_scope(build)?;
                *owner.instance.lock().unwrap() = Some(scope.instance);
                let required = required_session.clone();
                Factory::new(move || {
                    Box::new(SessionScopePlugin(
                        (*owner).clone(),
                        scope,
                        required.clone(),
                    )) as Box<dyn DynPlugin>
                })
            });
            builtins.register(branch_scope_module, move |build| {
                let owner = build.value::<Arc<BranchAssembly>>().ok_or_else(|| {
                    rutis::Error::from(ErrorKind::PluginFailed(
                        "branch created without its assembly".into(),
                    ))
                })?;
                let scope = branch_scope(build)?;
                *owner.instance.lock().unwrap() = Some(scope.instance);
                let required = required_branch.clone();
                Factory::new(move || {
                    Box::new(BranchScopePlugin((*owner).clone(), scope, required.clone()))
                        as Box<dyn DynPlugin>
                })
            });
            Ok(group(SESSION_GROUP, session))
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(AgentError::Build("plugin registration panicked".into())))
    }
    /// Sessions own a private rutis root that the Agent shuts down on close.
    pub fn build(&self) -> Agent {
        Agent::new(self.clone(), self.finished(), AgentHost::Private)
    }
    /// Sessions are loaded under `host`, in a loader of their own; the host
    /// owns that context's lifetime.
    pub fn build_in(&self, host: &Ctx) -> Agent {
        Agent::new(
            self.clone(),
            self.finished(),
            AgentHost::Within(host.clone()),
        )
    }
    /// Sessions are instances of `loader`, created inside the Agent instance
    /// that `anchor` runs in: a running loader plugin of that instance,
    /// usually the host's Agent plugin. The loader must have this builder's
    /// modules ([`Self::install`]) and catalog, and its [`AGENT_GROUP`] row
    /// must be [`Self::agent_row`], which provides the process values.
    pub fn build_at(&self, loader: Loader, anchor: &Ctx) -> Agent {
        Agent::new(
            self.clone(),
            self.finished(),
            AgentHost::Placed(Placement {
                loader,
                anchor: anchor.clone(),
            }),
        )
    }
    fn finished(&self) -> Assembly {
        let mut assembly = self.assembly.clone();
        assembly.openings = OpeningCount::default();
        assembly.catalog = Arc::new(self.catalog.clone());
        if !self.custom_store {
            let store = Arc::new(MemoryTimelineStore::default());
            assembly.store = store.clone();
            assembly.ephemeral_store = Some(store);
        }
        assembly
    }
}

/// Configuration of the dim modules: they take none.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoConfig {}

/// A loader module built from a closure. The first plugin is built when the
/// factory is, to read the name and dependencies the fiber is registered
/// with; it is handed to the first load.
struct Factory {
    name: String,
    injects: Vec<TypeKey>,
    first: Mutex<Option<Box<dyn DynPlugin>>>,
    make: Box<dyn Fn() -> std::result::Result<Box<dyn DynPlugin>, Error> + Send + Sync>,
}
impl Factory {
    fn new(
        make: impl Fn() -> Box<dyn DynPlugin> + Send + Sync + 'static,
    ) -> std::result::Result<Self, Error> {
        Self::try_new(move || Ok(make()))
    }
    fn try_new(
        make: impl Fn() -> std::result::Result<Box<dyn DynPlugin>, Error> + Send + Sync + 'static,
    ) -> std::result::Result<Self, Error> {
        let make = Box::new(move || {
            catch_unwind(AssertUnwindSafe(&make)).map_err(|_| {
                rutis::Error::from(ErrorKind::PluginFailed("plugin factory panicked".into()))
            })?
        });
        let first = make()?;
        catch_unwind(AssertUnwindSafe(|| Self {
            name: first.name().to_owned(),
            injects: first.injects().to_vec(),
            first: Mutex::new(Some(first)),
            make,
        }))
        .map_err(|_| rutis::Error::from(ErrorKind::PluginFailed("plugin metadata panicked".into())))
    }
}
impl PluginFactory<Value> for Factory {
    fn name(&self) -> &str {
        &self.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.injects.to_vec()
    }
    fn validate_config(&self, config: &Value) -> std::result::Result<(), Error> {
        no_config(config)
    }
    fn build(&self, config: &Value) -> std::result::Result<Box<dyn DynPlugin>, Error> {
        no_config(config)?;
        match self.first.lock().unwrap().take() {
            Some(plugin) => Ok(plugin),
            None => (self.make)(),
        }
    }
}
/// A Dim module's row takes no configuration (`null` or `{}`), decoded as the
/// loader decodes a typed module's.
fn no_config(config: &Value) -> std::result::Result<(), Error> {
    let decoded = match config {
        Value::Null => serde_json::from_value::<NoConfig>(json!({})),
        config => serde_json::from_value::<NoConfig>(config.clone()),
    };
    decoded.map(drop).map_err(|error| {
        rutis::Error::from(ErrorKind::Validation {
            issues: vec![error.to_string()],
        })
    })
}
/// A module built per copy, outside any instance.
struct Unscoped(String);
impl PluginFactory<Value> for Unscoped {
    fn name(&self) -> &str {
        &self.0
    }
    fn build(&self, _: &Value) -> std::result::Result<Box<dyn DynPlugin>, Error> {
        Err(rutis::Error::from(ErrorKind::PluginFailed(
            format!("{} runs only inside an instanced group", self.0).into(),
        )))
    }
}

/// The loader modules of an assembly. A Rust part that depends on names is a
/// rutis `DynPlugin` whose keys depend on the instances its copy runs in, and
/// rutis's `Builtins` builds per copy only typed plugins, so Dim's modules are
/// resolved here, each copy's factory built from where it runs
/// (`Resolved::with_scoped`); `builtins` holds the host's typed modules and is
/// tried after them. Hand it to the loader as its resolver.
#[derive(Default)]
pub struct Modules {
    pub builtins: Builtins,
    scoped: std::collections::HashMap<String, Arc<Resolved>>,
}
impl Modules {
    pub fn new() -> Self {
        Self::default()
    }
    /// Whether `name` is registered here or in `builtins`.
    pub async fn contains(&self, name: &str) -> bool {
        self.scoped.contains_key(name) || self.builtins.resolve(name).await.is_ok()
    }
    /// Registers the module `name`: `build` makes each copy's factory from
    /// the instances it runs in.
    fn register(
        &mut self,
        name: impl Into<String>,
        build: impl Fn(&Build) -> std::result::Result<Factory, Error> + Send + Sync + 'static,
    ) {
        let name = name.into();
        let scoped: ScopedFactory = Arc::new(move |at: &Build| {
            let factory: Arc<dyn PluginFactory<Value>> = Arc::new(build(at)?);
            Ok(factory)
        });
        let resolved = Resolved::new(Arc::new(Unscoped(name.clone())))
            .with_schema(serde_json::to_value(schemars::schema_for!(NoConfig)).ok())
            .with_meta(json!({ "source": "builtin" }))
            .with_scoped(scoped);
        self.scoped.insert(name, Arc::new(resolved));
    }
}
impl Resolver for Modules {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> RutisFuture<'a, std::result::Result<Arc<Resolved>, LoaderError>> {
        match self.scoped.get(name) {
            Some(resolved) => {
                let resolved = resolved.clone();
                Box::pin(async move { Ok(resolved) })
            }
            None => self.builtins.resolve(name),
        }
    }
}

/// Reserved identities outside the modules, which own registered module names.
struct ModuleNames(HashSet<String>);
impl ModuleNames {
    async fn name(&mut self, plugin: &str, modules: &Modules) -> String {
        let mut name = plugin.to_owned();
        let mut suffix = 2;
        while self.0.contains(&name) || modules.contains(&name).await {
            name = format!("{plugin}#{suffix}");
            suffix += 1;
        }
        self.0.insert(name.clone());
        name
    }
}
/// Registers a scope's two mounters ([`crate::parts`]) and returns their
/// module names, the required one first.
async fn mounters(
    scope: crate::PartScope,
    assembly: &Assembly,
    catalog: &Arc<rutis_loader::ServiceCatalog>,
    modules: &mut ModuleNames,
    builtins: &mut Modules,
) -> [String; 2] {
    let mut names = [String::new(), String::new()];
    for (slot, required) in names.iter_mut().zip([true, false]) {
        let module = modules
            .name(crate::parts::PartMounter::module(scope, required), builtins)
            .await;
        let (registry, wanted, catalog) = (
            assembly.parts.clone(),
            assembly.required_parts.clone(),
            catalog.clone(),
        );
        builtins.register(module.clone(), move |build| {
            let (session, branch_id, mounted, copies) = match scope {
                crate::PartScope::Session => {
                    let session = (*build.value::<Arc<SessionAssembly>>().ok_or_else(|| {
                        rutis::Error::from(ErrorKind::PluginFailed(
                            "Session created without its assembly".into(),
                        ))
                    })?)
                    .clone();
                    let (mounted, copies) = (session.mounted.clone(), session.copies.clone());
                    (session, None, mounted, copies)
                }
                crate::PartScope::Branch => {
                    let branch = build.value::<Arc<BranchAssembly>>().ok_or_else(|| {
                        rutis::Error::from(ErrorKind::PluginFailed(
                            "branch created without its assembly".into(),
                        ))
                    })?;
                    (
                        branch.session.clone(),
                        Some(branch.id.clone()),
                        branch.mounted.clone(),
                        branch.copies.clone(),
                    )
                }
            };
            let site = Arc::new(crate::parts::MountSite {
                scope,
                registry: registry.clone(),
                required: wanted.clone(),
                catalog: catalog.clone(),
                build: build.clone(),
                session: session.config.clone(),
                session_id: session.id.clone(),
                restored: session.restored,
                branch_id,
                mounted,
                copies,
            });
            Factory::new(move || {
                Box::new(crate::parts::PartMounter {
                    site: site.clone(),
                    required,
                })
            })
        });
        *slot = module;
    }
    names
}
/// Registers the part providing `values` in an Agent instance, checking
/// their names first: each is a process name.
async fn register_process_values(
    values: &[ProcessValue],
    catalog: &Arc<rutis_loader::ServiceCatalog>,
    builtins: &mut Modules,
) -> Result<()> {
    if builtins.contains(PROCESS_VALUES).await {
        return Err(AgentError::Build(format!(
            "reserved module: {PROCESS_VALUES}"
        )));
    }
    let values: Arc<[ProcessValue]> = values.into();
    let make = Arc::new(move |names: &Names<'_>| {
        let provides = values
            .iter()
            .map(|value| (value.bind)(names))
            .collect::<std::result::Result<_, _>>()?;
        Ok(Box::new(ProcessValuesPlugin(provides)) as Box<dyn DynPlugin>)
    });
    make(&Names::check(catalog, &[], None))
        .map_err(|error| AgentError::Build(format!("a process value: {error}")))?;
    let catalog = catalog.clone();
    builtins.register(PROCESS_VALUES, move |build| {
        let (make, catalog, build) = (make.clone(), catalog.clone(), build.clone());
        Factory::try_new(move || make(&Names::copy(&catalog, &build)))
    });
    Ok(())
}
/// Provides an Agent's process values in its Agent instance.
struct ProcessValuesPlugin(Vec<ProvideValue>);
impl DynPlugin for ProcessValuesPlugin {
    fn name(&self) -> &str {
        PROCESS_VALUES
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> RutisFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            for provide in &self.0 {
                provide(ctx)?;
            }
            Ok(Effect::Done)
        })
    }
}
/// Registers a process plugin as a loader module named after it and returns
/// the name. `make` must be pure: registration builds one plugin to read its
/// name, and each load builds another; capture shared configuration in
/// `make` when it must survive a reload, and acquire I/O only in `apply`. Its
/// names are checked here: a Session or branch name fails. Register the
/// names it depends on first.
pub fn register_part(
    builtins: &mut Modules,
    catalog: &rutis_loader::ServiceCatalog,
    make: impl Fn(&Names<'_>) -> std::result::Result<Box<dyn DynPlugin>, Error> + Send + Sync + 'static,
) -> Result<String> {
    let sample = make(&Names::check(catalog, &[], None))
        .map_err(|error| AgentError::Build(format!("a process part: {error}")))?;
    let name = sample.name().to_owned();
    let (make, catalog) = (Arc::new(make), Arc::new(catalog.clone()));
    builtins.register(name.clone(), move |build| {
        let (make, catalog, build) = (make.clone(), catalog.clone(), build.clone());
        Factory::try_new(move || make(&Names::copy(&catalog, &build)))
    });
    Ok(name)
}
fn extension_value<V: Send + Sync + 'static>(build: &Build) -> std::result::Result<Arc<V>, Error> {
    build.value::<V>().ok_or_else(|| {
        rutis::Error::from(ErrorKind::PluginFailed(
            "Session extension created without its value".into(),
        ))
    })
}
/// A row named after its module.
fn row(module: &str) -> Value {
    json!({ "id": module, "name": module })
}
fn group(id: &str, config: Vec<Value>) -> Value {
    json!({ "id": id, "group": true, "instanced": true, "config": config })
}
fn session_scope(build: &Build) -> std::result::Result<SessionScope, Error> {
    Ok(SessionScope {
        instance: build.instance(SESSION_GROUP)?,
    })
}
fn branch_scope(build: &Build) -> std::result::Result<BranchScope, Error> {
    Ok(BranchScope {
        session: session_scope(build)?,
        instance: build.instance(BRANCH_GROUP)?,
    })
}

/// Where an Agent's Sessions are created.
pub(crate) enum AgentHost {
    /// A private root and loader, created with the first Session.
    Private,
    /// A loader of the Agent's own under the host's context.
    Within(Ctx),
    /// The host's loader, inside the host's Agent instance.
    Placed(Placement),
}
/// A loader and a running plugin of the Agent instance Sessions go in.
#[derive(Clone)]
pub(crate) struct Placement {
    pub loader: Loader,
    pub anchor: Ctx,
}

/// What an Agent created for itself to host its Sessions.
pub(crate) struct Owned {
    /// The private root, when the Agent made one.
    pub root: Option<Ctx>,
    /// The Agent instance, then its loader.
    pub views: Vec<FiberView>,
}

/// Mounts a loader of the Agent's own under `host` (a private root when
/// `None`) and creates the Agent instance its Sessions go in. Under a host,
/// the loader's service is isolated so several Agents can share the host.
pub(crate) async fn own_placement(
    builder: &AgentBuilder,
    host: Option<&Ctx>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(Placement, Owned)> {
    let mut builder = builder.clone();
    builder.assembly.parts = Arc::default();
    let builder = &builder;
    let (root, parent) = match host {
        None => {
            let root = Ctx::root_with_sink(
                tokio::runtime::Handle::current(),
                Arc::new(|error| tracing::error!(%error, "Agent runtime error")),
            );
            (Some(root.clone()), root)
        }
        Some(host) => {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let label = format!("dim-agent/{number}");
            (
                None,
                host.isolate(TypeKey::of::<Loader>(), &label)
                    .isolate(TypeKey::of::<crate::PartRegistry>(), &label)
                    .isolate(rutis_bridge::runtime::mounter_key().erase(), &label),
            )
        }
    };
    let mut builtins = Modules::new();
    builtins.register("AgentAnchor", |build| {
        let slot = build.value::<AnchorSlot>().ok_or_else(|| {
            rutis::Error::from(ErrorKind::PluginFailed(
                "Agent created without its slot".into(),
            ))
        })?;
        Factory::new(move || Box::new(AgentAnchor((*slot).clone())) as Box<dyn DynPlugin>)
    });
    let installed = async {
        let plugins = match builder.assembly.plugins.is_empty() {
            true => vec![],
            false => builder.plugin_rows(&mut builtins).await?,
        };
        Ok::<_, AgentError>((plugins, builder.install(&mut builtins, []).await?))
    };
    let (plugin_rows, session_row) = match installed.await {
        Ok(rows) => rows,
        Err(error) => {
            if let Some(root) = root {
                root.shutdown().await.map_err(AgentError::from)?;
            }
            return Err(error);
        }
    };
    let plugin = rutis_loader::LoaderPlugin::new(
        builtins,
        rutis_loader::LoaderOptions {
            catalog: builder.catalog().clone(),
            ..Default::default()
        },
    );
    let loader = plugin.handle();
    let view = parent.plugin(plugin);
    let mut owned = Owned {
        root,
        views: vec![view.clone()],
    };
    let placed = async {
        wait_active(&parent, std::slice::from_ref(&view), timeout, cancel).await?;
        let mut inserted = plugin_rows;
        inserted.push(builder.agent_row([row("AgentAnchor"), session_row]));
        let rows = json!([{ "insert": inserted }]);
        let rows = serde_json::from_value(rows).map_err(|e| AgentError::Build(e.to_string()))?;
        let report = loader
            .reconcile(vec![rutis_loader::Layer::new("assembly", rows)], None)
            .await
            .map_err(loader_error)?;
        if !report.failures.is_empty() || !report.issues.is_empty() {
            return Err(AgentError::Build(format!(
                "{:?} {:?}",
                report.failures, report.issues
            )));
        }
        let slot = AnchorSlot::default();
        let agent = loader
            .create_instance(&parent, AGENT_GROUP)
            .with(slot.clone())
            .await
            .map_err(loader_error)?;
        owned.views.insert(0, agent.view);
        let anchor = slot
            .0
            .get()
            .cloned()
            .ok_or_else(|| AgentError::Build("Agent anchor did not load".into()))?;
        Ok(anchor)
    }
    .await;
    match placed {
        Ok(anchor) => Ok((Placement { loader, anchor }, owned)),
        Err(error) => Err(match release(owned).await {
            Ok(()) => error,
            Err(cleanup) => error.with_cleanup(cleanup),
        }),
    }
}
/// Closes what [`own_placement`] created.
pub(crate) async fn release(owned: Owned) -> Result<()> {
    let mut errors = vec![];
    match owned.root {
        Some(root) => errors.extend(root.shutdown().await.err().map(AgentError::from)),
        None => {
            for view in owned.views {
                errors.extend(view.shutdown().await.err().map(AgentError::from));
            }
        }
    }
    AgentError::aggregate(errors)
}
#[derive(Clone, Default)]
struct AnchorSlot(Arc<std::sync::OnceLock<Ctx>>);
/// The Agent instance's plugin when the Agent hosts itself: Sessions are
/// created from its context.
struct AgentAnchor(AnchorSlot);
impl DynPlugin for AgentAnchor {
    fn name(&self) -> &str {
        "AgentAnchor"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> RutisFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            self.0.0.set(ctx.clone()).map_err(|_| {
                rutis::Error::from(ErrorKind::PluginFailed("Agent anchor reloaded".into()))
            })?;
            Ok(Effect::Done)
        })
    }
}

/// A loaded scope plugin: its own context and the capability plugins it installed.
pub(crate) struct Loaded<S> {
    pub ctx: Ctx,
    pub scope: S,
    pub children: Vec<FiberView>,
    pub(crate) required: Arc<[String]>,
    /// The parts the required mounter mounted when the scope opened, which
    /// creation waited for; the assembly check reports one that has left.
    pub(crate) parts: Vec<FiberView>,
}

pub(crate) struct SessionAssembly {
    pub assembly: Assembly,
    extension: Mutex<ExtensionRequest>,
    /// The Session instance, once the loader spawned its rows.
    pub instance: Mutex<Option<rutis::host::InstanceId>>,
    pub loader: Loader,
    pub config: SessionConfig,
    pub id: String,
    pub restored: bool,
    pub unloading: CancellationToken,
    pub faults: FaultReporter,
    pub closing: CancellationToken,
    pub lifecycle: SessionLifecycle,
    pub loaded: Mutex<Option<Arc<Loaded<SessionScope>>>>,
    /// The parts the Session's mounters mounted when it opened, until its
    /// creation ends.
    pub(crate) mounted: Arc<Mutex<crate::parts::Opened>>,
    pub(crate) copies: crate::parts::Copies,
}
impl SessionAssembly {
    pub(crate) fn loaded(&self) -> Result<Arc<Loaded<SessionScope>>> {
        self.loaded
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AgentError::Closed(self.id.clone()))
    }
}

struct SessionScopePlugin(Arc<SessionAssembly>, SessionScope, Arc<[String]>);
impl DynPlugin for SessionScopePlugin {
    fn name(&self) -> &str {
        "SessionScope"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> RutisFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            let (owner, scope) = (&self.0, self.1);
            ctx.provide_at(
                scope.key::<crate::SessionLoader>(),
                Arc::new(crate::SessionLoader::new(owner.loader.clone(), ctx.clone())),
            )?;
            ctx.provide_at(scope.key::<SessionConfig>(), Arc::new(owner.config.clone()))?;
            ctx.provide_at(
                scope.key::<AgentConfig>(),
                Arc::new(owner.assembly.config.clone()),
            )?;
            ctx.provide_at(
                scope.key::<SessionIdentity>(),
                Arc::new(SessionIdentity {
                    session_id: owner.id.clone(),
                    restored: owner.restored,
                    unloading: owner.unloading.clone(),
                }),
            )?;
            ctx.provide_at(
                scope.key::<SessionLifecycle>(),
                Arc::new(owner.lifecycle.clone()),
            )?;
            ctx.provide_at(
                scope.key::<TimelineFactory>(),
                Arc::new(TimelineFactory(Arc::downgrade(owner))),
            )?;
            ctx.provide_at::<dyn TimelineStore>(
                scope.key::<dyn TimelineStore>(),
                owner.assembly.store.clone(),
            )?;
            ctx.provide_at(scope.key::<FaultReporter>(), Arc::new(owner.faults.clone()))?;
            for provide in &owner.assembly.values {
                provide(ctx, &scope)?;
            }
            let mut loaded = owner.loaded.lock().unwrap();
            if loaded.is_some() {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    "Session scope reloaded".into(),
                )));
            }
            *loaded = Some(Arc::new(Loaded {
                ctx: ctx.clone(),
                scope,
                children: vec![],
                required: self.2.clone(),
                parts: vec![],
            }));
            Ok(Effect::Done)
        })
    }
}

/// Structural assembly runs once, just like SessionScope itself. The nested
/// instance belongs to the Session group, not to this launcher fiber.
enum ExtensionRequest {
    Pending(Option<Extension>),
    Started,
}
struct SessionExtensionPlugin {
    owner: Arc<SessionAssembly>,
    /// Gated on the Session's identity, which its scope provides once the
    /// Session is assembled.
    identity: Key<SessionIdentity>,
}
impl DynPlugin for SessionExtensionPlugin {
    fn name(&self) -> &str {
        "SessionExtension"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> RutisFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            let request = std::mem::replace(
                &mut *self.owner.extension.lock().unwrap(),
                ExtensionRequest::Started,
            );
            let extension = match request {
                ExtensionRequest::Pending(Some(extension)) => extension,
                ExtensionRequest::Pending(None) => return Ok(Effect::Done),
                ExtensionRequest::Started => {
                    return Err(rutis::Error::from(ErrorKind::PluginFailed(
                        "Session extension launcher reloaded".into(),
                    )));
                }
            };
            let cancel = ctx.cancellation_token();
            let created = tokio::select! {
                created = (extension.configure)(self.owner.loader.create_instance(ctx, &extension.group)) => created
                    .map_err(|error| ErrorKind::PluginFailed(loader_error(error).into()))?,
                _ = cancel.cancelled() => return Ok(Effect::Done),
            };
            let failures = created
                .report
                .into_iter()
                .filter_map(|(_, result)| match result {
                    InstanceResult::Failed(error) => Some(loader_error(error)),
                    _ => None,
                })
                .collect();
            AgentError::aggregate(failures)
                .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Clone)]
pub struct TimelineFactory(Weak<SessionAssembly>);

/// Separates a rejected candidate from a failure to release its resources.
pub struct TimelineBuildError {
    pub cause: AgentError,
    pub cleanup: Option<AgentError>,
}
impl From<AgentError> for TimelineBuildError {
    fn from(cause: AgentError) -> Self {
        Self {
            cause,
            cleanup: None,
        }
    }
}
impl From<TimelineBuildError> for AgentError {
    fn from(error: TimelineBuildError) -> Self {
        match error.cleanup {
            Some(cleanup) => error.cause.with_cleanup(cleanup),
            None => error.cause,
        }
    }
}

pub(crate) struct BranchAssembly {
    session: Arc<SessionAssembly>,
    /// The branch instance, once the loader spawned its rows.
    instance: Mutex<Option<rutis::host::InstanceId>>,
    id: String,
    ancestors: Mutex<Option<Vec<(String, u64)>>>,
    sequence: u64,
    loaded: Mutex<Option<Arc<LoadedBranch>>>,
    /// The parts the branch's mounters mounted when it opened, until its
    /// creation ends.
    mounted: Arc<Mutex<crate::parts::Opened>>,
    copies: crate::parts::Copies,
}
struct LoadedBranch {
    scope: Loaded<BranchScope>,
    timeline: Timeline,
    dispatcher: Dispatcher,
    lifecycle: BranchLifecycle,
    opening: Arc<BranchOpening>,
}
struct BranchScopePlugin(Arc<BranchAssembly>, BranchScope, Arc<[String]>);
impl DynPlugin for BranchScopePlugin {
    fn name(&self) -> &str {
        "BranchScope"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> RutisFuture<'a, std::result::Result<Effect, Error>> {
        Box::pin(async move {
            let (owner, scope) = (&self.0, self.1);
            let session = &owner.session;
            let ancestors = owner.ancestors.lock().unwrap().take().ok_or_else(|| {
                rutis::Error::from(ErrorKind::PluginFailed("Branch scope reloaded".into()))
            })?;
            let timeline = Timeline::branch(
                scope.instance,
                ctx.clone(),
                TimelineKey {
                    session_id: session.id.clone(),
                    branch_id: owner.id.clone(),
                },
                ancestors,
                session.assembly.store.clone(),
                owner.sequence,
                session.faults.clone(),
            );
            let dispatcher = Dispatcher::new(scope.instance, owner.sequence, timeline.clone());
            ctx.provide_at(scope.key::<Timeline>(), Arc::new(timeline.clone()))?;
            ctx.provide_at(scope.key::<Dispatcher>(), Arc::new(dispatcher.clone()))?;
            let lifecycle = BranchLifecycle::new();
            timeline.begin_opening(&lifecycle, &session.assembly.openings);
            ctx.provide_at(scope.key::<BranchLifecycle>(), Arc::new(lifecycle.clone()))?;
            let opening = Arc::new(BranchOpening::new());
            ctx.provide_at(scope.key::<BranchOpening>(), opening.clone())?;
            ctx.provide_at(
                scope.key::<TimelineIdentity>(),
                Arc::new(TimelineIdentity {
                    session_id: session.id.clone(),
                    branch_id: owner.id.clone(),
                }),
            )?;
            *owner.loaded.lock().unwrap() = Some(Arc::new(LoadedBranch {
                scope: Loaded {
                    ctx: ctx.clone(),
                    scope,
                    children: vec![],
                    required: self.2.clone(),
                    parts: vec![],
                },
                timeline,
                dispatcher,
                lifecycle,
                opening,
            }));
            Ok(Effect::Done)
        })
    }
}

impl TimelineFactory {
    pub fn startup_timeout(&self) -> Duration {
        self.0
            .upgrade()
            .map(|parent| parent.assembly.startup_timeout)
            .unwrap_or_default()
    }

    pub fn build(
        &self,
        id: String,
        ancestors: Vec<(String, u64)>,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, std::result::Result<Arc<TimelineContext>, TimelineBuildError>> {
        Box::pin(async move {
            let parent = self
                .0
                .upgrade()
                .ok_or_else(|| AgentError::Closed("session".into()))?;
            if parent.closing.is_cancelled() || cancel.is_cancelled() {
                return Err(AgentError::Closed(parent.id.clone()).into());
            }
            let session = parent.loaded()?;
            let key = TimelineKey {
                session_id: parent.id.clone(),
                branch_id: id.clone(),
            };
            let sequence = tokio::select! {
                result = tokio::time::timeout(self.startup_timeout(), parent.assembly.store.head(&key)) =>
                    result.map_err(|_| AgentError::Build("branch opening timed out".into()))??,
                _ = cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into()).into()),
            };
            let owner = Arc::new(BranchAssembly {
                session: parent.clone(),
                instance: Mutex::new(None),
                id: id.clone(),
                ancestors: Mutex::new(Some(ancestors)),
                sequence,
                loaded: Mutex::new(None),
                mounted: Arc::default(),
                copies: Arc::default(),
            });
            let started = {
                let value = owner.clone();
                create_instance(
                    &parent.loader,
                    &session.ctx,
                    BRANCH_GROUP.into(),
                    move |create| create.with(value),
                    self.startup_timeout(),
                    &cancel,
                    {
                        let owner = owner.clone();
                        move || {
                            let instance = *owner.instance.lock().unwrap();
                            let required = owner
                                .loaded
                                .lock()
                                .unwrap()
                                .as_ref()
                                .map(|loaded| loaded.scope.required.clone());
                            (instance, required, owner.mounted.lock().unwrap().clone())
                        }
                    },
                )
                .await
            };
            let mounted = std::mem::take(&mut *owner.mounted.lock().unwrap());
            let result = AssertUnwindSafe(async {
                let created = started.as_ref().map_err(|(error, _)| error.clone())?;
                let branch = {
                    let mut loaded = owner.loaded.lock().unwrap();
                    let current = loaded
                        .as_ref()
                        .ok_or_else(|| AgentError::Build("branch scope did not load".into()))?;
                    let branch = Arc::new(LoadedBranch {
                        scope: Loaded {
                            ctx: current.scope.ctx.clone(),
                            scope: current.scope.scope,
                            children: created.children.clone(),
                            required: current.scope.required.clone(),
                            parts: mounted.required.clone(),
                        },
                        timeline: current.timeline.clone(),
                        dispatcher: current.dispatcher.clone(),
                        lifecycle: current.lifecycle.clone(),
                        opening: current.opening.clone(),
                    });
                    *loaded = Some(branch.clone());
                    branch
                };
                if parent.closing.is_cancelled() {
                    return Err(AgentError::Closed(parent.id.clone()));
                }
                let ctx = &branch.scope.ctx;
                let scope = branch.scope.scope;
                required::<dyn LoopState>(ctx, scope.key::<dyn LoopState>())?;
                branch.timeline.index_submissions().await?;
                Ok(TimelineContext::start(
                    id,
                    ctx.clone(),
                    created.view.clone(),
                    branch.scope.children.clone(),
                    branch.scope.required.clone(),
                    branch.scope.parts.clone(),
                    owner.copies.clone(),
                    scope,
                    branch.timeline.clone(),
                    branch.dispatcher.clone(),
                    branch.lifecycle.clone(),
                    branch.opening.clone(),
                    parent.faults.clone(),
                    parent.assembly.catalog.clone(),
                ))
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(AgentError::Build("timeline assembly panicked".into())));
            match result {
                Ok(value) => Ok(value),
                Err(error) => {
                    let branch = owner.loaded.lock().unwrap().take();
                    if let Some(branch) = &branch {
                        branch.lifecycle.advance(BranchPhase::Closing);
                    }
                    let created = match started {
                        Ok(created) => Some(created),
                        Err((_, created)) => created,
                    };
                    let cleanup = match &created {
                        Some(created) => {
                            shutdown_scope(
                                &created.view,
                                &with_copies(&created.children, &owner.copies),
                            )
                            .await
                        }
                        None => Ok(()),
                    };
                    if let Some(branch) = &branch {
                        let _ = branch.timeline.close().await;
                        branch.lifecycle.advance(BranchPhase::Closed);
                    }
                    Err(TimelineBuildError {
                        cause: error,
                        cleanup: cleanup.err(),
                    })
                }
            }
        })
    }
}

/// A created instance and all its rows, including optional unavailable parts.
pub(crate) struct Created {
    pub view: FiberView,
    pub children: Vec<FiberView>,
}

/// Creates an instance of `group` from `ctx` and waits until its registered
/// rows are Active, within `timeout`. On failure the instance, when one was
/// created, comes back with the error for the caller to roll back; when a
/// timeout or cancellation leaves the creation running before the instance
/// can be found, a task disposes it once the creation ends.
///
/// `instance` reads its identity and the scope's registered row IDs; the
/// identity locates an instance whose creation is still running.
async fn create_instance(
    loader: &Loader,
    ctx: &Ctx,
    group: String,
    configure: impl for<'a> FnOnce(CreateInstance<'a>) -> CreateInstance<'a> + Send + 'static,
    timeout: Duration,
    cancel: &CancellationToken,
    instance: impl Fn() -> (
        Option<rutis::host::InstanceId>,
        Option<Arc<[String]>>,
        crate::parts::Opened,
    ),
) -> std::result::Result<Created, (AgentError, Option<Created>)> {
    let mut progress = StartupProgress::new(ctx, timeout).map_err(|error| (error, None))?;
    let mut creation = {
        let (loader, ctx, group) = (loader.clone(), ctx.clone(), group.clone());
        tokio::spawn(async move { configure(loader.create_instance(&ctx, &group)).await })
    };
    let ended = progress
        .wait(
            async {
                tokio::select! {
                    joined = &mut creation => Some(joined),
                    _ = cancel.cancelled() => None,
                }
            },
            || {
                instance()
                    .0
                    .and_then(|id| locate(loader, ctx, id))
                    .map(|created| vec![created.view.id])
                    .unwrap_or_default()
            },
        )
        .await
        .flatten();
    let created = match ended {
        Some(Ok(Ok(instance))) => instance,
        Some(Ok(Err(error))) => return Err((loader_error(error), None)),
        Some(Err(error)) => {
            return Err((
                AgentError::fault("instance creation", error.to_string()),
                None,
            ));
        }
        None => {
            let found = instance()
                .0
                .and_then(|instance| locate(loader, ctx, instance));
            let error = match &found {
                _ if cancel.is_cancelled() => AgentError::Cancelled("initialization".into()),
                Some(created) => timed_out(ctx, &created.children),
                None => AgentError::Build(format!("initialization timed out: {group}")),
            };
            if found.is_none() {
                tokio::spawn(async move {
                    if let Ok(Ok(instance)) = creation.await {
                        let _ = instance.view.shutdown().await;
                    }
                });
            }
            return Err((error, found));
        }
    };
    let required = instance().1;
    let failures = created
        .report
        .into_iter()
        .filter(|(id, _)| required.as_ref().is_none_or(|rows| rows.contains(id)))
        .filter_map(|(id, result)| match result {
            InstanceResult::Failed(error) if crate::parts::PartMounter::is_row(&id) => {
                Some(AgentError::Build(loader_error(error).to_string()))
            }
            InstanceResult::Failed(error) => Some(loader_error(error)),
            _ => None,
        })
        .collect();
    let mut created = Created {
        children: rows(&loader.entries(), &created.view, None),
        view: created.view,
    };
    if let Err(error) = AgentError::aggregate(failures) {
        return Err((error, Some(created)));
    }
    let Some(required) = required else {
        return Err((
            AgentError::Build(format!("{group} scope did not load")),
            Some(created),
        ));
    };
    let mut waiting = rows(&loader.entries(), &created.view, Some(&required));
    waiting.extend(instance().2.required);
    let ready = progress
        .wait(active_rows(&waiting, cancel), || vec![created.view.id])
        .await
        .unwrap_or_else(|| Err(timed_out(ctx, &waiting)));
    created.children = rows(&loader.entries(), &created.view, None);
    if let Err(error) = ready {
        return Err((error, Some(created)));
    }
    let optional: Vec<FiberView> = created
        .children
        .iter()
        .filter(|view| !waiting.iter().any(|required| required.id == view.id))
        .cloned()
        .chain(instance().2.optional)
        .collect();
    let settled = async {
        for view in waiting.iter().chain(&optional) {
            let _ = view.await;
        }
    };
    let settled = progress
        .wait(
            async {
                tokio::select! {
                    () = settled => Ok(()),
                    _ = cancel.cancelled() => Err(AgentError::Cancelled("initialization".into())),
                }
            },
            || vec![created.view.id],
        )
        .await
        .unwrap_or_else(|| Err(timed_out(ctx, &created.children)));
    match settled {
        Ok(()) => Ok(created),
        Err(error) => Err((error, Some(created))),
    }
}
/// Candidate rows, including nested extensions. Main and child branches do
/// not exist until Session publication and have their own assembly snapshots.
fn rows(
    entries: &[EntryInfo],
    instance: &FiberView,
    required: Option<&[String]>,
) -> Vec<FiberView> {
    scope_entries(entries, instance, required)
        .into_iter()
        .filter_map(|entry| entry.view.clone())
        .collect()
}

/// The rows `instance` holds, the ones `required` names when given, and never
/// its own row. A row belongs to an instance when its chain of enclosing
/// instances contains it. Creation and the assembly check both read their
/// rows here, so the two cannot select differently.
fn scope_entries<'a>(
    entries: &'a [EntryInfo],
    instance: &FiberView,
    required: Option<&[String]>,
) -> Vec<&'a EntryInfo> {
    entries
        .iter()
        .filter(|entry| required.is_none_or(|rows| rows.iter().any(|id| id == &entry.id)))
        .filter(|entry| {
            entry
                .instance
                .as_ref()
                .is_some_and(|i| i.chain.iter().any(|(_, id)| *id == instance.instance()))
        })
        .filter(|entry| {
            entry
                .view
                .as_ref()
                .is_none_or(|view| view.id != instance.id)
        })
        .collect()
}
/// The instance `instance` of a creation that did not end; it walks the
/// tree under `ctx`'s root.
fn locate(loader: &Loader, ctx: &Ctx, instance: rutis::host::InstanceId) -> Option<Created> {
    let plugin = ctx
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.instance == instance)?
        .id;
    let view = ctx.view(plugin)?;
    Some(Created {
        children: rows(&loader.entries(), &view, None),
        view,
    })
}
fn loader_error(error: rutis_loader::LoaderError) -> AgentError {
    match error {
        rutis_loader::LoaderError::Rejected { error, .. } => error.into(),
        error => AgentError::Build(error.to_string()),
    }
}

pub(crate) struct BuiltSession {
    pub owner: Arc<SessionAssembly>,
    pub view: FiberView,
    pub tree: Arc<dyn TimelineTreeControl>,
}

pub(crate) enum SessionInitialization {
    New,
    Prepared(Vec<StoredBranch>),
    Restore(Vec<StoredBranch>),
}

/// A Session extension requested for one Session: its group and the
/// value its capabilities read.
pub(crate) struct Extension {
    pub group: String,
    pub configure: Box<dyn for<'a> FnOnce(CreateInstance<'a>) -> CreateInstance<'a> + Send>,
}

pub(crate) async fn build_session(
    placement: &Placement,
    assembly: Assembly,
    id: String,
    config: SessionConfig,
    closing: CancellationToken,
    initialization: SessionInitialization,
    extension: Option<Extension>,
) -> Result<BuiltSession> {
    if let Some(extension) = &extension
        && !assembly
            .extensions
            .iter()
            .any(|(group, _)| group == &extension.group)
    {
        return Err(AgentError::Build(format!(
            "unknown Session extension: {}",
            extension.group
        )));
    }
    let host = &placement.anchor;
    let owner = Arc::new(SessionAssembly {
        assembly,
        extension: Mutex::new(ExtensionRequest::Pending(extension)),
        instance: Mutex::new(None),
        loader: placement.loader.clone(),
        config,
        id,
        restored: matches!(initialization, SessionInitialization::Restore(_)),
        faults: FaultReporter::default(),
        unloading: CancellationToken::new(),
        lifecycle: SessionLifecycle::new(closing.clone()),
        closing,
        loaded: Mutex::new(None),
        mounted: Arc::default(),
        copies: Arc::default(),
    });
    let startup_timeout = owner.assembly.startup_timeout;
    let instance = |owner: &Arc<SessionAssembly>| {
        let owner = owner.clone();
        move || {
            let instance = *owner.instance.lock().unwrap();
            let required = owner
                .loaded
                .lock()
                .unwrap()
                .as_ref()
                .map(|loaded| loaded.required.clone());
            (instance, required, owner.mounted.lock().unwrap().clone())
        }
    };
    let started = {
        let value = owner.clone();
        create_instance(
            &placement.loader,
            host,
            SESSION_GROUP.into(),
            move |create| create.with(value),
            startup_timeout,
            &owner.closing,
            instance(&owner),
        )
        .await
    };
    let mounted = std::mem::take(&mut *owner.mounted.lock().unwrap());
    let result = AssertUnwindSafe(async {
        let created = started.as_ref().map_err(|(error, _)| error.clone())?;
        let current = owner.loaded()?;
        let session = Arc::new(Loaded {
            ctx: current.ctx.clone(),
            scope: current.scope,
            children: created.children.clone(),
            required: current.required.clone(),
            parts: mounted.required.clone(),
        });
        *owner.loaded.lock().unwrap() = Some(session.clone());
        let tree = required::<dyn TimelineTreeControl>(
            &session.ctx,
            session.scope.key::<dyn TimelineTreeControl>(),
        )?;
        let initialize = async {
            match initialization {
                SessionInitialization::Prepared(branches)
                | SessionInitialization::Restore(branches) => tree.restore(branches).await,
                SessionInitialization::New => tree.create_main().await.map(|_| ()),
            }
        };
        tokio::pin!(initialize);
        let mut progress = StartupProgress::new(host, startup_timeout)?;
        let initialize = tokio::select! {
            result = progress.wait(&mut initialize, || vec![created.view.id]) => {
                match result {
                    Some(result) => result,
                    None => {
                        let error = timed_out(host, std::slice::from_ref(&created.view));
                        tree.begin_close();
                        let _ = initialize.await;
                        Err(error)
                    }
                }
            },
            _ = owner.closing.cancelled() => {
                tree.begin_close();
                let _ = initialize.await;
                Err(AgentError::Cancelled("session initialization".into()))
            }
        };
        if let Err(error) = initialize {
            return match tree.close().await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.with_cleanup(cleanup)),
            };
        }
        Ok((created.view.clone(), tree.clone()))
    })
    .catch_unwind()
    .await
    .unwrap_or_else(|_| Err(AgentError::Build("session assembly panicked".into())));
    match result {
        Ok((view, tree)) => Ok(BuiltSession { owner, view, tree }),
        Err(error) => {
            owner.lifecycle.advance(SessionPhase::Closed);
            let rollback = match started {
                Ok(created) => Some(created),
                Err((_, created)) => created,
            };
            let _session = owner.loaded.lock().unwrap().take();
            let cleanup = match &rollback {
                Some(created) => {
                    shutdown_scope(
                        &created.view,
                        &with_copies(&created.children, &owner.copies),
                    )
                    .await
                }
                None => Ok(()),
            };
            match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.with_cleanup(cleanup)),
            }
        }
    }
}
/// A service the facade needs to publish a scope; absence is an assembly error.
fn required<T: ?Sized + Send + Sync + 'static>(ctx: &Ctx, key: Key<T>) -> Result<Arc<T>> {
    ctx.get_at(&key).ok_or_else(|| {
        AgentError::Build(format!(
            "required service missing: {}",
            key.erase().describe()
        ))
    })
}
/// Waits until every view is Active; a failure or disposal of any view,
/// cancellation or an interval without progress ends the wait. First activation
/// or opening progress in any view's subtree renews the interval. A Failed view ends the wait even
/// while a dependent stays Pending. `ctx` observes those rows and supplies
/// unmet dependencies on timeout.
pub async fn wait_active(
    ctx: &Ctx,
    views: &[FiberView],
    timeout: std::time::Duration,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut progress = StartupProgress::new(ctx, timeout)?;
    progress
        .wait(active_rows(views, cancel), || {
            views.iter().map(|view| view.id).collect()
        })
        .await
        .unwrap_or_else(|| Err(timed_out(ctx, views)))
}

async fn active_rows(views: &[FiberView], cancel: &CancellationToken) -> Result<()> {
    let mut waiting: Vec<_> = views.iter().map(|view| (view, view.watch())).collect();
    loop {
        let mut index = 0;
        while index < waiting.len() {
            let (view, state) = &mut waiting[index];
            let snapshot = state.borrow_and_update().clone();
            match snapshot.state {
                FiberState::Active => {
                    waiting.swap_remove(index);
                    continue;
                }
                FiberState::Failed => {
                    return Err(snapshot
                        .error
                        .map(AgentError::from)
                        .unwrap_or_else(|| AgentError::Build(format!("{}: Failed", view.name()))));
                }
                FiberState::Disposed => {
                    return Err(AgentError::Build(format!("{}: Disposed", view.name())));
                }
                FiberState::Pending | FiberState::Loading | FiberState::Unloading => index += 1,
            }
        }
        if waiting.is_empty() {
            return Ok(());
        }
        let changed = futures_util::future::select_all(
            waiting
                .iter_mut()
                .map(|(_, state)| Box::pin(state.changed())),
        );
        let ended = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled("initialization".into())),
            (changed, index, _) = changed => changed.is_err().then_some(index),
        };
        if let Some(index) = ended {
            let name = waiting[index].0.name();
            return Err(AgentError::Build(format!(
                "{name} ended during initialization"
            )));
        }
    }
}
fn timed_out(ctx: &Ctx, views: &[FiberView]) -> AgentError {
    let snapshot = ctx.diagnostics();
    let ids: HashSet<_> = views
        .iter()
        .flat_map(|view| subtree(&snapshot, view.id))
        .collect();
    let pending: Vec<_> = snapshot
        .plugins
        .into_iter()
        .filter(|p| ids.contains(&p.id) && p.state != FiberState::Active)
        .map(|p| {
            let unmet: Vec<_> = p
                .injects
                .iter()
                .filter(|d| d.status != rutis::host::diagnostics::DependencyStatus::Ready)
                .map(|d| format!("{} {:?}", d.key.describe(), d.status))
                .collect();
            format!("{} {:?} [{}]", p.name, p.state, unmet.join(", "))
        })
        .collect();
    AgentError::Build(format!("initialization timed out: {}", pending.join("; ")))
}

/// One rule an assembly breaks ([`Agent::check_assembly`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssemblyRule {
    /// A required row, or a plugin below one, is not `Active`.
    Ready,
    /// A plugin reads an external service it does not declare.
    UndeclaredRead,
    /// A plugin read a service generation its resolved dependencies lack.
    Generation,
    /// A scoped service is provided without an instance key.
    InstanceKey,
}

/// One problem [`Agent::check_assembly`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssemblyFinding {
    /// The Session the finding is about.
    pub session: String,
    /// The branch the plugin belongs to, when its scope is a branch.
    pub branch: Option<String>,
    /// The plugin the finding is about.
    pub plugin: String,
    /// The rule the plugin breaks.
    pub rule: AssemblyRule,
    /// What the rule saw: a fiber state, or a service key.
    pub detail: String,
}
impl std::fmt::Display for AssemblyFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session {}", self.session)?;
        if let Some(branch) = &self.branch {
            write!(f, " branch {branch}")?;
        }
        write!(f, ": ")?;
        match self.rule {
            AssemblyRule::Ready => write!(
                f,
                "{} is not Active after its scope became ready: {}",
                self.plugin, self.detail
            ),
            AssemblyRule::UndeclaredRead => {
                write!(f, "{} read undeclared service {}", self.plugin, self.detail)
            }
            AssemblyRule::Generation => write!(
                f,
                "{} read a service generation absent from resolved dependencies: {}",
                self.plugin, self.detail
            ),
            AssemblyRule::InstanceKey => write!(
                f,
                "scoped service provided without an instance key: {}",
                self.detail
            ),
        }
    }
}

/// Checks one live scope of `snapshot`: its `required` rows, the parts its
/// mounters mounted and everything else in the scope, as creation waited for
/// them. `rows` is the loader's [index](scope_rows) of rows by instance,
/// `view` the scope instance, `required` the loader row ids its profile
/// requires, `parts` the required parts it mounted and `nested` the scopes
/// nested inside it. `scope` names the Session the scope belongs to and, for
/// a branch scope, the branch. Each plugin is checked once, by the innermost
/// scope that contains it: a nested scope's subtree is left to that scope's
/// own pass. A required row or part that has left the tree is reported too:
/// nothing in the snapshot would otherwise remember it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_scope(
    snapshot: &rutis::host::diagnostics::RuntimeDiagnostics,
    rows: &std::collections::HashMap<rutis::host::InstanceId, Vec<EntryInfo>>,
    view: &FiberView,
    required: &[String],
    parts: &[FiberView],
    nested: &[FiberView],
    scope: (&str, Option<&str>),
    findings: &mut Vec<AssemblyFinding>,
) {
    let (session, branch) = scope;
    let mut report = |plugin: String, rule: AssemblyRule, detail: String| {
        findings.push(AssemblyFinding {
            session: session.to_string(),
            branch: branch.map(str::to_string),
            plugin,
            rule,
            detail,
        });
    };
    let mut absent =
        |id: String| report(id, AssemblyRule::Ready, "absent from the snapshot".into());
    let entries = rows.get(&view.instance()).map(Vec::as_slice);
    let required_rows = scope_entries(entries.unwrap_or_default(), view, Some(required));
    for entry in &required_rows {
        if !entry
            .view
            .as_ref()
            .is_some_and(|row| snapshot.plugins.iter().any(|plugin| plugin.id == row.id))
        {
            absent(entry.id.clone());
        }
    }
    for part in parts {
        if !snapshot.plugins.iter().any(|plugin| plugin.id == part.id) {
            absent(part.name().to_owned());
        }
    }
    let excluded: HashSet<_> = nested
        .iter()
        .flat_map(|nested| subtree(snapshot, nested.id))
        .collect();
    let members: HashSet<_> = subtree(snapshot, view.id)
        .into_iter()
        .filter(|id| !excluded.contains(id))
        .collect();
    let required: HashSet<_> = required_rows
        .iter()
        .filter_map(|entry| entry.view.as_ref().map(|row| row.id))
        .flat_map(|id| subtree(snapshot, id))
        .collect();
    for plugin in snapshot.plugins.iter().filter(|p| members.contains(&p.id)) {
        if required.contains(&plugin.id) && plugin.state != FiberState::Active {
            report(
                plugin.name.clone(),
                AssemblyRule::Ready,
                format!("{:?}", plugin.state),
            );
        }
        for access in &plugin.accesses {
            if access.external && !access.declared {
                report(
                    plugin.name.clone(),
                    AssemblyRule::UndeclaredRead,
                    access.key.describe(),
                );
            }
            if access.external
                && access.provider.is_some()
                && access.declared
                && !plugin.resolved_dependencies.iter().any(|r| {
                    r.key == access.key
                        && r.scope == access.scope
                        && Some(r.provider) == access.provider
                        && Some(r.generation) == access.generation
                })
            {
                report(
                    plugin.name.clone(),
                    AssemblyRule::Generation,
                    access.key.describe(),
                );
            }
        }
    }
    for binding in snapshot
        .bindings
        .iter()
        .filter(|b| members.contains(&b.provider) && b.key.instance_id().is_none())
    {
        report(
            snapshot
                .plugins
                .iter()
                .find(|p| p.id == binding.provider)
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            AssemblyRule::InstanceKey,
            binding.key.describe(),
        );
    }
}

/// The loader's rows by the instance each runs in: one pass over
/// [`Loader::entries`], so a check of every live scope reads the rows once and
/// the reads no longer grow with the number of Sessions. A row is filed under
/// every enclosing instance, the chain [`scope_entries`] selects on.
pub(crate) fn scope_rows(
    loader: &Loader,
) -> std::collections::HashMap<rutis::host::InstanceId, Vec<EntryInfo>> {
    let mut rows: std::collections::HashMap<_, Vec<EntryInfo>> = Default::default();
    for entry in loader.entries() {
        let Some(instance) = &entry.instance else {
            continue;
        };
        for (_, id) in &instance.chain {
            rows.entry(*id).or_default().push(entry.clone());
        }
    }
    rows
}

/// Plugin IDs of `root` and its descendants in one diagnostics snapshot.
pub(crate) fn subtree(
    snapshot: &rutis::host::diagnostics::RuntimeDiagnostics,
    root: PluginId,
) -> HashSet<PluginId> {
    let mut members = HashSet::from([root]);
    let mut grown = true;
    while grown {
        grown = false;
        for plugin in &snapshot.plugins {
            if let Some(parent) = plugin.parent
                && members.contains(&parent)
                && members.insert(plugin.id)
            {
                grown = true;
            }
        }
    }
    members
}

/// The snapshot restricted to one scope plugin and the capabilities it installed.
pub(crate) fn scope_diagnostics(
    ctx: &Ctx,
    scope: PluginId,
    children: &[FiberView],
) -> rutis::host::diagnostics::RuntimeDiagnostics {
    let rows: HashSet<_> = children.iter().map(|view| view.id).collect();
    let mut snapshot = ctx.diagnostics();
    retain_scope(&mut snapshot, scope, |plugin| rows.contains(&plugin));
    snapshot
}
/// Keeps `scope`, its rows (`row`) and the parts its mounters mounted.
pub(crate) fn retain_scope(
    snapshot: &mut rutis::host::diagnostics::RuntimeDiagnostics,
    scope: PluginId,
    row: impl Fn(PluginId) -> bool,
) {
    let mut members: HashSet<_> = snapshot
        .plugins
        .iter()
        .filter(|p| p.id == scope || row(p.id))
        .map(|p| p.id)
        .collect();
    let mounters: HashSet<_> = snapshot
        .plugins
        .iter()
        .filter(|p| members.contains(&p.id) && crate::parts::PartMounter::is_module(&p.name))
        .map(|p| p.id)
        .collect();
    members.extend(
        snapshot
            .plugins
            .iter()
            .filter(|p| p.parent.is_some_and(|parent| mounters.contains(&parent)))
            .map(|p| p.id),
    );
    snapshot.plugins.retain(|p| members.contains(&p.id));
    snapshot.bindings.retain(|b| members.contains(&b.provider));
}

/// Permanently closes a scope and returns only cleanup failures.
///
/// rutis reports a plugin's load result again from shutdown under the same
/// identity, with the apply error first and its rollback errors after it. A
/// plugin that had Failed already surfaced that whole error as the build cause.
/// A plugin still loading had its apply rejected by the cancellation itself;
/// that rejection is dropped while its rollback errors remain cleanup failures.
/// A scope's rows and the copies its mounters mounted, as one close sees them.
pub(crate) fn with_copies(children: &[FiberView], copies: &crate::parts::Copies) -> Vec<FiberView> {
    let mut members = children.to_vec();
    members.extend(copies.lock().unwrap().iter().cloned());
    members
}
pub(crate) async fn shutdown_scope(view: &FiberView, children: &[FiberView]) -> Result<()> {
    let members: Vec<_> = children.iter().chain([view]).collect();
    let before: Vec<_> = members.iter().map(|view| view.state().state).collect();
    let Err(error) = view.shutdown().await else {
        return Ok(());
    };
    let mut known = Vec::new();
    for (view, state) in members.iter().zip(before) {
        let Some(error) = view.state().error else {
            continue;
        };
        match (state, &*error) {
            (FiberState::Active, _) => {}
            (FiberState::Failed, _) => known.push(error.clone()),
            (_, ErrorKind::Aggregate { errors }) => known.extend(errors.first().cloned()),
            _ => known.push(error.clone()),
        }
    }
    match without(error, &known) {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}
fn without(error: Arc<ErrorKind>, known: &[Arc<ErrorKind>]) -> Option<Arc<ErrorKind>> {
    if known.iter().any(|k| Arc::ptr_eq(k, &error)) {
        return None;
    }
    let ErrorKind::Aggregate { errors } = &*error else {
        return Some(error);
    };
    let mut rest: Vec<_> = errors
        .iter()
        .filter_map(|e| without(e.clone(), known))
        .collect();
    match rest.len() {
        0 => None,
        1 => rest.pop(),
        n if n == errors.len() => Some(error),
        _ => Some(Arc::new(ErrorKind::Aggregate { errors: rest })),
    }
}
