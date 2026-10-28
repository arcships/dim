//! The Session's tools: the fixed table of host tools from
//! `SessionConfig.tools`, the tools parts register and the dynamic sets of
//! the tool sources parts register, see
//! `docs/architecture/rust-agent/plugins/ToolRegistry.md`. A part in any
//! language registers a tool here; a registration is withdrawn when its part
//! unloads ([rutis usage R6](../../../docs/design/rutis-usage.md#r6-contributions-register-with-their-owner)).
use crate::host::*;
use dim_agent_runtime::{
    AgentError, Names, Registration, Registrations, SessionConfig, SessionIdentity,
    SessionLifecycle,
};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, InstanceId, Key, TypeKey};
use rutis::{Ctx, Error, Event};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reference, Reply, Value, host_key_in, settle,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// A provider updated its sets; emitted under its Session's
/// [`tool_provider_changes`] name.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ToolProviderChanged {
    /// The Session's [`tool_provider_changes`] name; picks the name only.
    #[serde(skip)]
    pub name: String,
}
impl Event for ToolProviderChanged {
    const NAME: &'static str = "";
    type Value = ();
    fn name(&self) -> &str {
        &self.name
    }
}

/// The private name a Session's tool providers announce changes under. The
/// bus is flat, so the name carries the Session instance; callers use this
/// constructor rather than spelling the name.
pub fn tool_provider_changes(session: InstanceId) -> String {
    format!("dim/tools/{session}/provider-changed")
}

pub trait ToolHandler: Send + Sync + 'static {
    /// Resolve a filesystem write's target without effects before admission.
    fn resolve_write(&self, call: ToolCall, _: CancellationToken) -> HostFuture<'_, ToolCall> {
        Box::pin(async move { Ok(call) })
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult>;
}
/// A tool and its handler: a host tool, or one a part registers with
/// [`ToolRegistry::register`].
pub struct ToolContribution {
    pub definition: ToolDefinition,
    pub handler: Arc<dyn ToolHandler>,
}

/// After replacing its sets, a provider emits `ToolProviderChanged` under
/// [`tool_provider_changes`] for its Session instance, from its own context.
pub trait ToolProvider: Send + Sync + 'static {
    /// Every complete set this provider currently offers, one per key.
    fn sets(&self) -> Vec<Arc<ToolSourceSet>>;
}

/// One complete version of a source's tools. The handlers are bound to this
/// version's resources, which live as long as the set.
pub struct ToolSourceSet {
    /// `<registered name>` or `<registered name>:<instance>`.
    pub key: String,
    /// Never reused for this key, also across registry instances and
    /// processes: a version persisted in `tool_sources` names one set.
    pub version: String,
    pub tools: Vec<Arc<ToolContribution>>,
}

/// Read-only view of the source statuses for hosts; unlike `ToolRegistry`
/// it exposes no handler, so a Session handle may obtain it.
pub struct ToolSourcesView(watch::Receiver<ToolSourcesState>);
impl ToolSourcesView {
    pub fn get(&self) -> ToolSourcesState {
        self.0.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<ToolSourcesState> {
        self.0.clone()
    }
}

/// What the registry did with each source key, for management views.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSourcesState(pub BTreeMap<String, ToolSourceStatus>);
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSourceStatus {
    pub adopted: Option<String>,
    pub rejected: Option<ToolSourceRejection>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSourceRejection {
    pub version: String,
    pub reason: String,
}

#[derive(Default)]
struct Sources {
    current: BTreeMap<String, Arc<ToolSourceSet>>,
    /// Replaced versions, reachable while someone still holds them.
    replaced: HashMap<(String, String), Weak<ToolSourceSet>>,
    /// Every version ever held, so none is adopted twice.
    held: HashSet<(String, String)>,
}

/// The source of registered tools; its keys are `#contributed:<tool>`.
/// It starts with `#`, which no source name may contain, so it never meets a
/// source's key.
const CONTRIBUTED: &str = "#contributed";

/// The set a registered tool is offered as: one per registration, so a
/// request keeps the generation its prefix adopted. Each version is a new
/// UUID, so a restored prefix never names a later generation.
struct Offered {
    registration: Arc<Registration<Arc<ToolContribution>>>,
    set: Arc<ToolSourceSet>,
}

pub struct ToolRegistry {
    /// Every host tool, in the host's order; a part cannot register one of
    /// these names.
    host: Vec<Arc<ToolContribution>>,
    /// `SessionConfig.tools`, the Session's mount list: the host and
    /// registered tools it declares, in this order. `None` declares every
    /// host tool and every registered tool.
    configured: Option<Vec<String>>,
    tools: Arc<Registrations<Arc<ToolContribution>>>,
    providers: Arc<Registrations<Arc<dyn ToolProvider>>>,
    /// The set each registered tool is offered as.
    offered: Mutex<HashMap<String, Offered>>,
    /// Configured names nothing has registered, reported once each.
    missing: Mutex<HashSet<String>>,
    session_id: String,
    config: Arc<SessionConfig>,
    /// Serializes refreshes from read to commit, so an earlier read never
    /// overwrites a later one.
    refreshing: Mutex<()>,
    sources: Mutex<Sources>,
    state: watch::Sender<ToolSourcesState>,
}
impl ToolRegistry {
    /// Registers `tool` under its name on behalf of the part that owns
    /// `ctx`, until that part unloads. It is offered from the next read when
    /// the Session has no mount list or its list names it (#364); Permission
    /// still admits each call. The name must follow the tool name grammar,
    /// and neither a host tool nor another registration may have it.
    pub fn register(&self, ctx: &Ctx, tool: ToolContribution) -> Result<(), Error> {
        let name = tool.definition.name.clone();
        if !valid_tool_name(&name) {
            return Err(invalid(format!("invalid tool name {name}")));
        }
        if self.is_host(&name) {
            return Err(invalid(format!("tool {name} is a host tool")));
        }
        self.tools.register(ctx, name, Arc::new(tool)).map(|_| ())
    }
    /// Registers a dynamic tool source under `name` on behalf of the part
    /// that owns `ctx`, until that part unloads. Its keys are `<name>` or
    /// `<name>:<instance>`; the name follows the tool name grammar, so they
    /// never meet the registry's own `#contributed:<tool>` keys.
    pub fn register_source(
        &self,
        ctx: &Ctx,
        name: impl Into<String>,
        source: Arc<dyn ToolProvider>,
    ) -> Result<(), Error> {
        let name = name.into();
        if !valid_tool_name(&name) {
            return Err(invalid(format!(
                "tool source name {name} must match [A-Za-z0-9_-]{{1,64}}"
            )));
        }
        self.providers.register(ctx, name, source).map(|_| ())
    }
    /// Every declaration, after taking in what is registered now (a
    /// request's read, as `snapshot`): the mounted host and registered tools
    /// (in mount list order, or the host's tools then the registered ones by
    /// name), then other source tools by key, in set order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.snapshot().0
    }
    fn declared(&self, sources: &Sources) -> Vec<ToolDefinition> {
        let mounted: Vec<Arc<ToolContribution>> = match &self.configured {
            Some(names) => names
                .iter()
                .filter_map(|name| {
                    self.fixed(name).or_else(|| {
                        sources
                            .current
                            .get(&format!("{CONTRIBUTED}:{name}"))
                            .and_then(|set| set.tools.first().cloned())
                    })
                })
                .collect(),
            None => self
                .host
                .iter()
                .cloned()
                .chain(
                    sources
                        .current
                        .values()
                        .filter(|set| is_contributed(&set.key))
                        .flat_map(|set| set.tools.iter().cloned()),
                )
                .collect(),
        };
        let other = sources
            .current
            .values()
            .filter(|set| !is_contributed(&set.key))
            .flat_map(|set| set.tools.iter().cloned());
        mounted
            .into_iter()
            .chain(other)
            .map(|tool| tool.definition.clone())
            .collect()
    }
    /// Whether the Session mounts `name`.
    fn mounts(&self, name: &str) -> bool {
        self.configured
            .as_ref()
            .is_none_or(|names| names.iter().any(|mounted| mounted == name))
    }
    fn is_host(&self, name: &str) -> bool {
        self.host.iter().any(|tool| tool.definition.name == name)
    }
    /// Declarations and the source sets they were built from, read together,
    /// after taking in what is registered now.
    pub fn snapshot(&self) -> (Vec<ToolDefinition>, Vec<Arc<ToolSourceSet>>) {
        self.refresh();
        let sources = self.sources.lock().unwrap();
        let sets: Vec<_> = sources.current.values().cloned().collect();
        (self.declared(&sources), sets)
    }
    /// The mounted host tool of this name.
    pub fn fixed(&self, name: &str) -> Option<Arc<ToolContribution>> {
        if !self.mounts(name) {
            return None;
        }
        self.host
            .iter()
            .find(|tool| tool.definition.name == name)
            .cloned()
    }
    /// The current version of a registered tool.
    pub fn contributed(&self, name: &str) -> Option<Arc<ToolContribution>> {
        let sources = self.sources.lock().unwrap();
        let set = sources.current.get(&format!("{CONTRIBUTED}:{name}"))?;
        set.tools.first().cloned()
    }
    /// The current sets, by key.
    pub fn sources(&self) -> Vec<Arc<ToolSourceSet>> {
        self.sources
            .lock()
            .unwrap()
            .current
            .values()
            .cloned()
            .collect()
    }
    /// The current or a still-held replaced version of a key.
    pub fn source(&self, key: &str, version: &str) -> Option<Arc<ToolSourceSet>> {
        let sources = self.sources.lock().unwrap();
        match sources.current.get(key) {
            Some(set) if set.version == version => Some(set.clone()),
            _ => sources
                .replaced
                .get(&(key.to_owned(), version.to_owned()))
                .and_then(Weak::upgrade),
        }
    }
    pub fn sources_state(&self) -> watch::Receiver<ToolSourcesState> {
        self.state.subscribe()
    }

    /// The registered tool sets offered now: one per mounted registration.
    /// A name the mount list gives that is neither a host tool nor
    /// registered is reported once, and offered once something registers it.
    fn contributed_sets(&self) -> Vec<Arc<ToolSourceSet>> {
        let registered: Vec<_> = self
            .tools
            .all()
            .into_iter()
            .filter(|registration| self.mounts(&registration.name))
            .collect();
        let mut missing = self.missing.lock().unwrap();
        for name in self.configured.iter().flatten() {
            let absent = !self.is_host(name)
                && !registered
                    .iter()
                    .any(|registration| registration.name == *name);
            if !absent {
                missing.remove(name);
            } else if missing.insert(name.clone()) {
                tracing::info_span!(
                    "tool.attach",
                    session_id = %self.session_id,
                    outcome = %format_args!("unavailable:{name}"),
                )
                .in_scope(|| tracing::debug!(tool = name, "no part has registered the tool"));
            }
        }
        let mut offered = self.offered.lock().unwrap();
        offered.retain(|name, _| {
            registered
                .iter()
                .any(|registration| registration.name == *name)
        });
        registered
            .into_iter()
            .map(|registration| {
                let name = registration.name.clone();
                let current = offered
                    .get(&name)
                    .is_some_and(|last| Arc::ptr_eq(&last.registration, &registration));
                if !current {
                    let set = Arc::new(ToolSourceSet {
                        key: format!("{CONTRIBUTED}:{name}"),
                        version: uuid::Uuid::new_v4().to_string(),
                        tools: vec![registration.value.clone()],
                    });
                    offered.insert(name.clone(), Offered { registration, set });
                }
                offered[&name].set.clone()
            })
            .collect()
    }

    /// Applies what is registered now: each changed key is validated as a
    /// whole and replaced atomically; a missing key is withdrawn. Every
    /// source is read on any change, so a set rejected for a name another
    /// source held is retried once that name is free.
    fn refresh(&self) {
        let _serial = self.refreshing.lock().unwrap();
        let registered: HashSet<String> = self
            .tools
            .all()
            .iter()
            .map(|registration| registration.name.clone())
            .collect();
        let providers = self.providers.all();
        let offered: Vec<(&str, BTreeMap<String, Arc<ToolSourceSet>>)> =
            std::iter::once((CONTRIBUTED, self.contributed_sets()))
                .chain(
                    providers
                        .iter()
                        .map(|provider| (provider.name.as_str(), provider.value.sets())),
                )
                .map(|(name, sets)| {
                    let sets = sets.into_iter().map(|set| (set.key.clone(), set));
                    (name, sets.collect())
                })
                .collect();
        let mut sources = self.sources.lock().unwrap();
        let mut statuses = self.state.borrow().clone();
        let withdrawn: Vec<String> = sources
            .current
            .keys()
            .filter(|key| {
                !offered
                    .iter()
                    .any(|(name, sets)| owns(name, key) && sets.contains_key(*key))
            })
            .cloned()
            .collect();
        for key in withdrawn {
            let set = sources.current.remove(&key).expect("current key");
            sources
                .replaced
                .insert((key, set.version.clone()), Arc::downgrade(&set));
        }
        statuses
            .0
            .retain(|key, _| offered.iter().any(|(_, sets)| sets.contains_key(key)));
        let mut progressed = true;
        while progressed {
            progressed = false;
            for (name, set) in offered
                .iter()
                .flat_map(|(name, sets)| sets.values().map(move |set| (*name, set)))
            {
                let key = set.key.clone();
                if sources
                    .current
                    .get(&key)
                    .is_some_and(|current| current.version == set.version)
                {
                    continue;
                }
                match self.validate(name, &sources, &registered, set) {
                    Ok(()) => {
                        progressed = true;
                        if let Some(old) = sources.current.insert(key.clone(), set.clone()) {
                            sources
                                .replaced
                                .insert((key.clone(), old.version.clone()), Arc::downgrade(&old));
                        }
                        sources.held.insert((key.clone(), set.version.clone()));
                        statuses.0.insert(
                            key,
                            ToolSourceStatus {
                                adopted: Some(set.version.clone()),
                                rejected: None,
                            },
                        );
                    }
                    Err(reason) => {
                        let rejection = ToolSourceRejection {
                            version: set.version.clone(),
                            reason,
                        };
                        let status = statuses.0.entry(key.clone()).or_default();
                        if status.rejected.as_ref() != Some(&rejection) {
                            tracing::warn!(
                                key,
                                version = set.version,
                                reason = rejection.reason,
                                "tool source rejected"
                            );
                            status.rejected = Some(rejection);
                        }
                    }
                }
            }
        }
        sources.replaced.retain(|_, set| set.strong_count() > 0);
        drop(sources);
        self.state.send_if_modified(|state| {
            let modified = *state != statuses;
            *state = statuses;
            modified
        });
    }

    /// A configured name, a host tool's name and a registered tool's name
    /// are offered only by their own owner, never by another source.
    fn validate(
        &self,
        name: &str,
        sources: &Sources,
        registered: &HashSet<String>,
        set: &ToolSourceSet,
    ) -> Result<(), String> {
        if !owns(name, &set.key) {
            return Err(format!("key {} does not belong to source {name}", set.key));
        }
        if sources
            .held
            .contains(&(set.key.clone(), set.version.clone()))
        {
            return Err(format!("version {} was already used", set.version));
        }
        let mut names = HashSet::new();
        for tool in &set.tools {
            let tool_name = &tool.definition.name;
            if !valid_tool_name(tool_name) {
                return Err(format!("invalid tool name {tool_name}"));
            }
            if !names.insert(tool_name.as_str()) {
                return Err(format!("duplicate tool {tool_name}"));
            }
            let reserved = self
                .configured
                .iter()
                .flatten()
                .any(|name| name == tool_name)
                || self.is_host(tool_name)
                || registered.contains(tool_name);
            if reserved && set.key != format!("{CONTRIBUTED}:{tool_name}") {
                return Err(format!("tool {tool_name} is a fixed tool"));
            }
            if let Some(other) = sources.current.values().find(|other| {
                other.key != set.key
                    && other
                        .tools
                        .iter()
                        .any(|tool| tool.definition.name == *tool_name)
            }) {
                return Err(format!("tool {tool_name} is provided by {}", other.key));
            }
        }
        Ok(())
    }

    /// Registers a tool of another language: `declaration` is its
    /// definition and `execute` the caller's function, called with the call
    /// and a Promise that settles when the call is cancelled.
    fn register_foreign(
        &self,
        ctx: &Ctx,
        declaration: serde_json::Value,
        execute: Reference,
    ) -> Result<(), Error> {
        let definition: ToolDefinition = serde_json::from_value(declaration)
            .map_err(|error| invalid(format!("invalid tool declaration: {error}")))?;
        if definition.risk.as_ref().is_none_or(|risk| risk.is_empty()) {
            return Err(invalid(format!(
                "tool {} must declare its risk",
                definition.name
            )));
        }
        self.register(
            ctx,
            ToolContribution {
                definition,
                handler: Arc::new(ForeignTool {
                    execute,
                    generation: ctx.cancellation_token(),
                    session_id: self.session_id.clone(),
                    config: self.config.clone(),
                }),
            },
        )
    }
}

fn invalid(message: String) -> Error {
    AgentError::InvalidInput(message).into()
}

/// Whether a source key is a registered tool's. Like a host tool, it is
/// declared by a configuration without a tool part through `bind_current`.
pub fn is_contributed(key: &str) -> bool {
    owns(CONTRIBUTED, key)
}

fn owns(name: &str, key: &str) -> bool {
    key == name
        || key
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(':'))
}

pub fn valid_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

struct HostTool(Arc<ToolHost>);
impl ToolHandler for HostTool {
    fn resolve_write(&self, call: ToolCall, cancel: CancellationToken) -> HostFuture<'_, ToolCall> {
        self.0.0.resolve_write(call, cancel)
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        self.0.0.execute(call, context, cancel)
    }
}

/// A tool of another language: `execute` is the function its row
/// registered, and `generation` that row's generation. Once the row starts
/// unloading, a call is not made, and a call in flight is cut short, as it
/// may already have taken effect.
struct ForeignTool {
    execute: Reference,
    generation: CancellationToken,
    session_id: String,
    config: Arc<SessionConfig>,
}
impl ToolHandler for ForeignTool {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.generation.is_cancelled() {
                return Err(HostError::failed(
                    "tool provider generation is unloading; this call was not made. The tool is offered again once its provider is active; a later request uses the current provider.",
                ));
            }
            let cancel = cancel.child_token();
            let _finish = cancel.clone().drop_guard();
            let cancelled = Value::future(async move {
                cancel.cancelled().await;
                Ok(Value::Undefined)
            });
            let args = json!({
                "sessionId": self.session_id,
                "branchId": context.branch_id,
                "turnId": context.turn_id,
                "callId": call.id,
                "arguments": call.input,
                "config": self.config,
            });
            let reply = async {
                let reply = self
                    .execute
                    .call_async(Value::List(vec![args.into(), cancelled]))
                    .await?;
                settle(reply).await?.json()
            };
            let result = tokio::select! {
                biased;
                _ = self.generation.cancelled() => return Err(HostError::failed("tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider.")),
                result = reply => result,
            }
            .map_err(|error| HostError::failed(error.to_string()))?;
            serde_json::from_value(result)
                .map_err(|error| HostError::failed(format!("invalid tool result: {error}")))
        })
    }
}

/// `toolRegistry` for other languages: `register(declaration, execute)`
/// registers a tool for as long as the calling row is loaded.
struct ForeignRegistry(Arc<ToolRegistry>);
impl HostDispatch for ForeignRegistry {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(json!({ "register": "async" }))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method != "register" {
            return Err(BridgeError::Value(format!(
                "toolRegistry has no method {method}"
            )));
        }
        let caller = rutis_bridge::session::caller_row()
            .map(|row| row.ctx().clone())
            .ok_or_else(|| {
                BridgeError::Value(
                    "register needs the calling row's context: only a row of a language runtime can call it"
                        .into(),
                )
            })?;
        let mut args = match args {
            Value::Undefined => Vec::new(),
            args => args.list()?,
        };
        if args.len() > 2 {
            return Err(BridgeError::Value(format!(
                "expected at most 2 arguments, received {}",
                args.len()
            )));
        }
        args.resize(2, Value::Undefined);
        let mut args = args.into_iter();
        let declaration = args.next().expect("two arguments").json()?;
        let Some(Value::Reference(execute)) = args.next().filter(
            |execute| matches!(execute, Value::Reference(reference) if reference.is_function()),
        ) else {
            return Err(BridgeError::Value(
                "register needs the tool's declaration and its execute function".into(),
            ));
        };
        let registered = self
            .0
            .register_foreign(&caller, declaration, execute)
            .map(|()| Value::Undefined)
            .map_err(|error| BridgeError::Value(error.to_string()));
        Ok(Value::future(async move { registered }))
    }
}

pub fn definition(name: &str, description: &str, schema: serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: serde_json::from_value(schema).expect("built-in schema is valid"),
        source: ToolSource::Builtin,
        risk: None,
        metadata: None,
        file_changes: None,
    }
}

pub struct ToolRegistryPlugin {
    session: InstanceId,
    registry: Key<ToolRegistry>,
    view: Key<ToolSourcesView>,
    host: Key<ToolHost>,
    config: Key<SessionConfig>,
    identity: Key<SessionIdentity>,
    lifecycle: Key<SessionLifecycle>,
}
impl ToolRegistryPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            session: names.instance(dim_agent_runtime::SESSION_GROUP)?,
            registry: names.key()?,
            view: names.key()?,
            host: names.key()?,
            config: names.key()?,
            identity: names.key()?,
            lifecycle: names.key()?,
        })
    }
}
impl DynPlugin for ToolRegistryPlugin {
    fn name(&self) -> &str {
        "ToolRegistry"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.host.erase(),
            self.config.erase(),
            self.identity.erase(),
            self.lifecycle.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let host = ctx.require_at(&self.host)?;
            let config = ctx.require_at(&self.config)?;
            let identity = ctx.require_at(&self.identity)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let host_tools = host
                .0
                .definitions()
                .into_iter()
                .map(|definition| {
                    Arc::new(ToolContribution {
                        definition,
                        handler: Arc::new(HostTool(host.clone())),
                    })
                })
                .collect();
            let mut selected = HashSet::new();
            if let Some(name) = config
                .tools
                .iter()
                .flatten()
                .find(|name| !selected.insert(*name))
            {
                return Err(AgentError::InvalidInput(format!("duplicate tool {name}")).into());
            }
            let registry = Arc::new(ToolRegistry {
                host: host_tools,
                configured: config.tools.clone(),
                tools: Arc::default(),
                providers: Arc::default(),
                offered: Mutex::default(),
                missing: Mutex::default(),
                session_id: identity.session_id.clone(),
                config: config.clone(),
                refreshing: Mutex::default(),
                sources: Mutex::default(),
                state: watch::Sender::new(ToolSourcesState::default()),
            });
            let listener = registry.clone();
            ctx.listen::<ToolProviderChanged>()
                .name(tool_provider_changes(self.session))
                .on(move |_, _| {
                    let registry = listener.clone();
                    async move {
                        registry.refresh();
                        Ok(None)
                    }
                })?;
            let mut tools = registry.tools.changes();
            let mut providers = registry.providers.changes();
            let refresher = registry.clone();
            let task = tokio::spawn(async move {
                if !lifecycle.activated().await {
                    return;
                }
                loop {
                    tokio::select! {
                        biased;
                        _ = lifecycle.closing() => return,
                        _ = tools.changed() => {}
                        _ = providers.changed() => {}
                    }
                    refresher.refresh();
                }
            });
            ctx.effect(move || async move {
                task.abort();
                let _ = task.await;
                Ok(())
            })?;
            registry.refresh();
            ctx.provide_at(
                self.view.clone(),
                Arc::new(ToolSourcesView(registry.sources_state())),
            )?;
            ctx.provide_at::<dyn HostDispatch>(
                host_key_in(
                    &dim_agent_runtime::names::name_of::<ToolRegistry>(),
                    self.session,
                ),
                Arc::new(ForeignRegistry(registry.clone())),
            )?;
            ctx.provide_at(self.registry.clone(), registry)?;
            Ok(Effect::Done)
        })
    }
}
