use crate::core::OpeningCount;
use crate::*;
use futures_util::FutureExt;
use rutis::{
    BoxFuture as RutisFuture, CordisError, Ctx, Effect, FiberState, FiberView, Plugin,
    PluginFactory, PluginId, TypeKey, TypedPlugin,
};
use rutis_loader::{Build, Builtins, CreateInstance, InstanceResult, Loader, Resolver};
use serde_json::{Value, json};
use std::time::Duration;
use std::{
    collections::HashSet,
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

/// Names registered on an assembly, handed to the registry factories.
#[derive(Clone, Copy)]
pub struct Registered<'a> {
    pub tools: ToolNames<'a>,
    /// Prompt sources registered with [`AgentBuilder::session_prompt_source`].
    pub prompt_sources: &'a [String],
}

/// Tool names registered on an assembly, handed to the tool registry factory.
#[derive(Clone, Copy)]
pub struct ToolNames<'a> {
    /// Fixed contributions registered with [`AgentBuilder::session_tool`].
    pub contributed: &'a [String],
    /// Dynamic sources registered with [`AgentBuilder::session_tool_source`].
    pub sources: &'a [String],
}
type SessionMake = Arc<dyn Fn(&SessionScope, Registered<'_>) -> Box<dyn Plugin> + Send + Sync>;
type BranchMake = Arc<dyn Fn(&BranchScope) -> Box<dyn Plugin> + Send + Sync>;
type ScopedMake = Arc<dyn Fn(&SessionScope) -> Box<dyn Plugin> + Send + Sync>;
/// An extension capability: its plugin name and how a copy reads its value.
/// Reads an extension's value from where its copy runs.
type ExtensionMake =
    Arc<dyn Fn(&Build) -> std::result::Result<ScopedMake, CordisError> + Send + Sync>;
type Provide =
    Arc<dyn Fn(&Ctx, &SessionScope) -> std::result::Result<(), CordisError> + Send + Sync>;
#[derive(Clone)]
pub(crate) struct Assembly {
    pub config: AgentConfig,
    /// Deadline for a Session or branch to become Active.
    pub startup_timeout: Duration,
    pub store: Arc<dyn TimelineStore>,
    pub ephemeral_store: Option<Arc<MemoryTimelineStore>>,
    /// This Agent's branch openings that are alive.
    pub openings: OpeningCount,
    values: Vec<Provide>,
    session: Vec<SessionMake>,
    timeline: Vec<BranchMake>,
    /// Nested instanced groups created per Session on request.
    extensions: Vec<(String, Vec<(String, ExtensionMake)>)>,
    tools: Vec<String>,
    tool_sources: Vec<String>,
    host_tools: Vec<String>,
    prompt_sources: Vec<String>,
    session_methods: Vec<String>,
}
#[derive(Clone)]
pub struct AgentBuilder {
    assembly: Assembly,
    custom_store: bool,
}
impl AgentBuilder {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            custom_store: false,
            assembly: Assembly {
                config,
                startup_timeout: Duration::from_secs(10),
                store: Arc::new(MemoryTimelineStore::default()),
                ephemeral_store: None,
                openings: OpeningCount::default(),
                values: vec![],
                session: vec![],
                timeline: vec![],
                extensions: vec![],
                tools: vec![],
                tool_sources: vec![],
                host_tools: vec![],
                prompt_sources: vec![],
                session_methods: vec![],
            },
        }
    }
    /// Uses one store for every Session built by this Agent.
    pub fn timeline_store(&mut self, store: Arc<dyn TimelineStore>) -> &mut Self {
        self.assembly.store = store;
        self.custom_store = true;
        self
    }
    pub fn startup_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.assembly.startup_timeout = timeout;
        self
    }
    /// Registers a Session capability; the factory runs once per Session
    /// subtree, and once more when the assembly is installed, with a
    /// placeholder scope, to name its module ([`Self::install`]).
    pub fn session_plugin<P: Capability>(
        &mut self,
        factory: impl Fn(&SessionScope) -> P + Send + Sync + 'static,
    ) -> &mut Self {
        self.assembly
            .session
            .push(Arc::new(move |scope, _| typed(factory(scope))));
        self
    }
    /// Registers the Session capability that collects tools; the factory
    /// receives every name registered with [`Self::session_tool`] and
    /// [`Self::session_tool_source`].
    pub fn session_tool_registry<P: Capability>(
        &mut self,
        factory: impl Fn(&SessionScope, ToolNames<'_>) -> P + Send + Sync + 'static,
    ) -> &mut Self {
        self.assembly.session.push(Arc::new(move |scope, names| {
            typed(factory(scope, names.tools))
        }));
        self
    }
    /// Registers the Session capability that collects prompt sources; the
    /// factory receives every name registered with
    /// [`Self::session_prompt_source`].
    pub fn session_prompt_registry<P: Capability>(
        &mut self,
        factory: impl Fn(&SessionScope, &[String]) -> P + Send + Sync + 'static,
    ) -> &mut Self {
        self.assembly.session.push(Arc::new(move |scope, names| {
            typed(factory(scope, names.prompt_sources))
        }));
        self
    }
    /// Registers a prompt source a Session capability provides under this
    /// name; a registered name without a provider fails the Session.
    pub fn session_prompt_source(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        if !self.assembly.prompt_sources.contains(&name) {
            self.assembly.prompt_sources.push(name);
        }
        self
    }
    /// Registers a tool that a Session capability contributes under this name.
    pub fn session_tool(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        if !self.assembly.tools.contains(&name) {
            self.assembly.tools.push(name);
        }
        self
    }
    /// Registers a dynamic tool source a Session capability provides under
    /// this name; a registered name without a provider fails the Session.
    pub fn session_tool_source(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        if !self.assembly.tool_sources.contains(&name) {
            self.assembly.tool_sources.push(name);
        }
        self
    }
    /// Records tools the host `ToolHost` provides; nothing is injected for
    /// them. Together with contributions they are the provided tool names.
    pub fn host_tools(&mut self, names: impl IntoIterator<Item = String>) -> &mut Self {
        for name in names {
            if !self.assembly.host_tools.contains(&name) {
                self.assembly.host_tools.push(name);
            }
        }
        self
    }
    /// Every tool name a Session of this assembly can enable: host tools
    /// recorded with [`Self::host_tools`] and contributions registered with
    /// [`Self::session_tool`].
    pub fn provided_tools(&self) -> impl Iterator<Item = &str> {
        self.assembly
            .host_tools
            .iter()
            .chain(&self.assembly.tools)
            .map(String::as_str)
    }
    /// Records a `sessionMethods` namespace that a Session capability
    /// provides: the Session's known namespaces (methods contract A.1), which
    /// the host reads with [`Self::session_method_namespaces`].
    pub fn session_methods(&mut self, namespace: impl Into<String>) -> &mut Self {
        let namespace = namespace.into();
        if !self.assembly.session_methods.contains(&namespace) {
            self.assembly.session_methods.push(namespace);
        }
        self
    }
    /// The namespaces recorded with [`Self::session_methods`].
    pub fn session_method_namespaces(&self) -> &[String] {
        &self.assembly.session_methods
    }
    /// Registers a branch capability; the factory runs once per branch
    /// subtree, and once more when the assembly is installed, with a
    /// placeholder scope, to name its module ([`Self::install`]).
    pub fn timeline_plugin<P: Capability>(
        &mut self,
        factory: impl Fn(&BranchScope) -> P + Send + Sync + 'static,
    ) -> &mut Self {
        self.assembly
            .timeline
            .push(Arc::new(move |scope| typed(factory(scope))));
        self
    }
    /// Registers a capability of the Session extension `group`: a nested
    /// instanced group created only for Sessions that ask for it, with a
    /// value of type `V` (see [`Agent::create_session_with_extension`]).
    /// The extension loads alongside Session capabilities after its scope is
    /// ready. Every capability must be Active before main initialization.
    /// `name` is the plugin's name, which its fiber is registered under
    /// before any value exists; a plugin with another name fails to load.
    pub fn session_extension<V: Send + Sync + 'static, P: Capability>(
        &mut self,
        group: impl Into<String>,
        name: impl Into<String>,
        factory: impl Fn(&SessionScope, &V) -> P + Send + Sync + 'static,
    ) -> &mut Self {
        let (group, name) = (group.into(), name.into());
        let factory = Arc::new(factory);
        let make: ExtensionMake = Arc::new(move |build| {
            let value = build.value::<V>().ok_or_else(|| {
                CordisError::PluginFailed("Session extension created without its value".into())
            })?;
            let factory = factory.clone();
            let make: ScopedMake = Arc::new(move |scope| typed(factory(scope, &value)));
            Ok(make)
        });
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
    /// Provides a host-configured value as a Session service of every Session;
    /// the Session scope provides it before any capability loads.
    pub fn session_value<T: ?Sized + SessionService>(&mut self, value: Arc<T>) -> &mut Self {
        self.assembly.values.push(Arc::new(move |ctx, scope| {
            ctx.provide_as::<T>(scope.key::<T>(), value.clone())?;
            Ok(())
        }));
        self
    }
    /// Registers the loader modules of this assembly in `builtins`: the
    /// Session and branch scopes, every capability and every extension, and
    /// returns the [`SESSION_GROUP`] row holding them and the nested
    /// [`BRANCH_GROUP`]; a host puts it inside its [`AGENT_GROUP`] row.
    /// These registered parts are required for readiness. Additional host
    /// rows remain owned by the group without being required to become Active.
    ///
    /// A fiber is shown under its module name, so each module is named after
    /// its plugin: a plugin is built once with `ctx`'s instance to read it.
    /// Register host modules first: names already in `builtins` or used by
    /// another row get a `#n` suffix. Extension group names cannot be renamed.
    /// Pass host identities absent from `builtins` in `reserved_names`: both
    /// module names resolved by another runtime and row IDs that differ from
    /// their module names.
    /// Construction and metadata panics are returned as build errors.
    pub async fn install<'a>(
        &self,
        builtins: &mut Builtins,
        ctx: &Ctx,
        reserved_names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Value> {
        AssertUnwindSafe(async {
            let assembly = &self.assembly;
            let names = Arc::new((
                assembly.tools.clone(),
                assembly.tool_sources.clone(),
                assembly.prompt_sources.clone(),
            ));
            let sample = SessionScope {
                instance: ctx.instance(),
            };
            let sample_branch = BranchScope {
                session: sample,
                instance: ctx.instance(),
            };
            let mut modules = Modules(HashSet::from_iter(
                [
                    AGENT_GROUP,
                    SESSION_GROUP,
                    BRANCH_GROUP,
                    "AgentAnchor",
                    "AgentScope",
                ]
                .map(str::to_owned),
            ));
            modules
                .0
                .extend(reserved_names.into_iter().map(str::to_owned));
            for (name, _) in &assembly.extensions {
                if !modules.0.insert(name.clone()) || builtins.resolve(name).await.is_ok() {
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
                builtins.register_with::<NoConfig, _, _>(module, |build| {
                    let owner = build.value::<Arc<SessionAssembly>>().ok_or_else(|| {
                        CordisError::PluginFailed(
                            "Session extension created without its assembly".into(),
                        )
                    })?;
                    let scope = session_scope(build)?;
                    Factory::new(move || {
                        typed(SessionExtensionPlugin {
                            owner: (*owner).clone(),
                            scope,
                        })
                    })
                });
            }
            for make in &assembly.session {
                let (make, names) = (make.clone(), names.clone());
                let build_with = move |scope: &SessionScope| {
                    make(
                        scope,
                        Registered {
                            tools: ToolNames {
                                contributed: &names.0,
                                sources: &names.1,
                            },
                            prompt_sources: &names.2,
                        },
                    )
                };
                let module = modules.name(build_with(&sample).name(), builtins).await;
                session.push(row(&module));
                let build_with = Arc::new(build_with);
                builtins.register_with::<NoConfig, _, _>(module, move |build| {
                    let (scope, build_with) = (session_scope(build)?, build_with.clone());
                    Factory::new(move || build_with(&scope))
                });
            }
            let mut branch = vec![row(&branch_scope_module)];
            for make in &assembly.timeline {
                let module = modules.name(make(&sample_branch).name(), builtins).await;
                branch.push(row(&module));
                let make = make.clone();
                builtins.register_with::<NoConfig, _, _>(module, move |build| {
                    let (scope, make) = (branch_scope(build)?, make.clone());
                    Factory::new(move || make(&scope))
                });
            }
            let required_branch: Arc<[String]> = branch
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned())
                .collect();
            let mut required_session: Vec<String> = session
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned())
                .collect();
            session.push(group(BRANCH_GROUP, branch));
            for (name, makes) in &assembly.extensions {
                let mut rows = vec![];
                for (plugin, make) in makes {
                    let module = modules.name(plugin, builtins).await;
                    required_session.push(module.clone());
                    rows.push(row(&module));
                    let (make, plugin) = (make.clone(), plugin.clone());
                    builtins.register_with::<NoConfig, _, _>(module, move |build| {
                        let scope = session_scope(build)?;
                        let make = make(build)?;
                        let factory = Factory::new(move || make(&scope))?;
                        if factory.name != plugin {
                            return Err(CordisError::PluginFailed(
                                format!("extension plugin {} registered as {plugin}", factory.name)
                                    .into(),
                            ));
                        }
                        Ok(factory)
                    });
                }
                session.push(group(name, rows));
            }
            let required_session: Arc<[String]> = required_session.into();
            builtins.register_with::<NoConfig, _, _>(session_scope_module, move |build| {
                let owner = build.value::<Arc<SessionAssembly>>().ok_or_else(|| {
                    CordisError::PluginFailed("Session created without its assembly".into())
                })?;
                let scope = session_scope(build)?;
                // Recorded before any row applies, including a failed scope.
                *owner.instance.lock().unwrap() = Some(scope.instance);
                let required = required_session.clone();
                Factory::new(move || {
                    typed(SessionScopePlugin(
                        (*owner).clone(),
                        scope,
                        required.clone(),
                    ))
                })
            });
            builtins.register_with::<NoConfig, _, _>(branch_scope_module, move |build| {
                let owner = build.value::<Arc<BranchAssembly>>().ok_or_else(|| {
                    CordisError::PluginFailed("branch created without its assembly".into())
                })?;
                let scope = branch_scope(build)?;
                *owner.instance.lock().unwrap() = Some(scope.instance);
                let required = required_branch.clone();
                Factory::new(move || {
                    typed(BranchScopePlugin((*owner).clone(), scope, required.clone()))
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
    /// modules ([`Self::register`]) and its rows inside [`AGENT_GROUP`]
    /// ([`Self::session_row`]).
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
    first: Mutex<Option<Box<dyn Plugin>>>,
    make: Box<dyn Fn() -> std::result::Result<Box<dyn Plugin>, CordisError> + Send + Sync>,
}
impl Factory {
    fn new(
        make: impl Fn() -> Box<dyn Plugin> + Send + Sync + 'static,
    ) -> std::result::Result<Self, CordisError> {
        let make = Box::new(move || {
            catch_unwind(AssertUnwindSafe(&make))
                .map_err(|_| CordisError::PluginFailed("plugin factory panicked".into()))
        });
        let first = make()?;
        catch_unwind(AssertUnwindSafe(|| Self {
            name: first.name().to_owned(),
            injects: first.injects().to_vec(),
            first: Mutex::new(Some(first)),
            make,
        }))
        .map_err(|_| CordisError::PluginFailed("plugin metadata panicked".into()))
    }
}
impl PluginFactory<NoConfig> for Factory {
    fn name(&self) -> &str {
        &self.name
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn build(&self, _: &NoConfig) -> std::result::Result<Box<dyn Plugin>, CordisError> {
        match self.first.lock().unwrap().take() {
            Some(plugin) => Ok(plugin),
            None => (self.make)(),
        }
    }
}
/// Reserved identities outside Builtins, which owns registered module names.
struct Modules(HashSet<String>);
impl Modules {
    async fn name(&mut self, plugin: &str, builtins: &Builtins) -> String {
        let mut name = plugin.to_owned();
        let mut suffix = 2;
        while self.0.contains(&name) || builtins.resolve(&name).await.is_ok() {
            name = format!("{plugin}#{suffix}");
            suffix += 1;
        }
        self.0.insert(name.clone());
        name
    }
}
/// A row named after its module.
fn row(module: &str) -> Value {
    json!({ "id": module, "name": module })
}
fn group(id: &str, config: Vec<Value>) -> Value {
    json!({ "id": id, "group": true, "instanced": true, "config": config })
}
fn session_scope(build: &Build) -> std::result::Result<SessionScope, CordisError> {
    Ok(SessionScope {
        instance: build.instance(SESSION_GROUP)?,
    })
}
fn branch_scope(build: &Build) -> std::result::Result<BranchScope, CordisError> {
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
    let (root, parent) = match host {
        None => {
            let root = Ctx::root_with_sink(
                tokio::runtime::Handle::current(),
                Arc::new(|error| tracing::error!(%error, "Agent runtime error")),
            );
            (Some(root.clone()), root)
        }
        Some(host) => (
            None,
            host.isolate(
                TypeKey::of::<Loader>(),
                &format!("dim-agent/{}", uuid::Uuid::new_v4()),
            ),
        ),
    };
    let mut builtins = Builtins::new();
    builtins.register_with::<NoConfig, _, _>("AgentAnchor", |build| {
        let slot = build
            .value::<AnchorSlot>()
            .ok_or_else(|| CordisError::PluginFailed("Agent created without its slot".into()))?;
        Factory::new(move || typed(AgentAnchor((*slot).clone())))
    });
    let session_row = match builder.install(&mut builtins, &parent, []).await {
        Ok(row) => row,
        Err(error) => {
            if let Some(root) = root {
                root.shutdown().await.map_err(AgentError::from)?;
            }
            return Err(error);
        }
    };
    let plugin = rutis_loader::LoaderPlugin::new(builtins, rutis_loader::LoaderOptions::default());
    let loader = plugin.handle();
    let view = parent.plugin(plugin);
    let mut owned = Owned {
        root,
        views: vec![view.clone()],
    };
    let placed = async {
        wait_active(&parent, std::slice::from_ref(&view), timeout, cancel).await?;
        let rows = json!([{ "insert": [group(
            AGENT_GROUP,
            vec![row("AgentAnchor"), session_row],
        )] }]);
        let rows = serde_json::from_value(rows).map_err(|e| AgentError::Build(e.to_string()))?;
        let report = loader
            .reconcile(vec![rutis_loader::Layer::new("assembly", rows)], None)
            .await
            .map_err(loader_error)?;
        // A skipped row (a duplicate module name) would only show later.
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
impl TypedPlugin for AgentAnchor {
    type Deps = ();
    fn name(&self) -> &str {
        "AgentAnchor"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): Self::Deps,
    ) -> RutisFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            self.0
                .0
                .set(ctx.clone())
                .map_err(|_| CordisError::PluginFailed("Agent anchor reloaded".into()))?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for AgentAnchor {
    fn keys(&self) -> KeysOf<Self> {}
}

/// A loaded scope plugin: its own context and the capability plugins it installed.
pub(crate) struct Loaded<S> {
    pub ctx: Ctx,
    pub scope: S,
    pub children: Vec<FiberView>,
    required: Arc<[String]>,
}

pub(crate) struct SessionAssembly {
    pub assembly: Assembly,
    extension: Mutex<ExtensionRequest>,
    /// The Session instance, once the loader spawned its rows.
    pub instance: Mutex<Option<rutis::InstanceId>>,
    pub loader: Loader,
    pub config: SessionConfig,
    pub id: String,
    pub restored: bool,
    pub unloading: CancellationToken,
    pub faults: FaultReporter,
    pub closing: CancellationToken,
    pub lifecycle: SessionLifecycle,
    pub loaded: Mutex<Option<Arc<Loaded<SessionScope>>>>,
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
impl TypedPlugin for SessionScopePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "SessionScope"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): Self::Deps,
    ) -> RutisFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let (owner, scope) = (&self.0, self.1);
            ctx.provide_as(
                scope.key::<crate::SessionLoader>(),
                Arc::new(crate::SessionLoader::new(owner.loader.clone())),
            )?;
            ctx.provide_as(scope.key::<SessionConfig>(), Arc::new(owner.config.clone()))?;
            ctx.provide_as(
                scope.key::<AgentConfig>(),
                Arc::new(owner.assembly.config.clone()),
            )?;
            ctx.provide_as(
                scope.key::<SessionIdentity>(),
                Arc::new(SessionIdentity {
                    session_id: owner.id.clone(),
                    restored: owner.restored,
                    unloading: owner.unloading.clone(),
                }),
            )?;
            ctx.provide_as(
                scope.key::<SessionLifecycle>(),
                Arc::new(owner.lifecycle.clone()),
            )?;
            ctx.provide_as(
                scope.key::<TimelineFactory>(),
                Arc::new(TimelineFactory(Arc::downgrade(owner))),
            )?;
            ctx.provide_as::<dyn TimelineStore>(
                scope.key::<dyn TimelineStore>(),
                owner.assembly.store.clone(),
            )?;
            ctx.provide_as(scope.key::<FaultReporter>(), Arc::new(owner.faults.clone()))?;
            for provide in &owner.assembly.values {
                provide(ctx, &scope)?;
            }
            let mut loaded = owner.loaded.lock().unwrap();
            if loaded.is_some() {
                return Err(CordisError::PluginFailed("Session scope reloaded".into()));
            }
            *loaded = Some(Arc::new(Loaded {
                ctx: ctx.clone(),
                scope,
                children: vec![],
                required: self.2.clone(),
            }));
            Ok(Effect::Done)
        })
    }
}
impl Capability for SessionScopePlugin {
    fn keys(&self) -> KeysOf<Self> {}
}

/// Structural assembly runs once, just like SessionScope itself. The nested
/// instance belongs to the Session group, not to this launcher fiber.
enum ExtensionRequest {
    Pending(Option<Extension>),
    Started,
}
struct SessionExtensionPlugin {
    owner: Arc<SessionAssembly>,
    scope: SessionScope,
}
impl Capability for SessionExtensionPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}
impl TypedPlugin for SessionExtensionPlugin {
    type Deps = SessionGate<SessionIdentity>;
    fn name(&self) -> &str {
        "SessionExtension"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        _: Self::Deps,
    ) -> RutisFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let request = std::mem::replace(
                &mut *self.owner.extension.lock().unwrap(),
                ExtensionRequest::Started,
            );
            let extension = match request {
                ExtensionRequest::Pending(Some(extension)) => extension,
                ExtensionRequest::Pending(None) => return Ok(Effect::Done),
                ExtensionRequest::Started => {
                    return Err(CordisError::PluginFailed(
                        "Session extension launcher reloaded".into(),
                    ));
                }
            };
            let cancel = ctx.cancellation_token();
            let created = tokio::select! {
                created = (extension.configure)(self.owner.loader.create_instance(ctx, &extension.group)) => created
                    .map_err(|error| CordisError::PluginFailed(loader_error(error).into()))?,
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
                .map_err(|error| CordisError::PluginFailed(error.into()))?;
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
    instance: Mutex<Option<rutis::InstanceId>>,
    id: String,
    ancestors: Mutex<Option<Vec<(String, u64)>>>,
    sequence: u64,
    loaded: Mutex<Option<Arc<LoadedBranch>>>,
}
struct LoadedBranch {
    scope: Loaded<BranchScope>,
    timeline: Timeline,
    dispatcher: Dispatcher,
    lifecycle: BranchLifecycle,
    opening: Arc<BranchOpening>,
}
struct BranchScopePlugin(Arc<BranchAssembly>, BranchScope, Arc<[String]>);
impl TypedPlugin for BranchScopePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "BranchScope"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): Self::Deps,
    ) -> RutisFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let (owner, scope) = (&self.0, self.1);
            let session = &owner.session;
            let ancestors = owner
                .ancestors
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| CordisError::PluginFailed("Branch scope reloaded".into()))?;
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
            // The Timeline is closed by its handle or by build rollback.
            let dispatcher = Dispatcher::new(scope.instance, owner.sequence, timeline.clone());
            ctx.provide_as(scope.key::<Timeline>(), Arc::new(timeline.clone()))?;
            ctx.provide_as(scope.key::<Dispatcher>(), Arc::new(dispatcher.clone()))?;
            let lifecycle = BranchLifecycle::new();
            // Its capabilities rebuild from the history it is built with,
            // while the branch is `Loading`.
            timeline.begin_opening(&lifecycle, &session.assembly.openings);
            ctx.provide_as(scope.key::<BranchLifecycle>(), Arc::new(lifecycle.clone()))?;
            let opening = Arc::new(BranchOpening::new());
            ctx.provide_as(scope.key::<BranchOpening>(), opening.clone())?;
            ctx.provide_as(
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
impl Capability for BranchScopePlugin {
    fn keys(&self) -> KeysOf<Self> {}
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
        self.build_until(
            id,
            ancestors,
            cancel,
            tokio::time::Instant::now() + self.startup_timeout(),
        )
    }

    /// Uses the tree's complete opening budget, including parent readiness.
    pub fn build_until(
        &self,
        id: String,
        ancestors: Vec<(String, u64)>,
        cancel: CancellationToken,
        deadline: tokio::time::Instant,
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
                result = parent.assembly.store.head(&key) => result?,
                _ = cancel.cancelled() => return Err(AgentError::Cancelled("branch creation".into()).into()),
                _ = tokio::time::sleep_until(deadline) => return Err(AgentError::Build("branch opening timed out".into()).into()),
            };
            let owner = Arc::new(BranchAssembly {
                session: parent.clone(),
                instance: Mutex::new(None),
                id: id.clone(),
                ancestors: Mutex::new(Some(ancestors)),
                sequence,
                loaded: Mutex::new(None),
            });
            let started = {
                let value = owner.clone();
                create_instance(
                    &parent.loader,
                    &session.ctx,
                    BRANCH_GROUP.into(),
                    move |create| create.with(value),
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
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
                            (instance, required)
                        }
                    },
                )
                .await
            };
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
                // Publication requires Loop; the context looks it up per call.
                required::<dyn LoopState>(ctx, scope.key::<dyn LoopState>())?;
                Ok(TimelineContext::start(
                    id,
                    ctx.clone(),
                    created.view.clone(),
                    branch.scope.children.clone(),
                    scope,
                    branch.timeline.clone(),
                    branch.dispatcher.clone(),
                    branch.lifecycle.clone(),
                    branch.opening.clone(),
                    parent.faults.clone(),
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
                        // Waiting background work learns the branch never started.
                        branch.lifecycle.advance(BranchPhase::Closing);
                    }
                    let created = match started {
                        Ok(created) => Some(created),
                        Err((_, created)) => created,
                    };
                    // Unload the capabilities first so their cleanup can still
                    // write terminals, then close the Timeline, as a close does.
                    let cleanup = match &created {
                        Some(created) => shutdown_scope(&created.view, &created.children).await,
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
    instance: impl Fn() -> (Option<rutis::InstanceId>, Option<Arc<[String]>>),
) -> std::result::Result<Created, (AgentError, Option<Created>)> {
    let deadline = tokio::time::Instant::now() + timeout;
    // shortcut: loader creation still settles Loading rows; revisit if apply must wait on external work.
    let mut creation = {
        let (loader, ctx, group) = (loader.clone(), ctx.clone(), group.clone());
        tokio::spawn(async move { configure(loader.create_instance(&ctx, &group)).await })
    };
    let ended = tokio::select! {
        joined = &mut creation => Some(joined),
        _ = cancel.cancelled() => None,
        _ = tokio::time::sleep_until(deadline) => None,
    };
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
                // Dispose the instance once its creation ends.
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
        .filter_map(|(_, result)| match result {
            InstanceResult::Failed(error) => Some(loader_error(error)),
            _ => None,
        })
        .collect();
    let mut created = Created {
        children: rows(loader, &created.view, None),
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
    let waiting = rows(loader, &created.view, Some(&required));
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let ready = wait_active(ctx, &waiting, remaining, cancel).await;
    // A launcher may still be Pending when the loader returns. Once it is
    // Active its nested rows exist, but those rows may themselves be Pending.
    // Refresh on failure too, so the rollback covers everything built.
    created.children = rows(loader, &created.view, None);
    if let Err(error) = ready {
        return Err((error, Some(created)));
    }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let waiting = rows(loader, &created.view, Some(&required));
    match wait_active(ctx, &waiting, remaining, cancel)
        .await
        .and_then(|()| validate(ctx, created.view.id, &waiting))
    {
        Ok(()) => Ok(created),
        Err(error) => Err((error, Some(created))),
    }
}
/// Candidate rows, including nested extensions. Main and child branches do
/// not exist until Session publication and have their own assembly snapshots.
fn rows(loader: &Loader, instance: &FiberView, required: Option<&[String]>) -> Vec<FiberView> {
    loader
        .entries()
        .into_iter()
        .filter(|entry| required.is_none_or(|rows| rows.contains(&entry.id)))
        .filter(|entry| {
            entry
                .instance
                .as_ref()
                .is_some_and(|i| i.chain.iter().any(|(_, id)| *id == instance.instance()))
        })
        .filter_map(|entry| entry.view)
        .filter(|view| view.id != instance.id)
        .collect()
}
/// The instance `instance` of a creation that did not end; it walks the
/// tree under `ctx`'s root.
fn locate(loader: &Loader, ctx: &Ctx, instance: rutis::InstanceId) -> Option<Created> {
    let plugin = ctx
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.instance == instance)?
        .id;
    let view = ctx.view(plugin)?;
    Some(Created {
        children: rows(loader, &view, None),
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
    });
    let startup_timeout = owner.assembly.startup_timeout;
    let deadline = tokio::time::Instant::now() + startup_timeout;
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
            (instance, required)
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
    let result = AssertUnwindSafe(async {
        let created = started.as_ref().map_err(|(error, _)| error.clone())?;
        let current = owner.loaded()?;
        let session = Arc::new(Loaded {
            ctx: current.ctx.clone(),
            scope: current.scope,
            children: created.children.clone(),
            required: current.required.clone(),
        });
        *owner.loaded.lock().unwrap() = Some(session.clone());
        let tree = required::<dyn TimelineTreeControl>(
            &session.ctx,
            session.scope.key::<dyn TimelineTreeControl>(),
        )?;
        // The tree provider must be Active before branch subtrees inject it.
        let initialize = async {
            match initialization {
                SessionInitialization::Prepared(branches)
                | SessionInitialization::Restore(branches) => tree.restore(branches).await,
                SessionInitialization::New => tree.create_main().await.map(|_| ()),
            }
        };
        tokio::pin!(initialize);
        let initialize = tokio::select! {
            result = &mut initialize => result,
            _ = owner.closing.cancelled() => {
                tree.begin_close();
                let _ = initialize.await;
                Err(AgentError::Cancelled("session initialization".into()))
            }
            _ = tokio::time::sleep_until(deadline) => {
                // Capture pending plugin dependencies before cancellation removes
                // their rows; the Session deadline also covers branch loading.
                let error = timed_out(host, std::slice::from_ref(&created.view));
                tree.begin_close();
                let _ = initialize.await;
                Err(error)
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
            // Never published: deferred background work ends without starting.
            owner.lifecycle.advance(SessionPhase::Closed);
            let rollback = match started {
                Ok(created) => Some(created),
                Err((_, created)) => created,
            };
            // Unpublished from here on; released once the rollback ends.
            let _session = owner.loaded.lock().unwrap().take();
            let cleanup = match &rollback {
                Some(created) => shutdown_scope(&created.view, &created.children).await,
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
fn required<T: ?Sized + Send + Sync + 'static>(ctx: &Ctx, key: TypeKey) -> Result<Arc<T>> {
    ctx.get_as::<T>(key.clone())
        .ok_or_else(|| AgentError::Build(format!("required service missing: {}", key.describe())))
}
/// Waits until every view is Active; a failure, disposal, cancellation or the
/// deadline ends the wait. `ctx` only describes unmet dependencies on timeout.
pub async fn wait_active(
    ctx: &Ctx,
    views: &[FiberView],
    timeout: std::time::Duration,
    cancel: &CancellationToken,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    for view in views {
        let mut state = view.watch();
        loop {
            let snapshot = state.borrow_and_update().clone();
            match snapshot.state {
                FiberState::Active => break,
                FiberState::Failed => {
                    return Err(snapshot
                        .error
                        .map(AgentError::from)
                        .unwrap_or_else(|| AgentError::Build(format!("{}: Failed", view.name()))));
                }
                FiberState::Disposed => {
                    return Err(AgentError::Build(format!("{}: Disposed", view.name())));
                }
                // Unloading can be initialization rollback in progress; its final
                // state carries the cause.
                FiberState::Pending | FiberState::Loading | FiberState::Unloading => {}
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled("initialization".into())),
                _ = tokio::time::sleep_until(deadline) => return Err(timed_out(ctx, views)),
                changed = state.changed() => {
                    changed.map_err(|_| AgentError::Build(format!("{} ended during initialization", view.name())))?;
                }
            }
        }
    }
    Ok(())
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
                .filter(|d| d.status != rutis::DependencyStatus::Ready)
                .map(|d| format!("{} {:?}", d.key.describe(), d.status))
                .collect();
            format!("{} {:?} [{}]", p.name, p.state, unmet.join(", "))
        })
        .collect();
    AgentError::Build(format!("initialization timed out: {}", pending.join("; ")))
}

/// Assembly checks for plugin authors. A diagnostics snapshot walks the whole
/// process tree, so release builds skip it to keep Session creation constant-time.
fn validate(ctx: &Ctx, scope: PluginId, required: &[FiberView]) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let snapshot = ctx.diagnostics();
    let members = subtree(&snapshot, scope);
    let required: HashSet<_> = required
        .iter()
        .flat_map(|view| subtree(&snapshot, view.id))
        .collect();
    for plugin in snapshot.plugins.iter().filter(|p| members.contains(&p.id)) {
        // Required capabilities must await the children they register in apply.
        if required.contains(&plugin.id) && plugin.state != FiberState::Active {
            return Err(AgentError::Build(format!(
                "{} is not Active after its scope became ready: {:?}",
                plugin.name, plugin.state
            )));
        }
        for access in &plugin.accesses {
            // Scoped reads are strict; this still catches direct `get_as` reads.
            if access.external && !access.declared {
                return Err(AgentError::Build(format!(
                    "{} read undeclared service {}",
                    plugin.name,
                    access.key.describe()
                )));
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
                return Err(AgentError::Build(format!(
                    "{} read a service generation absent from resolved dependencies: {}",
                    plugin.name,
                    access.key.describe()
                )));
            }
        }
    }
    if let Some(binding) = snapshot
        .bindings
        .iter()
        .find(|b| members.contains(&b.provider) && b.key.instance_id().is_none())
    {
        return Err(AgentError::Build(format!(
            "scoped service provided without an instance key: {}",
            binding.key.describe()
        )));
    }
    Ok(())
}

/// Plugin IDs of `root` and its descendants in one diagnostics snapshot.
pub(crate) fn subtree(snapshot: &rutis::RuntimeDiagnostics, root: PluginId) -> HashSet<PluginId> {
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
) -> rutis::RuntimeDiagnostics {
    let members: HashSet<_> = children.iter().map(|view| view.id).chain([scope]).collect();
    let mut snapshot = ctx.diagnostics();
    snapshot.plugins.retain(|p| members.contains(&p.id));
    snapshot.bindings.retain(|b| members.contains(&b.provider));
    snapshot
}

/// Permanently closes a scope and returns only cleanup failures.
///
/// rutis reports a plugin's load result again from shutdown under the same
/// identity, with the apply error first and its rollback errors after it. A
/// plugin that had Failed already surfaced that whole error as the build cause.
/// A plugin still loading had its apply rejected by the cancellation itself;
/// that rejection is dropped while its rollback errors remain cleanup failures.
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
            (_, CordisError::Aggregate { errors }) => known.extend(errors.first().cloned()),
            _ => known.push(error.clone()),
        }
    }
    match without(error, &known) {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}
fn without(error: Arc<CordisError>, known: &[Arc<CordisError>]) -> Option<Arc<CordisError>> {
    if known.iter().any(|k| Arc::ptr_eq(k, &error)) {
        return None;
    }
    let CordisError::Aggregate { errors } = &*error else {
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
        _ => Some(Arc::new(CordisError::Aggregate { errors: rest })),
    }
}
