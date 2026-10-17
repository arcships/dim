//! Structural Session rows; runtime and native bindings share each row's owner.
use crate::{
    Result, ServerError,
    agents::AgentIndex,
    settings::{ProjectAuthorized, SettingsChanged, SourceDeclaration, project_authorizations},
};
use dim_agent_capabilities::{
    methods::SessionMethods,
    session_broadcast::SessionBroadcastEvent,
    timeline_tree::{ForeignBranchService, branch_binding},
    tool_registry::valid_tool_name,
};
use dim_agent_runtime::{
    AgentBuilder, BRANCH_GROUP, BranchLifecycle, BranchScope, Dispatcher, SESSION_GROUP, Session,
    SessionConfig, SessionIdentity, SessionLifecycle, SessionScope, Timeline, TimelineIdentity,
};
use rutis::{BoxFuture, CordisError, Ctx, Effect, Event, Plugin, PluginFactory, TypeKey};
use rutis_bridge::{
    runtime::{LocalRuntime, Process, Stdio},
    session::{HostDispatch, Value as RpcValue, host_key_in, settle},
};
use rutis_loader::{LoaderError, Resolved, Resolver, RuntimeResolver, ServiceCatalog};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Local Node runtime arguments and ordinary loader rows, with a
/// `RowSchema.provides` snapshot for names needed before Node starts.
#[derive(Clone, Debug)]
pub struct SessionTypeScriptConfig {
    pub node: PathBuf,
    pub js_runtime: PathBuf,
    pub anchor: PathBuf,
    pub rows: Vec<Value>,
}

/// Settings events the Session runtime's parts receive with `ctx.on`
/// (settings-service §4.5), each with its JSON payload as the one argument.
const SETTINGS_EVENTS: [&str; 2] = [SettingsChanged::NAME, ProjectAuthorized::NAME];
const SESSION_EVENT: &str = "session.event";

enum SessionDelivery {
    Event(Session, Value),
    Closed(String),
}
struct SessionEvents {
    send: mpsc::UnboundedSender<SessionDelivery>,
}
impl rutis::PatternListener<SessionBroadcastEvent> for SessionEvents {
    fn call<'a>(
        &'a self,
        ctx: &'a Ctx,
        key: rutis::EventKey<SessionBroadcastEvent>,
        record: &'a SessionBroadcastEvent,
    ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
        let session_id = key.name().and_then(|name| name.strip_prefix("session/"));
        if let Some(session_id) = session_id {
            match record {
                SessionBroadcastEvent::Event { branch_id, event } => {
                    if let Some(index) = ctx.get::<AgentIndex>()
                        && let Some((_, session)) = index.session(session_id)
                        && !session.is_closing()
                        && !session.is_unloading()
                    {
                        let payload = serde_json::json!({
                            "sessionId":session_id, "branchId":branch_id, "event":event,
                        });
                        let _ = self.send.send(SessionDelivery::Event(session, payload));
                    }
                }
                SessionBroadcastEvent::Closed => {
                    let _ = self
                        .send
                        .send(SessionDelivery::Closed(session_id.to_owned()));
                }
                SessionBroadcastEvent::Fence { .. } => {}
            }
        }
        Box::pin(async { Ok(None) })
    }
}

/// The callback returns before Node I/O. This owner task holds each channel's
/// FIFO and workers, so concurrent Session channels need no shared mutable map.
pub(crate) fn forward_session_events(
    ctx: &Ctx,
    process: Arc<Process>,
) -> std::result::Result<(), CordisError> {
    let (send, mut incoming) = mpsc::unbounded_channel();
    ctx.events().on_pattern(
        ctx,
        rutis::EventPattern::prefix("session/"),
        SessionEvents { send },
    )?;
    let stop = ctx.cancellation_token();
    let task = tokio::spawn(async move {
        let mut channels: HashMap<
            String,
            (
                Session,
                mpsc::UnboundedSender<Value>,
                tokio::task::AbortHandle,
            ),
        > = HashMap::new();
        let mut workers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                completed = workers.join_next_with_id(), if !workers.is_empty() => {
                    // Idle restore reuses the channel; retire only this worker's resources.
                    if let Some(Ok((task, session_id))) = completed
                        && channels.get(&session_id).is_some_and(|(_, _, worker)| worker.id() == task)
                    {
                        channels.remove(&session_id);
                    }
                },
                record = incoming.recv() => match record {
                    Some(SessionDelivery::Event(session, payload)) => {
                        if channels.get(session.id()).is_some_and(|(owner, _, _)| owner.is_closing() || owner.is_unloading())
                            && let Some((_, _, worker)) = channels.remove(session.id())
                        {
                            worker.abort();
                        }
                        let (_, send, _) = channels.entry(session.id().to_owned()).or_insert_with(|| {
                            let (send, mut queue) = mpsc::unbounded_channel();
                            let process = process.clone();
                            let owner = session.clone();
                            let worker = workers.spawn(async move {
                                let closed = session.closed();
                                tokio::pin!(closed);
                                loop {
                                    let payload = tokio::select! {
                                        biased;
                                        _ = &mut closed => break,
                                        payload = queue.recv() => match payload {
                                            Some(payload) => payload,
                                            None => break,
                                        }
                                    };
                                    if session.is_closing() || session.is_unloading() {
                                        break;
                                    }
                                    if let Err(error) = process.emit(SESSION_EVENT, vec![RpcValue::from(payload)]).await {
                                        tracing::warn!(event = SESSION_EVENT, %error, "Session event delivery to TypeScript parts failed");
                                    }
                                }
                                session.id().to_owned()
                            });
                            (owner, send, worker)
                        });
                        let _ = send.send(payload);
                    }
                    Some(SessionDelivery::Closed(session_id)) => {
                        if let Some((_, _, worker)) = channels.remove(&session_id) {
                            worker.abort();
                        }
                    }
                    None => break,
                }
            }
        }
        channels.clear();
        workers.shutdown().await;
    });
    ctx.effect(move || {
        Effect::AsyncDisposer(Box::new(move || {
            Box::pin(async move {
                let _ = task.await;
                Ok(())
            })
        }))
    })?;
    Ok(())
}

/// Queues one Settings event for the runtime and returns: other listeners on
/// a Settings channel never wait for Node.
struct Enqueue(mpsc::UnboundedSender<(&'static str, Value)>);
impl Enqueue {
    fn send<E: rutis::Event<Value = ()> + serde::Serialize>(
        &self,
        event: &E,
    ) -> BoxFuture<'static, std::result::Result<Option<()>, CordisError>> {
        let payload = serde_json::to_value(event).expect("Settings event payloads serialize");
        let _ = self.0.send((E::NAME, payload));
        Box::pin(async { Ok(None) })
    }
}
impl rutis::PatternListener<SettingsChanged> for Enqueue {
    fn call<'a>(
        &'a self,
        _: &'a Ctx,
        _: rutis::EventKey<SettingsChanged>,
        event: &'a SettingsChanged,
    ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
        self.send(event)
    }
}
impl rutis::Listener<ProjectAuthorized> for Enqueue {
    fn call<'a>(
        &'a self,
        _: &'a Ctx,
        event: &'a ProjectAuthorized,
    ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
        self.send(event)
    }
}

/// Forwards Settings' committed changes of every source, and newly
/// authorized projects, to the Session runtime's parts (settings-service
/// §4.5). The listeners only queue; one task emits in commit order and ends
/// with the rows part. A failed delivery is that listener's diagnostic and
/// does not stop later events.
pub(crate) fn forward_settings_events(
    ctx: &Ctx,
    process: Arc<Process>,
) -> std::result::Result<(), CordisError> {
    let (send, mut queue) = mpsc::unbounded_channel();
    ctx.events().on_pattern(
        ctx,
        rutis::EventPattern::<SettingsChanged>::prefix("settings/"),
        Enqueue(send.clone()),
    )?;
    ctx.events()
        .on(ctx, &project_authorizations(), Enqueue(send))?;
    let task = tokio::spawn(async move {
        while let Some((name, payload)) = queue.recv().await {
            if let Err(error) = process.emit(name, vec![RpcValue::from(payload)]).await {
                tracing::warn!(event = name, %error, "Settings event delivery to TypeScript parts failed");
            }
        }
    });
    ctx.effect(move || {
        Effect::Disposer(Box::new(move || {
            task.abort();
            Ok(())
        }))
    })?;
    Ok(())
}

/// The rows' `settings` entries (settings-service §3.1, §5.4): a row's
/// package declares its sources next to `provides`, and the row id is their
/// owner. Settings checks and compiles each declaration when it applies.
pub(crate) fn settings_sources(config: &SessionTypeScriptConfig) -> Result<Vec<SourceDeclaration>> {
    let mut sources = Vec::new();
    for row in &config.rows {
        let Some(entries) = row.get("settings") else {
            continue;
        };
        let owner = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ServerError::Configuration("Session TypeScript row requires id".into())
            })?;
        let entries: Vec<SourceDeclaration> =
            serde_json::from_value(entries.clone()).map_err(|error| {
                ServerError::Configuration(format!(
                    "settings of Session TypeScript row {owner}: {error}"
                ))
            })?;
        sources.extend(entries.into_iter().map(|mut entry| {
            entry.owner = owner.to_owned();
            entry
        }));
    }
    Ok(sources)
}

pub(crate) struct Parts {
    pub runtime: Arc<LocalRuntime>,
    pub rows_runtime: Arc<RuntimeResolver>,
    pub resolver: Arc<SessionResolver>,
    pub rows: Vec<Value>,
    pub branch_rows: Vec<Value>,
    pub session_events: bool,
}

struct DeclaredService {
    name: String,
    methods: Vec<(String, bool)>,
}
struct BranchPart {
    bind: String,
    services: Vec<DeclaredService>,
}
struct Snapshot {
    provides: Map<String, Value>,
    branch: Option<Arc<BranchPart>>,
}

#[derive(Clone)]
enum PartScope {
    Session(SessionScope),
    Branch(BranchScope, Arc<BranchPart>),
}

impl Parts {
    pub fn new(
        config: &SessionTypeScriptConfig,
        builder: &mut AgentBuilder,
        catalog: &mut ServiceCatalog,
        management: &[(String, String)],
    ) -> Result<Self> {
        let invalid = |message: &str| ServerError::Configuration(message.into());
        if [&config.node, &config.js_runtime, &config.anchor]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(invalid("Session TypeScript runtime paths must be absolute"));
        }
        let mut ids = HashSet::new();
        let mut identities = HashSet::new();
        let mut snapshots: HashMap<String, Snapshot> = HashMap::new();
        let mut session_events = false;
        let mut rows = Vec::new();
        let mut branch_rows = Vec::new();
        for row in &config.rows {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| invalid("Session TypeScript row requires id"))?;
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| invalid("Session TypeScript row requires name"))?;
            if !ids.insert(id) {
                return Err(invalid("duplicate Session TypeScript row id"));
            }
            let disabled = match row.get("disabled") {
                None => false,
                Some(Value::Bool(disabled)) => *disabled,
                Some(_) => return Err(invalid("row disabled must be boolean")),
            };
            let provides = row
                .get("provides")
                .and_then(Value::as_object)
                .ok_or_else(|| invalid("Session TypeScript row requires RowSchema.provides"))?;
            if snapshots
                .get(name)
                .is_some_and(|snapshot| &snapshot.provides != provides)
            {
                return Err(invalid(
                    "rows of one module must share the same provides snapshot",
                ));
            }
            let mut bindings = provides
                .keys()
                .filter(|name| name.starts_with("branchPart:"));
            let binding = bindings.next().cloned();
            if bindings.next().is_some() {
                return Err(invalid(
                    "a branch row requires exactly one branchPart service",
                ));
            }
            let mut services = Vec::new();
            for (service, methods) in provides {
                if !identities.insert(service.clone()) {
                    return Err(invalid("duplicate Session TypeScript service identity"));
                }
                if let Some(binding) = &binding {
                    if service == binding {
                        if service == "branchPart:"
                            || methods != &serde_json::json!({"bind":"async"})
                        {
                            return Err(invalid("invalid branchPart identity or method mode"));
                        }
                    } else {
                        if service.is_empty()
                            || ["toolContribution:", "sessionMethods:", "sessionEvents:"]
                                .iter()
                                .any(|family| service.starts_with(family))
                            || catalog.key(service).is_some()
                            || catalog.is_shared(service)
                        {
                            return Err(invalid("invalid or colliding branch service identity"));
                        }
                        let methods = methods
                            .as_object()
                            .ok_or_else(|| invalid("branch service methods must be an object"))?;
                        if methods
                            .values()
                            .any(|mode| !matches!(mode.as_str(), Some("sync" | "async")))
                        {
                            return Err(invalid("invalid branch service method mode"));
                        }
                        services.push(DeclaredService {
                            name: service.clone(),
                            methods: methods
                                .iter()
                                .map(|(name, mode)| (name.clone(), mode == "async"))
                                .collect(),
                        });
                    }
                    catalog.register_shared_instance(service, BRANCH_GROUP);
                    continue;
                } else if let Some(tool) = service.strip_prefix("toolContribution:") {
                    if !valid_tool_name(tool)
                        || methods != &serde_json::json!({"declaration":"sync", "execute":"async"})
                    {
                        return Err(invalid("invalid toolContribution identity or method modes"));
                    }
                    if builder.provided_tools().any(|existing| existing == tool) {
                        return Err(invalid("TypeScript tool collides with an existing tool"));
                    }
                    // Keep disabled identities known when restoring saved tool selections.
                    builder.session_tool(tool);
                } else if let Some(namespace) = service.strip_prefix("sessionMethods:") {
                    if methods != &serde_json::json!({"describe":"async", "call":"async"})
                        || builder
                            .session_method_namespaces()
                            .iter()
                            .any(|existing| existing == namespace)
                    {
                        return Err(invalid("invalid or duplicate Session method namespace"));
                    }
                    crate::methods::SessionNamespaces::new(management, &[namespace.to_owned()])
                        .map_err(|error| ServerError::Configuration(error.to_string()))?;
                    if !disabled {
                        builder.session_methods(namespace);
                    }
                } else if let Some(part) = service.strip_prefix("sessionEvents:") {
                    if part.is_empty() || methods != &serde_json::json!({"bind":"sync"}) {
                        return Err(invalid("invalid sessionEvents identity or method mode"));
                    }
                    session_events |= !disabled;
                } else {
                    return Err(invalid("unsupported Session TypeScript service family"));
                }
                catalog.register_shared_instance(service, SESSION_GROUP);
            }
            let branch = binding.map(|bind| Arc::new(BranchPart { bind, services }));
            let mut structural = row.clone();
            structural
                .as_object_mut()
                .expect("validated row object")
                .remove("settings");
            if branch.is_some() {
                branch_rows.push(structural);
            } else {
                rows.push(structural);
            }
            snapshots.insert(
                name.to_owned(),
                Snapshot {
                    provides: provides.clone(),
                    branch,
                },
            );
        }
        let runtime = Arc::new(
            LocalRuntime::node(&config.js_runtime, &config.anchor)
                .interpreter(config.node.clone())
                .named("session-node")
                .emits(SETTINGS_EVENTS.into_iter().chain([SESSION_EVENT]))
                .stdin(Stdio::Null)
                .stdout(Stdio::Null)
                .stderr(Stdio::Null),
        );
        let rows_runtime = Arc::new(RuntimeResolver::node(runtime.handle()).with_catalog(catalog));
        let resolver = Arc::new(SessionResolver {
            runtime: rows_runtime.clone(),
            snapshots,
        });
        Ok(Self {
            runtime,
            rows_runtime,
            resolver,
            session_events,
            rows,
            branch_rows,
        })
    }
}

pub(crate) struct SessionResolver {
    runtime: Arc<RuntimeResolver>,
    snapshots: HashMap<String, Snapshot>,
}
impl Resolver for SessionResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, std::result::Result<Arc<Resolved>, LoaderError>> {
        Box::pin(async move {
            let snapshot = self
                .snapshots
                .get(name)
                .ok_or_else(|| LoaderError::NotFound { name: name.into() })?;
            let original = self.runtime.resolve(name).await?;
            match original.meta.get("provides") {
                Some(Value::Object(actual)) if actual == &snapshot.provides => {}
                // The pinned SDK resolves this placeholder again before releasing RuntimeRows.
                None if original.meta["source"] == "runtime"
                    && original.meta["schema"]
                        == "unavailable: the Cordis runtime is not running" => {}
                _ => {
                    return Err(LoaderError::Resolve {
                        name: name.into(),
                        message: "row provides differs from its structural declaration".into(),
                    });
                }
            }
            let namespaces: Arc<[String]> = snapshot
                .provides
                .keys()
                .filter_map(|name| name.strip_prefix("sessionMethods:").map(str::to_owned))
                .collect();
            let tools: Arc<[String]> = snapshot
                .provides
                .keys()
                .filter(|name| name.starts_with("toolContribution:"))
                .cloned()
                .collect();
            let events: Arc<[String]> = snapshot
                .provides
                .keys()
                .filter(|name| name.starts_with("sessionEvents:"))
                .cloned()
                .collect();
            let mut resolved = Resolved::new(Arc::new(BoundFactory {
                inner: original.factory.clone(),
                scope: None,
                namespaces: namespaces.clone(),
                tools: tools.clone(),
                events: events.clone(),
                injects: original.factory.injects().to_vec(),
            }))
            .with_schema(original.schema.clone())
            .with_meta(original.meta.clone());
            if original.foreign_scope {
                resolved = resolved.with_foreign_scope();
            }
            if let Some(declares) = &original.declares {
                resolved = resolved.with_declarations(declares.clone());
            }
            let scoped = original
                .scoped
                .clone()
                .ok_or_else(|| LoaderError::Resolve {
                    name: name.into(),
                    message: "runtime row has no scoped factory".into(),
                })?;
            let branch = snapshot.branch.clone();
            resolved = resolved.with_scoped(Arc::new(move |build| {
                let inner = scoped(build)?;
                let scope = SessionScope {
                    instance: build.instance(SESSION_GROUP)?,
                };
                let mut injects = inner.injects().to_vec();
                let scope = if let Some(part) = &branch {
                    let branch = BranchScope {
                        session: scope,
                        instance: build.instance(BRANCH_GROUP)?,
                    };
                    injects.extend([
                        branch.key::<Timeline>(),
                        branch.key::<Dispatcher>(),
                        branch.key::<TimelineIdentity>(),
                        branch.key::<BranchLifecycle>(),
                    ]);
                    PartScope::Branch(branch, part.clone())
                } else {
                    if !events.is_empty() {
                        injects.extend([
                            scope.key::<SessionIdentity>(),
                            scope.key::<SessionConfig>(),
                            scope.key::<SessionLifecycle>(),
                        ]);
                    }
                    PartScope::Session(scope)
                };
                Ok(Arc::new(BoundFactory {
                    inner,
                    scope: Some(scope),
                    namespaces: namespaces.clone(),
                    tools: tools.clone(),
                    events: events.clone(),
                    injects,
                }))
            }));
            Ok(Arc::new(resolved))
        })
    }
}

struct BoundFactory {
    inner: Arc<dyn PluginFactory<Value>>,
    scope: Option<PartScope>,
    namespaces: Arc<[String]>,
    tools: Arc<[String]>,
    events: Arc<[String]>,
    injects: Vec<TypeKey>,
}
impl PluginFactory<Value> for BoundFactory {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn validate_config(&self, config: &Value) -> std::result::Result<(), CordisError> {
        self.inner.validate_config(config)
    }
    fn build(&self, config: &Value) -> std::result::Result<Box<dyn Plugin>, CordisError> {
        Ok(Box::new(BoundRow {
            inner: self.inner.build(config)?,
            scope: self.scope.clone().ok_or_else(|| {
                CordisError::PluginFailed("TypeScript part outside a Session".into())
            })?,
            namespaces: self.namespaces.clone(),
            tools: self.tools.clone(),
            events: self.events.clone(),
            injects: self.injects.clone(),
        }))
    }
}

struct BoundRow {
    inner: Box<dyn Plugin>,
    scope: PartScope,
    namespaces: Arc<[String]>,
    tools: Arc<[String]>,
    events: Arc<[String]>,
    injects: Vec<TypeKey>,
}
struct ProjectedServices {
    keys: Arc<[TypeKey]>,
    changed: Arc<tokio::sync::Notify>,
}
impl rutis::Listener<rutis::ServiceChanged> for ProjectedServices {
    fn call<'a>(
        &'a self,
        _: &'a Ctx,
        event: &'a rutis::ServiceChanged,
    ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
        Box::pin(async move {
            if self.keys.contains(&event.key) {
                self.changed.notify_one();
            }
            Ok(None)
        })
    }
}
impl Plugin for BoundRow {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn injects(&self) -> &[TypeKey] {
        &self.injects
    }
    fn validate(&self) -> std::result::Result<(), CordisError> {
        self.inner.validate()
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            if let PartScope::Session(scope) = &self.scope {
                for name in self.tools.iter() {
                    ctx.provide_as(
                        TypeKey::keyed_dynamic::<CancellationToken>(name.as_str())
                            .with_instance(scope.instance),
                        Arc::new(ctx.cancellation_token()),
                    )?;
                }
            }
            let keys: Arc<[TypeKey]> = match &self.scope {
                PartScope::Branch(scope, part) => std::iter::once(part.bind.as_str())
                    .chain(part.services.iter().map(|service| service.name.as_str()))
                    .map(|name| host_key_in(name, scope.instance))
                    .collect(),
                PartScope::Session(scope) => self
                    .namespaces
                    .iter()
                    .map(|namespace| {
                        host_key_in(&format!("sessionMethods:{namespace}"), scope.instance)
                    })
                    .chain(
                        self.tools
                            .iter()
                            .map(|name| host_key_in(name, scope.instance)),
                    )
                    .chain(
                        self.events
                            .iter()
                            .map(|name| host_key_in(name, scope.instance)),
                    )
                    .collect(),
            };
            let changed = Arc::new(tokio::sync::Notify::new());
            // rows.load may reply before its service notification is projected.
            // Subscribe before loading, then read only this row's own services.
            let listener = if keys.is_empty() {
                None
            } else {
                Some(ctx.events().on(
                    ctx,
                    &rutis::EventKey::of(),
                    ProjectedServices {
                        keys: keys.clone(),
                        changed: changed.clone(),
                    },
                )?)
            };
            let cleanup = self.inner.apply(ctx).await?;
            ctx.effect(move || cleanup)?;
            let cancelled = ctx.cancellation_token();
            for (index, key) in keys.iter().enumerate() {
                let service = loop {
                    if let Some(service) = ctx.get_as::<dyn HostDispatch>(key.clone()) {
                        break service;
                    }
                    tokio::select! {
                        _ = changed.notified() => {},
                        _ = cancelled.cancelled() => return Err(CordisError::PluginFailed("TypeScript part binding cancelled".into())),
                    }
                };
                let scope = match &self.scope {
                    PartScope::Branch(scope, part) => {
                        if index == 0 {
                            let binding = branch_binding(
                                ctx,
                                self.name(),
                                scope.service::<TimelineIdentity>(ctx)?,
                                scope.service::<Timeline>(ctx)?,
                                scope.service::<Dispatcher>(ctx)?,
                                scope.service::<BranchLifecycle>(ctx)?,
                            );
                            let reply = service
                                .invoke("bind", RpcValue::List(vec![binding]))
                                .map_err(|error| CordisError::PluginFailed(error.into()))?;
                            let reply = tokio::select! {
                            biased;
                            _ = cancelled.cancelled() => return Err(CordisError::PluginFailed("branch part binding cancelled".into())),
                            reply = settle(reply) => reply,
                        }.map_err(|error| CordisError::PluginFailed(error.into()))?;
                            if !matches!(reply, RpcValue::Data(Value::Null)) {
                                return Err(CordisError::PluginFailed(
                                    "branchPart.bind must return null".into(),
                                ));
                            }
                        } else {
                            let declaration = &part.services[index - 1];
                            ctx.provide_as(
                                TypeKey::keyed_dynamic::<ForeignBranchService>(
                                    declaration.name.as_str(),
                                )
                                .with_instance(scope.instance),
                                Arc::new(ForeignBranchService::new(
                                    service,
                                    declaration.methods.iter().cloned(),
                                    cancelled.clone(),
                                )),
                            )?;
                        }
                        continue;
                    }
                    PartScope::Session(scope) => scope,
                };
                if let Some(namespace) = self.namespaces.get(index) {
                    SessionMethods::provide_foreign(ctx, scope, namespace, service).await?;
                } else if index >= self.namespaces.len() + self.tools.len() {
                    let identity = scope.service::<SessionIdentity>(ctx)?;
                    let config = scope.service::<SessionConfig>(ctx)?;
                    let lifecycle = scope.service::<SessionLifecycle>(ctx)?;
                    let cancelled = cancelled.clone();
                    let closing = RpcValue::future(async move {
                        tokio::select! {
                            _ = lifecycle.closing() => {},
                            _ = cancelled.cancelled() => {},
                        }
                        Ok(RpcValue::Undefined)
                    });
                    let args =
                        serde_json::json!({"sessionId":identity.session_id, "config":config});
                    let reply = service
                        .invoke("bind", RpcValue::List(vec![args.into(), closing]))
                        .map_err(|error| CordisError::PluginFailed(error.into()))?;
                    if !matches!(reply, RpcValue::Data(Value::Null)) {
                        return Err(CordisError::PluginFailed(
                            "sessionEvents.bind must return null synchronously".into(),
                        ));
                    }
                }
            }
            if let Some(listener) = listener {
                listener
                    .dispose()
                    .await
                    .map_err(|error| CordisError::PluginFailed(error.to_string().into()))?;
            }
            Ok(Effect::Done)
        })
    }
}
