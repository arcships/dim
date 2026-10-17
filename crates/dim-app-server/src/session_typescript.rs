//! Structural Session rows; runtime and native bindings share each row's owner.
use crate::{
    Result, ServerError,
    agents::AgentIndex,
    settings::{ProjectAuthorized, SettingsChanged, SourceDeclaration, project_authorizations},
};
use dim_agent_capabilities::{
    methods::SessionMethods,
    session_broadcast::SessionBroadcastEvent,
    tool_registry::valid_tool_name,
    tree_binding::{self, ForeignBranchService},
};
use dim_agent_runtime::{
    AgentBuilder, AgentError, BRANCH_GROUP, BranchLifecycle, BranchScope, Dispatcher,
    SESSION_GROUP, Session, SessionConfig, SessionIdentity, SessionLifecycle, SessionScope,
    Timeline, TimelineIdentity,
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
    /// Rows of the Session group and of its nested branch group.
    pub rows: Vec<Value>,
    pub branch_rows: Vec<Value>,
    pub session_events: bool,
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
        let mut snapshots = HashMap::new();
        let mut session_events = false;
        let mut branch_ids = HashSet::new();
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
                .is_some_and(|snapshot| snapshot != provides)
            {
                return Err(invalid(
                    "rows of one module must share the same provides snapshot",
                ));
            }
            // A branchPart declaration places the row in each branch; its
            // other names are the part's public business services.
            if provides.keys().any(|name| name.starts_with("branchPart:")) {
                let mut parts = 0;
                for (service, methods) in provides {
                    if !identities.insert(service.clone()) {
                        return Err(invalid("duplicate Session TypeScript service identity"));
                    }
                    if let Some(part) = service.strip_prefix("branchPart:") {
                        if part.is_empty() || methods != &serde_json::json!({"bind":"async"}) {
                            return Err(invalid("invalid branchPart identity or method mode"));
                        }
                        parts += 1;
                    } else if service.is_empty()
                        || service.contains(':')
                        || catalog.key(service).is_some()
                        || methods.as_object().is_none_or(|methods| {
                            methods.is_empty()
                                || methods
                                    .values()
                                    .any(|mode| mode != "sync" && mode != "async")
                        })
                    {
                        return Err(invalid(
                            "invalid, mixed or colliding branch part service declaration",
                        ));
                    }
                }
                if parts != 1 {
                    return Err(invalid("a branch row declares exactly one branchPart"));
                }
                for service in provides.keys() {
                    catalog.register_shared_instance(service, BRANCH_GROUP);
                }
                branch_ids.insert(id.to_owned());
                snapshots.insert(name.to_owned(), provides.clone());
                continue;
            }
            for (service, methods) in provides {
                if !identities.insert(service.clone()) {
                    return Err(invalid("duplicate Session TypeScript service identity"));
                }
                if let Some(tool) = service.strip_prefix("toolContribution:") {
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
            snapshots.insert(name.to_owned(), provides.clone());
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
        // `settings` is the host's input to Settings, not a loader field.
        let (branch_rows, rows) = config
            .rows
            .iter()
            .cloned()
            .map(|mut row| {
                if let Value::Object(fields) = &mut row {
                    fields.remove("settings");
                }
                row
            })
            .partition(|row| row["id"].as_str().is_some_and(|id| branch_ids.contains(id)));
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
    snapshots: HashMap<String, Map<String, Value>>,
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
                Some(Value::Object(actual)) if actual == snapshot => {}
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
                .keys()
                .filter_map(|name| name.strip_prefix("sessionMethods:").map(str::to_owned))
                .collect();
            let tools: Arc<[String]> = snapshot
                .keys()
                .filter(|name| name.starts_with("toolContribution:"))
                .cloned()
                .collect();
            let events: Arc<[String]> = snapshot
                .keys()
                .filter(|name| name.starts_with("sessionEvents:"))
                .cloned()
                .collect();
            let part = snapshot
                .keys()
                .find(|name| name.starts_with("branchPart:"))
                .map(|part| {
                    let public = snapshot
                        .iter()
                        .filter(|(name, _)| *name != part)
                        .map(|(name, methods)| {
                            (
                                name.clone(),
                                methods.as_object().cloned().unwrap_or_default(),
                            )
                        })
                        .collect();
                    Arc::new(BranchPart {
                        setup: part.clone(),
                        public,
                    })
                });
            let mut resolved = Resolved::new(Arc::new(BoundFactory {
                inner: original.factory.clone(),
                scope: None,
                branch: None,
                part: part.clone(),
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
            resolved = resolved.with_scoped(Arc::new(move |build| {
                let inner = scoped(build)?;
                let scope = SessionScope {
                    instance: build.instance(SESSION_GROUP)?,
                };
                let mut injects = inner.injects().to_vec();
                if !events.is_empty() {
                    injects.extend([
                        scope.key::<SessionIdentity>(),
                        scope.key::<SessionConfig>(),
                        scope.key::<SessionLifecycle>(),
                    ]);
                }
                let branch = match &part {
                    Some(_) => {
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
                        Some(branch)
                    }
                    None => None,
                };
                Ok(Arc::new(BoundFactory {
                    inner,
                    scope: Some(scope),
                    branch,
                    part: part.clone(),
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

/// A branch row's `branchPart:<part>` and its public business services with
/// their declared method modes.
struct BranchPart {
    setup: String,
    public: Vec<(String, Map<String, Value>)>,
}

struct BoundFactory {
    inner: Arc<dyn PluginFactory<Value>>,
    scope: Option<SessionScope>,
    branch: Option<BranchScope>,
    part: Option<Arc<BranchPart>>,
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
            scope: self.scope.ok_or_else(|| {
                CordisError::PluginFailed("TypeScript part outside a Session".into())
            })?,
            branch: match (&self.part, self.branch) {
                (Some(part), Some(branch)) => Some((branch, part.clone())),
                (None, _) => None,
                (Some(_), None) => {
                    return Err(CordisError::PluginFailed(
                        "TypeScript branch part outside a branch".into(),
                    ));
                }
            },
            namespaces: self.namespaces.clone(),
            tools: self.tools.clone(),
            events: self.events.clone(),
            injects: self.injects.clone(),
        }))
    }
}

struct BoundRow {
    inner: Box<dyn Plugin>,
    scope: SessionScope,
    branch: Option<(BranchScope, Arc<BranchPart>)>,
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
impl BoundRow {
    /// Applies the original row, registers its cleanup and waits for its own
    /// projected services: rows.load may reply before its service
    /// notification is projected, so the listener exists before loading.
    async fn apply_projected(
        &self,
        ctx: &Ctx,
        keys: Vec<TypeKey>,
    ) -> std::result::Result<Vec<Arc<dyn HostDispatch>>, CordisError> {
        let keys: Arc<[TypeKey]> = keys.into();
        let changed = Arc::new(tokio::sync::Notify::new());
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
        let mut services = Vec::with_capacity(keys.len());
        for key in keys.iter() {
            services.push(loop {
                if let Some(service) = ctx.get_as::<dyn HostDispatch>(key.clone()) {
                    break service;
                }
                tokio::select! {
                    _ = changed.notified() => {},
                    _ = cancelled.cancelled() => return Err(CordisError::PluginFailed("TypeScript part binding cancelled".into())),
                }
            });
        }
        if let Some(listener) = listener {
            listener
                .dispose()
                .await
                .map_err(|error| CordisError::PluginFailed(error.to_string().into()))?;
        }
        Ok(services)
    }

    /// C5: bind the branch part once with its own branch's references, then
    /// publish its public services; none is visible before bind completed.
    async fn apply_branch(
        &self,
        ctx: &Ctx,
        branch: &BranchScope,
        part: &BranchPart,
    ) -> std::result::Result<(), CordisError> {
        let keys = std::iter::once(&part.setup)
            .chain(part.public.iter().map(|(name, _)| name))
            .map(|name| host_key_in(name, branch.instance))
            .collect();
        let mut services = self.apply_projected(ctx, keys).await?.into_iter();
        let setup = services.next().expect("setup service");
        let failed = |error: AgentError| CordisError::PluginFailed(error.to_string().into());
        let identity = branch.service::<TimelineIdentity>(ctx).map_err(failed)?;
        let lifecycle = branch.service::<BranchLifecycle>(ctx).map_err(failed)?;
        let cancelled = ctx.cancellation_token();
        let activated = {
            let (lifecycle, cancelled) = (lifecycle.clone(), cancelled.clone());
            RpcValue::future(async move {
                let activated = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => false,
                    activated = lifecycle.activated() => activated,
                };
                Ok(serde_json::json!(activated).into())
            })
        };
        let closing = {
            let cancelled = cancelled.clone();
            RpcValue::future(async move {
                tokio::select! {
                    _ = lifecycle.closing() => {},
                    _ = cancelled.cancelled() => {},
                }
                Ok(RpcValue::Undefined)
            })
        };
        let binding = RpcValue::Record(
            [
                (
                    "identity".into(),
                    serde_json::json!({"sessionId": identity.session_id, "branchId": identity.branch_id})
                        .into(),
                ),
                (
                    "dispatcher".into(),
                    dispatcher_binding(
                        ctx,
                        self.inner.name(),
                        branch.service::<Dispatcher>(ctx).map_err(failed)?,
                    ),
                ),
                (
                    "timeline".into(),
                    timeline_binding(ctx, branch.service::<Timeline>(ctx).map_err(failed)?),
                ),
                ("activated".into(), activated),
                ("closing".into(), closing),
            ]
            .into(),
        );
        let reply = setup
            .invoke("bind", RpcValue::List(vec![binding]))
            .map_err(|error| CordisError::PluginFailed(error.into()))?;
        let reply = tokio::select! {
            biased;
            _ = cancelled.cancelled() => return Err(CordisError::PluginFailed("branch part binding cancelled".into())),
            reply = settle(reply) => reply,
        }
        .map_err(|error| CordisError::PluginFailed(error.into()))?;
        if !matches!(reply, RpcValue::Data(Value::Null)) {
            return Err(CordisError::PluginFailed(
                "branchPart.bind must resolve to null".into(),
            ));
        }
        for ((name, methods), service) in part.public.iter().zip(services) {
            ctx.provide_as(
                branch.named_key::<ForeignBranchService>(name),
                Arc::new(ForeignBranchService {
                    service,
                    methods: methods.clone(),
                }),
            )?;
        }
        Ok(())
    }
}

/// A row-bound reference ends with its original row.
fn ended(row: &Ctx) -> Option<RpcValue> {
    row.cancellation_token().is_cancelled().then(|| {
        serde_json::json!({"ok": false, "error": {"code": "closed", "message": "branch part row has ended"}})
            .into()
    })
}

/// `dispatcher.subscribe(keys, handler)` on the original row's context: the
/// subscription is removed with that row. The handler's result settles before
/// native handling completes; a failure fails the Dispatcher.
fn dispatcher_binding(row: &Ctx, part: &str, dispatcher: Arc<Dispatcher>) -> RpcValue {
    let (row, part) = (row.clone(), part.to_owned());
    let subscribe = RpcValue::callback(move |args| {
        if let Some(ended) = ended(&row) {
            return Ok(ended);
        }
        let Ok([keys, handler]) = <[RpcValue; 2]>::try_from(args.list()?) else {
            return Ok(tree_binding::invalid_params("expected keys and a handler"));
        };
        let handler = match handler.reference() {
            Ok(handler) if handler.is_function() => handler,
            _ => return Ok(tree_binding::invalid_params("handler must be a function")),
        };
        let keys: Vec<String> = match keys.json().map(serde_json::from_value) {
            Ok(Ok(keys)) => keys,
            _ => return Ok(tree_binding::invalid_params("keys must be strings")),
        };
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let name = part.clone();
        Ok(
            match dispatcher.subscribe(&row, part.clone(), &keys, move |event| {
                let (handler, name) = (handler.clone(), name.clone());
                Box::pin(async move {
                    let reply = match handler
                        .call_async(RpcValue::List(vec![tree_binding::data(&*event)]))
                        .await
                    {
                        Ok(value) => settle(value).await,
                        Err(error) => Err(error),
                    };
                    match reply {
                        Ok(RpcValue::Undefined | RpcValue::Data(Value::Null)) => Ok(()),
                        Ok(_) => Err(AgentError::fault(name, "handler returned a value")),
                        Err(error) => Err(AgentError::fault(name, error.to_string())),
                    }
                })
            }) {
                Ok(()) => tree_binding::ok(RpcValue::Data(Value::Null)),
                Err(error) => tree_binding::failure(&error),
            },
        )
    });
    RpcValue::Record([("subscribe".into(), subscribe)].into())
}

/// Read-only reconstruction reads of this branch's Timeline.
fn timeline_binding(row: &Ctx, timeline: Arc<Timeline>) -> RpcValue {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Through {
        through: u64,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Local {
        start: u64,
        through: u64,
    }
    let committed = {
        let (row, timeline) = (row.clone(), timeline.clone());
        RpcValue::callback(move |args| {
            if let Some(ended) = ended(&row) {
                return Ok(ended);
            }
            if !args.list()?.is_empty() {
                return Ok(tree_binding::invalid_params("committed takes no arguments"));
            }
            Ok(tree_binding::ok(tree_binding::data(&timeline.committed())))
        })
    };
    let history = {
        let (row, timeline) = (row.clone(), timeline.clone());
        RpcValue::callback(move |args| {
            if let Some(ended) = ended(&row) {
                return Ok(ended);
            }
            let through = tree_binding::request::<Through>(args).map(|t| t.through);
            let timeline = timeline.clone();
            Ok(RpcValue::future(async move {
                let through = match through {
                    Ok(through) => through,
                    Err(invalid) => return Ok(invalid),
                };
                Ok(match timeline.history(Some(through)).await {
                    Ok(events) => tree_binding::ok(tree_binding::data(
                        &events.iter().map(|event| &**event).collect::<Vec<_>>(),
                    )),
                    Err(error) => tree_binding::failure(&error),
                })
            }))
        })
    };
    let local = {
        let row = row.clone();
        RpcValue::callback(move |args| {
            if let Some(ended) = ended(&row) {
                return Ok(ended);
            }
            let range = tree_binding::request::<Local>(args);
            let timeline = timeline.clone();
            Ok(RpcValue::future(async move {
                let Local { start, through } = match range {
                    Ok(range) => range,
                    Err(invalid) => return Ok(invalid),
                };
                Ok(match timeline.local(start..=through).await {
                    Ok(events) => tree_binding::ok(tree_binding::data(
                        &events.iter().map(|event| &**event).collect::<Vec<_>>(),
                    )),
                    Err(error) => tree_binding::failure(&error),
                })
            }))
        })
    };
    RpcValue::Record(
        [
            ("committed".into(), committed),
            ("history".into(), history),
            ("local".into(), local),
        ]
        .into(),
    )
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
            if let Some((branch, part)) = &self.branch {
                self.apply_branch(ctx, branch, part).await?;
                return Ok(Effect::Done);
            }
            for name in self.tools.iter() {
                ctx.provide_as(
                    TypeKey::keyed_dynamic::<CancellationToken>(name.as_str())
                        .with_instance(self.scope.instance),
                    Arc::new(ctx.cancellation_token()),
                )?;
            }
            let keys = self
                .namespaces
                .iter()
                .map(|namespace| {
                    host_key_in(&format!("sessionMethods:{namespace}"), self.scope.instance)
                })
                .chain(
                    self.tools
                        .iter()
                        .map(|name| host_key_in(name, self.scope.instance)),
                )
                .chain(
                    self.events
                        .iter()
                        .map(|name| host_key_in(name, self.scope.instance)),
                )
                .collect();
            let services = self.apply_projected(ctx, keys).await?;
            let cancelled = ctx.cancellation_token();
            for (index, service) in services.into_iter().enumerate() {
                if let Some(namespace) = self.namespaces.get(index) {
                    SessionMethods::provide_foreign(ctx, &self.scope, namespace, service).await?;
                } else if index >= self.namespaces.len() + self.tools.len() {
                    let identity = self.scope.service::<SessionIdentity>(ctx)?;
                    let config = self.scope.service::<SessionConfig>(ctx)?;
                    let lifecycle = self.scope.service::<SessionLifecycle>(ctx)?;
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
            Ok(Effect::Done)
        })
    }
}
