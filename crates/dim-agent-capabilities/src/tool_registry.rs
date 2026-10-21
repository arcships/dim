//! The Session's tools: the fixed table of host tools from
//! `SessionConfig.tools`, the contributed tools of capabilities and the
//! dynamic sets of registered `SessionToolSource`s, see
//! `docs/architecture/rust-agent/plugins/ToolRegistry.md`.
use crate::host::*;
use dim_agent_runtime::{
    AgentError, Capability, Contributions, Diagnostics, InSession, KeysOf, SessionConfig,
    SessionIdentity, SessionLifecycle, SessionScope, SessionService, ToolNames,
};
use dim_agent_types::*;
use rutis::{
    BoxFuture, CordisError, Ctx, Effect, Event, EventKey, FiberState, FiberStatusChanged, Listener,
    ServiceChanged, TypedPlugin,
};
use rutis_bridge::session::{HostDispatch, Value, host_key_in, settle};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

/// A provider updated its sets; emitted on its Session instance's channel.
pub struct ToolProviderChanged;
impl Event for ToolProviderChanged {
    const NAME: &'static str = "tools.provider_changed";
    type Value = ();
}

/// The registry replaced or withdrew sets; emitted after its state is updated.
pub struct ToolsChanged;
impl Event for ToolsChanged {
    const NAME: &'static str = "tools.changed";
    type Value = ();
}

pub trait ToolHandler: Send + Sync + 'static {
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult>;
}
/// A tool a Session capability provides under
/// `SessionScope::named_key::<ToolContribution>(name)` for a name registered
/// with `AgentBuilder::session_tool`.
pub struct ToolContribution {
    pub definition: ToolDefinition,
    pub handler: Arc<dyn ToolHandler>,
}
impl SessionService for ToolContribution {}

/// A dynamic tool source a Session capability provides under
/// `SessionScope::named_key::<SessionToolSource>(name)` for a name
/// registered with `AgentBuilder::session_tool_source`.
pub struct SessionToolSource(pub Arc<dyn ToolProvider>);
impl SessionService for SessionToolSource {}

/// After replacing its sets, a provider emits `ToolProviderChanged` with
/// `EventKey::of().instance(session_scope.instance)` from its own context.
pub trait ToolProvider: Send + Sync + 'static {
    /// Every complete set this provider currently offers, one per key.
    fn sets(&self) -> Vec<Arc<ToolSourceSet>>;
}

/// One complete version of a source's tools. The handlers are bound to this
/// version's resources, which live as long as the set.
pub struct ToolSourceSet {
    /// `<registered name>` or `<registered name>:<instance>`.
    pub key: String,
    /// Never reused for this key.
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

/// The source of contributed tools; its keys are `contributed:<tool>`.
/// It starts with `#`, which no registered source name may contain, so it
/// never meets a registered source's key.
const CONTRIBUTED: &str = "#contributed";

pub struct ToolRegistry {
    ctx: Ctx,
    scope: SessionScope,
    /// Configured host tools, in `SessionConfig.tools` order.
    fixed: Vec<Arc<ToolContribution>>,
    /// `SessionConfig.tools`: the declaration order of host and contributed
    /// tools.
    configured: Vec<String>,
    /// Configured tools and every registered contributor reserve names; a
    /// contributed name is offered only by its own `contributed:` set.
    reserved: HashSet<String>,
    /// Registered name and provider of every dynamic source, after the
    /// contributed tools.
    providers: Vec<(String, Arc<dyn ToolProvider>)>,
    contributed: Arc<Contributed>,
    /// Serializes refreshes from read to commit, so an earlier read never
    /// overwrites a later one.
    refreshing: Mutex<()>,
    sources: Mutex<Sources>,
    state: watch::Sender<ToolSourcesState>,
}
impl ToolRegistry {
    /// Every declaration: configured host and contributed tools in
    /// `SessionConfig.tools` order, then other source tools by key, in set
    /// order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let sources = self.sources.lock().unwrap();
        self.declared(&sources)
    }
    fn declared(&self, sources: &Sources) -> Vec<ToolDefinition> {
        let configured = self.configured.iter().filter_map(|name| {
            self.fixed
                .iter()
                .find(|tool| tool.definition.name == *name)
                .or_else(|| {
                    sources
                        .current
                        .get(&format!("{CONTRIBUTED}:{name}"))
                        .and_then(|set| set.tools.first())
                })
        });
        let other = sources
            .current
            .values()
            .filter(|set| !is_contributed(&set.key))
            .flat_map(|set| set.tools.iter());
        configured
            .chain(other)
            .map(|tool| tool.definition.clone())
            .collect()
    }
    /// Declarations and the source sets they were built from, read together,
    /// after taking in every contributor active now.
    pub fn snapshot(&self) -> (Vec<ToolDefinition>, Vec<Arc<ToolSourceSet>>) {
        self.contributed.reading.store(true, Ordering::Release);
        self.refresh();
        let sources = self.sources.lock().unwrap();
        let sets: Vec<_> = sources.current.values().cloned().collect();
        (self.declared(&sources), sets)
    }
    /// The host tool of this name.
    pub fn fixed(&self, name: &str) -> Option<Arc<ToolContribution>> {
        self.fixed
            .iter()
            .find(|tool| tool.definition.name == name)
            .cloned()
    }
    /// The current version of a contributed tool.
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

    /// Applies what every provider offers now: each changed key is
    /// validated as a whole and replaced atomically; a missing key is
    /// withdrawn. All providers are read on any change, so a set rejected for
    /// a name another source held is retried once that name is free.
    fn refresh(&self) {
        let _serial = self.refreshing.lock().unwrap();
        let offered: Vec<(&str, BTreeMap<String, Arc<ToolSourceSet>>)> = self
            .providers
            .iter()
            .map(|(name, provider)| {
                (
                    name.as_str(),
                    provider
                        .sets()
                        .into_iter()
                        .map(|set| (set.key.clone(), set))
                        .collect(),
                )
            })
            .collect();
        let mut sources = self.sources.lock().unwrap();
        let mut statuses = self.state.borrow().clone();
        let mut changed = false;
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
            changed = true;
        }
        statuses
            .0
            .retain(|key, _| offered.iter().any(|(_, sets)| sets.contains_key(key)));
        // Repeat until nothing more is adopted: a set rejected for a name that
        // a later replacement in the same pass frees is adopted now.
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
                match self.validate(name, &sources, set) {
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
                        changed = true;
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
        if changed
            && let Err(error) = self.ctx.events().emit(
                &self.ctx,
                &EventKey::<ToolsChanged>::of().instance(self.scope.instance),
                Arc::new(ToolsChanged),
            )
        {
            tracing::warn!(%error, "failed to publish tools.changed");
        }
    }

    fn validate(&self, name: &str, sources: &Sources, set: &ToolSourceSet) -> Result<(), String> {
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
            if self.reserved.contains(tool_name) && set.key != format!("{CONTRIBUTED}:{tool_name}")
            {
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
}

/// A provider entered or left Active, or a service binding changed: a
/// contributed tool may have appeared, been replaced or retired. The refresh
/// runs in the registry's own task, never in the emitter's callback.
struct ContributorsChanged(Arc<Notify>);
impl Listener<FiberStatusChanged> for ContributorsChanged {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        event: &'a FiberStatusChanged,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        Box::pin(async move {
            if event.from == FiberState::Active || event.to == FiberState::Active {
                self.0.notify_one();
            }
            Ok(None)
        })
    }
}
impl Listener<ServiceChanged> for ContributorsChanged {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _: &'a ServiceChanged,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        Box::pin(async move {
            self.0.notify_one();
            Ok(None)
        })
    }
}

/// The capability that currently provides a contributed tool.
#[derive(Clone)]
enum Contributor {
    Native(Arc<ToolContribution>),
    Foreign(Arc<dyn HostDispatch>),
}
impl Contributor {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Native(a), Self::Native(b)) => Arc::ptr_eq(a, b),
            (Self::Foreign(a), Self::Foreign(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
/// What was last seen for one contributed tool: the contributor (held, so its
/// identity is not reused) and the set built from it, if its declaration was
/// valid.
struct Attached {
    contributor: Option<Contributor>,
    set: Option<Arc<ToolSourceSet>>,
}

/// Offers each configured contributed tool whose capability is active now, as
/// one set per tool. A set's version changes only with its contributor, so a
/// request keeps the generation its prefix adopted.
struct Contributed {
    ctx: Ctx,
    scope: SessionScope,
    names: Vec<String>,
    identity: Arc<SessionIdentity>,
    config: Arc<SessionConfig>,
    diagnostics: Arc<Diagnostics>,
    attached: Mutex<HashMap<String, Attached>>,
    versions: AtomicU64,
    /// Set by the first snapshot, after the registry's own `apply`: it may
    /// not read services it does not declare while it applies.
    reading: AtomicBool,
}
impl Contributed {
    fn contributor(&self, name: &str) -> Option<Contributor> {
        if let Some(tool) = self
            .ctx
            .get_as::<ToolContribution>(self.scope.named_key::<ToolContribution>(name))
        {
            return Some(Contributor::Native(tool));
        }
        let service = self.ctx.get_as::<dyn HostDispatch>(host_key_in(
            &format!("toolContribution:{name}"),
            self.scope.instance,
        ))?;
        // A generation that is unloading no longer offers its tool.
        let retiring = self
            .ctx
            .get_as::<CancellationToken>(
                rutis::TypeKey::keyed_dynamic::<CancellationToken>(format!(
                    "toolContribution:{name}"
                ))
                .with_instance(self.scope.instance),
            )
            .is_none_or(|generation| generation.is_cancelled());
        (!retiring).then_some(Contributor::Foreign(service))
    }
    fn attach(&self, name: &str, contributor: &Contributor) -> Option<Arc<ToolSourceSet>> {
        let tool = match contributor {
            Contributor::Native(tool) => tool.clone(),
            Contributor::Foreign(service) => match foreign_tool(
                service.clone(),
                name,
                &self.identity,
                self.config.clone(),
                &self.ctx,
                &self.scope,
            ) {
                Ok(tool) => tool,
                Err(error) => {
                    let span = self.diagnostics.span("tool.attach", None, None, None);
                    span.record("outcome", format!("invalid-declaration:{name}"));
                    tracing::warn!(tool = name, %error, "optional tool declaration rejected");
                    return None;
                }
            },
        };
        Some(Arc::new(ToolSourceSet {
            key: format!("{CONTRIBUTED}:{name}"),
            version: self.versions.fetch_add(1, Ordering::Relaxed).to_string(),
            tools: vec![tool],
        }))
    }
}
impl ToolProvider for Contributed {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        if !self.reading.load(Ordering::Acquire) {
            return Vec::new();
        }
        let mut attached = self.attached.lock().unwrap();
        let mut sets = Vec::new();
        for name in &self.names {
            let contributor = self.contributor(name);
            let last = attached.get(name);
            let unchanged = match (
                &contributor,
                last.and_then(|last| last.contributor.as_ref()),
            ) {
                (Some(now), Some(before)) => now.same(before),
                (None, None) => last.is_some(),
                _ => false,
            };
            if !unchanged {
                let set = match &contributor {
                    Some(contributor) => self.attach(name, contributor),
                    None => {
                        let span = self.diagnostics.span("tool.attach", None, None, None);
                        span.record("outcome", format!("unavailable:{name}"));
                        tracing::debug!(tool = name, "contributed tool provider is not active");
                        None
                    }
                };
                attached.insert(name.clone(), Attached { contributor, set });
            }
            if let Some(set) = &attached[name].set {
                sets.push(set.clone());
            }
        }
        sets
    }
}

impl Listener<ToolProviderChanged> for Arc<ToolRegistry> {
    fn call<'a>(
        &'a self,
        _ctx: &'a Ctx,
        _: &'a ToolProviderChanged,
    ) -> BoxFuture<'a, Result<Option<()>, CordisError>> {
        Box::pin(async move {
            self.refresh();
            Ok(None)
        })
    }
}

/// Whether a source key is a contributed tool's. Like a host tool, it is
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
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        self.0.0.execute(call, context, cancel)
    }
}

struct ForeignTool {
    service: Arc<dyn HostDispatch>,
    generation: Arc<CancellationToken>,
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
            let reply = self
                .service
                .invoke("execute", Value::List(vec![args.into(), cancelled]))
                .map_err(|error| HostError::failed(error.to_string()))?;
            let result = tokio::select! {
                biased;
                _ = self.generation.cancelled() => return Err(HostError::failed("tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider.")),
                result = settle(reply) => result,
            }
                .and_then(|value| value.json())
                .map_err(|error| HostError::failed(error.to_string()))?;
            serde_json::from_value(result)
                .map_err(|error| HostError::failed(format!("invalid tool result: {error}")))
        })
    }
}

fn foreign_tool(
    service: Arc<dyn HostDispatch>,
    name: &str,
    identity: &SessionIdentity,
    config: Arc<SessionConfig>,
    ctx: &Ctx,
    scope: &SessionScope,
) -> Result<Arc<ToolContribution>, String> {
    let generation = ctx
        .get_as::<CancellationToken>(
            rutis::TypeKey::keyed_dynamic::<CancellationToken>(format!("toolContribution:{name}"))
                .with_instance(scope.instance),
        )
        .ok_or("foreign tool has no owning generation")?;
    let value = service
        .invoke("declaration", Value::List(vec![]))
        .and_then(|value| value.json())
        .map_err(|error| error.to_string())?;
    let definition: ToolDefinition =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if definition.name != name || definition.risk.as_ref().is_none_or(|risk| risk.is_empty()) {
        return Err("tool declaration must match its identity and declare its risk".into());
    }
    Ok(Arc::new(ToolContribution {
        definition,
        handler: Arc::new(ForeignTool {
            service,
            generation,
            session_id: identity.session_id.clone(),
            config,
        }),
    }))
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
    scope: SessionScope,
    contributed: Arc<[String]>,
    sources: Arc<[String]>,
}
impl ToolRegistryPlugin {
    pub fn new(scope: &SessionScope, names: ToolNames<'_>) -> Self {
        Self {
            scope: *scope,
            contributed: names.contributed.into(),
            sources: names.sources.into(),
        }
    }
}
impl TypedPlugin for ToolRegistryPlugin {
    type Deps = (
        InSession<ToolHost>,
        InSession<SessionConfig>,
        Contributions<SessionToolSource>,
        InSession<Diagnostics>,
        InSession<SessionIdentity>,
        InSession<SessionLifecycle>,
    );
    fn name(&self) -> &str {
        "ToolRegistry"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            InSession(host),
            InSession(config),
            Contributions(sources),
            InSession(diagnostics),
            InSession(identity),
            InSession(lifecycle),
        ): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let host_tools: BTreeMap<_, _> = host
                .0
                .definitions()
                .into_iter()
                .map(|definition| {
                    (
                        definition.name.clone(),
                        Arc::new(ToolContribution {
                            definition,
                            handler: Arc::new(HostTool(host.clone())),
                        }),
                    )
                })
                .collect();
            let mut selected = HashSet::new();
            for name in &config.tools {
                if !selected.insert(name)
                    || (self.contributed.contains(name) && host_tools.contains_key(name))
                {
                    return Err(AgentError::InvalidInput(format!("duplicate tool {name}")).into());
                }
                if !host_tools.contains_key(name) && !self.contributed.contains(name) {
                    return Err(AgentError::InvalidInput(format!(
                        "tool {name} is not provided by the host"
                    ))
                    .into());
                }
            }
            let reserved = selected
                .into_iter()
                .cloned()
                .chain(self.contributed.iter().cloned())
                .collect();
            // Source keys are `<name>` or `<name>:<instance>`; names follow the
            // tool name grammar, which keeps `#` for the registry's own sets.
            if let Some(name) = sources
                .iter()
                .map(|(name, _)| name)
                .find(|name| !valid_tool_name(name))
            {
                return Err(AgentError::InvalidInput(format!(
                    "tool source name {name} must match [A-Za-z0-9_-]{{1,64}}"
                ))
                .into());
            }
            let providers: Vec<(String, Arc<dyn ToolProvider>)> = sources
                .into_iter()
                .map(|(name, provider)| (name, provider.0.clone()))
                .collect();
            let scope = self.scope;
            let fixed = config
                .tools
                .iter()
                .filter_map(|name| host_tools.get(name).cloned())
                .collect();
            let contributed = Arc::new(Contributed {
                ctx: ctx.clone(),
                scope,
                names: config
                    .tools
                    .iter()
                    .filter(|name| self.contributed.contains(*name))
                    .cloned()
                    .collect(),
                identity,
                config: config.clone(),
                diagnostics,
                attached: Mutex::default(),
                versions: AtomicU64::new(1),
                reading: AtomicBool::new(false),
            });
            let providers = std::iter::once((
                CONTRIBUTED.to_owned(),
                contributed.clone() as Arc<dyn ToolProvider>,
            ))
            .chain(providers)
            .collect();
            let registry = Arc::new(ToolRegistry {
                ctx: ctx.clone(),
                scope,
                fixed,
                configured: config.tools.clone(),
                reserved,
                providers,
                contributed,
                refreshing: Mutex::default(),
                sources: Mutex::default(),
                state: watch::Sender::new(ToolSourcesState::default()),
            });
            // Subscribe before the first read; notifications carry no state.
            ctx.events().on(
                ctx,
                &EventKey::<ToolProviderChanged>::of().instance(scope.instance),
                registry.clone(),
            )?;
            let changed = Arc::new(Notify::new());
            ctx.events().on(
                ctx,
                &EventKey::<FiberStatusChanged>::of(),
                ContributorsChanged(changed.clone()),
            )?;
            ctx.events().on(
                ctx,
                &EventKey::<ServiceChanged>::of(),
                ContributorsChanged(changed.clone()),
            )?;
            let refresher = registry.clone();
            // Background refreshes start once the Session is published; a
            // change before that keeps its permit, and ToolBinding's first
            // snapshot reads contributors itself.
            let task = tokio::spawn(async move {
                if !lifecycle.activated().await {
                    return;
                }
                loop {
                    changed.notified().await;
                    refresher.refresh();
                }
            });
            ctx.effect(move || {
                task.abort();
                Effect::Done
            })?;
            registry.refresh();
            ctx.provide_as(
                self.scope.key::<ToolSourcesView>(),
                Arc::new(ToolSourcesView(registry.sources_state())),
            )?;
            ctx.provide_as(self.scope.key::<ToolRegistry>(), registry)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ToolRegistryPlugin {
    fn keys(&self) -> KeysOf<Self> {
        let scope = self.scope;
        (
            scope,
            scope,
            (scope, self.sources.clone()),
            scope,
            scope,
            scope,
        )
    }
}
