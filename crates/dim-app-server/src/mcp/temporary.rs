//! Disposable Session bindings; configured MCP sources and their publisher are unchanged.
use super::{
    McpConnection,
    config::{self, McpServerSettings},
    daemon_environment,
    server::{McpServer, McpServerError, McpServerPlugin},
    session::contribution,
};
use dim_agent_capabilities::{
    methods::{self, Interruption, MethodCall, MethodDecl, RpcError, SessionMethods},
    tool_registry::{
        ToolProvider, ToolProviderChanged, ToolRegistry, ToolSourceSet, tool_provider_changes,
    },
};
use dim_agent_runtime::{Names, SESSION_GROUP, SessionConfig, SessionLifecycle};
use futures_util::future::{select_all, try_join_all};
use rutis::Ctx;
use rutis::host::{
    BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberView, InstanceId, Key, TypeKey,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, watch},
    time::Instant,
};
use tokio_util::task::TaskTracker;

pub(super) const LEASE: Duration = Duration::from_secs(30);
const NAMESPACE: &str = "session.mcp_temporary";

/// A temporary binding is the client's own document, so every reference
/// expands with `Origin::User`: no field is guarded and no credential names
/// are needed.
fn temporary_expansion(configuration: &McpServerSettings) -> McpServerSettings {
    config::expand(
        configuration,
        config::Origin::User,
        &daemon_environment,
        &BTreeSet::new(),
    )
    .configuration
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Attach {
    #[serde(rename = "sessionId")]
    _session_id: String,
    binding_id: uuid::Uuid,
    servers: BTreeMap<String, McpServerSettings>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Address {
    #[serde(rename = "sessionId")]
    _session_id: String,
    binding_id: uuid::Uuid,
}
struct Binding {
    servers: BTreeMap<String, McpServerSettings>,
    view: FiberView,
    deadline: Mutex<Instant>,
    ready: watch::Receiver<Option<Result<(), RpcError>>>,
}
type Bindings = Arc<Mutex<BTreeMap<uuid::Uuid, Arc<Binding>>>>;
fn failed() -> RpcError {
    RpcError::service(
        "mcp_binding_failed",
        "Temporary MCP server initialization or connection failed",
    )
}
/// The same refusal with the server's cause appended, so a refused tool list
/// names the tool and the schema path.
fn failed_because(error: &McpServerError) -> RpcError {
    RpcError::service(
        "mcp_binding_failed",
        format!("Temporary MCP server initialization or connection failed: {error}"),
    )
}
fn missing() -> RpcError {
    RpcError::service(
        "mcp_binding_missing",
        "Temporary MCP binding is absent; attach it again",
    )
}

pub(super) struct TemporaryMcp {
    identity: Key<dim_agent_runtime::SessionIdentity>,
    files: Key<dim_agent_capabilities::host::SessionFilesHost>,
    session: InstanceId,
    methods: Key<SessionMethods>,
    config: Key<SessionConfig>,
    tools: Key<ToolRegistry>,
    lifecycle: Key<SessionLifecycle>,
}
impl TemporaryMcp {
    pub(super) fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
            files: names.key()?,
            session: names.instance(SESSION_GROUP)?,
            methods: names.key()?,
            config: names.key()?,
            tools: names.key()?,
            lifecycle: names.key()?,
        })
    }
}
impl DynPlugin for TemporaryMcp {
    fn name(&self) -> &str {
        "TemporaryMcp"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.identity.erase(),
            self.methods.erase(),
            self.lifecycle.erase(),
            self.config.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let config = ctx.require_at(&self.config)?;
            let bindings: Bindings = Arc::default();
            let changed = Arc::new(Notify::new());
            let mutation = Arc::new(tokio::sync::Mutex::new(()));
            let tasks = TaskTracker::new();
            let stop = ctx.cancellation_token();
            let (cleanup, draining) = (bindings.clone(), tasks.clone());
            ctx.effect(move || async move {
                draining.close();
                draining.wait().await;
                let pending = std::mem::take(&mut *cleanup.lock().unwrap());
                let mut failed = false;
                for binding in pending.into_values() {
                    failed |= binding.view.shutdown().await.is_err();
                }
                if failed {
                    Err(ErrorKind::PluginFailed("Temporary MCP cleanup failed".into()).into())
                } else {
                    Ok(())
                }
            })?;
            let (expires, wake, expiry_mutation) =
                (bindings.clone(), changed.clone(), mutation.clone());
            tasks.spawn(async move {
                if !lifecycle.activated().await { return; }
                loop {
                    let notified = wake.notified();
                    let deadline = expires.lock().unwrap().values().map(|b| *b.deadline.lock().unwrap()).min();
                    tokio::select! {
                        () = stop.cancelled()=>break,
                        () = lifecycle.closing()=>break,
                        () = notified=>{},
                        () = async { match deadline { Some(at)=>tokio::time::sleep_until(at).await, None=>std::future::pending().await } }=>{},
                    }
                    let _mutation = expiry_mutation.lock().await;
                    let retired = {
                        let mut bindings = expires.lock().unwrap();
                        let ids: Vec<_> = bindings.iter().filter(|(_,b)| *b.deadline.lock().unwrap() <= Instant::now() || b.ready.borrow().as_ref().is_some_and(Result::is_err)).map(|(id,_)| *id).collect();
                        ids.into_iter().filter_map(|id| bindings.remove(&id)).collect::<Vec<_>>()
                    };
                    for binding in retired { if let Err(error) = binding.view.shutdown().await { tracing::warn!(%error, "temporary MCP cleanup failed"); } }
                }
            });
            let identity = ctx.require_at(&self.identity)?;
            let files = ctx.get_at(&self.files);
            let context = ctx.clone();
            let (session, tools, lifecycle) =
                (self.session, self.tools.clone(), self.lifecycle.clone());
            methods.register(ctx, NAMESPACE, ["attach","renew","detach"].into_iter().map(|name| MethodDecl::new(&format!("{NAMESPACE}.{name}"), true, Interruption::Drain)).collect(), Arc::new(move |call, cancelled, _| {
                let session_id = identity.session_id.clone();
                let files = files.clone();
                let MethodCall { method, params, .. } = call;
                let (bindings, changed, mutation, ctx, cwd, tools, lifecycle) = (bindings.clone(), changed.clone(), mutation.clone(), context.clone(), config.cwd.clone(), tools.clone(), lifecycle.clone());
                Box::pin(async move {
                    if method.ends_with(".attach") {
                        let mut input: Attach = methods::params(params)?;
                        if input.servers.is_empty() { return Ok(json!({})); }
                        for (name, configuration) in &mut input.servers {
                            if name.trim().is_empty() { return Err(RpcError::invalid("Empty MCP server name")); }
                            if let McpServerSettings::Stdio { cwd: directory, .. } = configuration { *directory = Some(cwd.clone()); }
                            let expanded = temporary_expansion(configuration);
                            McpServerPlugin::new(name, expanded, Key::of()).map_err(|_| RpcError::invalid("Invalid temporary MCP configuration"))?;
                        }
                        let guard = mutation.lock().await;
                        let stale = {
                            let mut entries = bindings.lock().unwrap();
                            let expired = entries.get(&input.binding_id).is_some_and(|b| *b.deadline.lock().unwrap() <= Instant::now() || b.ready.borrow().as_ref().is_some_and(Result::is_err) || b.view.state().error.is_some());
                            if expired { entries.remove(&input.binding_id) } else { None }
                        };
                        if let Some(stale) = stale { stale.view.shutdown().await.map_err(|_| failed())?; }
                        let binding = {
                            let mut bindings = bindings.lock().unwrap();
                            if let Some(binding) = bindings.get(&input.binding_id) {
                                if binding.servers != input.servers { return Err(RpcError::service("mcp_binding_conflict", "Binding id already has different MCP configuration")); }
                                *binding.deadline.lock().unwrap() = Instant::now() + LEASE;
                                binding.clone()
                            } else {
                                let (ready, received) = watch::channel(None);
                                let plugin = BindingPlugin { files, session_id, id: input.binding_id, session, servers: input.servers.clone(), tools, lifecycle, ready, changed: changed.clone() };
                                let view = ctx.plugin(Box::new(plugin) as Box<dyn DynPlugin>);
                                let binding = Arc::new(Binding { servers: input.servers, view, ready: received, deadline: Mutex::new(Instant::now()+LEASE) });
                                bindings.insert(input.binding_id, binding.clone());
                                binding
                            }
                        };
                        changed.notify_one();
                        let loaded = (&binding.view).await.map_err(|_| failed());
                        drop(guard);
                        let mut ready = binding.ready.clone();
                        let result = async {
                          loaded?;
                          loop {
                            if let Some(result) = ready.borrow().clone() { return result; }
                            tokio::select! {
                                () = cancelled.cancelled()=>return Err(RpcError::interrupted()),
                                result = ready.changed()=>if result.is_err() { return Err(failed()); },
                            }
                          }
                        }.await;
                        if result.is_err() {
                            let _guard = mutation.lock().await;
                            { let mut entries = bindings.lock().unwrap();
                            if entries.get(&input.binding_id).is_some_and(|current| Arc::ptr_eq(current,&binding)) { entries.remove(&input.binding_id); } }
                            let _ = binding.view.shutdown().await;
                        }
                        return result.map(|()| json!({}));
                    }
                    let address: Address = methods::params(params)?;
                    if method.ends_with(".renew") {
                        let bindings = bindings.lock().unwrap();
                        let binding = bindings.get(&address.binding_id).ok_or_else(missing)?;
                        if *binding.deadline.lock().unwrap() <= Instant::now() { return Err(missing()); }
                        *binding.deadline.lock().unwrap() = Instant::now()+LEASE;
                        changed.notify_one();
                        return Ok(json!({}));
                    }
                    let _guard = mutation.lock().await;
                    let binding = bindings.lock().unwrap().remove(&address.binding_id);
                    changed.notify_one();
                    if let Some(binding) = binding { binding.view.shutdown().await.map_err(|_| failed())?; }
                    Ok(json!({}))
                })
            }))?;
            Ok(Effect::Done)
        })
    }
}

struct Tools(Arc<Mutex<Vec<Arc<ToolSourceSet>>>>);
impl ToolProvider for Tools {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        self.0.lock().unwrap().clone()
    }
}
struct BindingPlugin {
    files: Option<Arc<dim_agent_capabilities::host::SessionFilesHost>>,
    session_id: String,
    id: uuid::Uuid,
    session: InstanceId,
    servers: BTreeMap<String, McpServerSettings>,
    tools: Key<ToolRegistry>,
    lifecycle: Key<SessionLifecycle>,
    ready: watch::Sender<Option<Result<(), RpcError>>>,
    changed: Arc<Notify>,
}
impl DynPlugin for BindingPlugin {
    fn name(&self) -> &str {
        "TemporaryMcpBinding"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase(), self.lifecycle.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.tools)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let stop = ctx.cancellation_token();
            let tasks = TaskTracker::new();
            let (closing, draining) = (stop.clone(), tasks.clone());
            ctx.effect(move || async move {
                closing.cancel();
                draining.close();
                draining.wait().await;
                Ok(())
            })?;
            let prefix = format!("temporary_mcp_{}", self.id.simple());
            let sets = Arc::new(Mutex::new(Vec::new()));
            registry.register_source(ctx, &prefix, Arc::new(Tools(sets.clone())))?;
            let mut connections = Vec::new();
            for (name, configuration) in &self.servers {
                let id = format!("temporary:{}:{name}", self.id);
                let key = Key::<McpServer>::dynamic(id.clone()).instance(self.session);
                let expanded = temporary_expansion(configuration);
                let plugin = McpServerPlugin::new(&id, expanded, key.clone())
                    .map_err(|_| ErrorKind::PluginFailed("Invalid temporary MCP server".into()))?;
                let view = ctx.plugin(plugin);
                (&view).await.map_err(|_| {
                    ErrorKind::PluginFailed("Temporary MCP server failed to load".into())
                })?;
                let service = ctx.get_at(&key).ok_or_else(|| {
                    ErrorKind::PluginFailed("Temporary MCP server did not publish".into())
                })?;
                connections.push((id, Arc::new(McpConnection::temporary(service))));
            }
            let (ready, changed, ctx, session) = (
                self.ready.clone(),
                self.changed.clone(),
                ctx.clone(),
                self.session,
            );
            let files = self.files.clone();
            let session_id = self.session_id.clone();
            tasks.spawn(async move {
                let operation = async {
                    if !lifecycle.activated().await { return Err(failed()); }
                    let mut watches: Vec<_> = connections.iter().map(|(_,connection)| (connection.subscribe(), connection.tool_changes())).collect();
                    let mut previous = None;
                    loop {
                        for (state, tools) in &mut watches { state.borrow_and_update(); tools.borrow_and_update(); }
                        let lists = try_join_all(connections.iter().map(|(_,connection)| connection.tools(&stop))).await.map_err(|error| failed_because(&error))?;
                        if previous.as_ref() != Some(&lists) {
                        let next = connections.iter().zip(&lists).map(|((id,connection),tools)| Arc::new(ToolSourceSet {
                            key: format!("{prefix}:{id}"), version: uuid::Uuid::new_v4().to_string(),
                            tools: tools.iter().map(|tool| contribution(id,tool,connection.clone(),stop.clone(),files.clone(),session_id.clone())).collect(),
                        })).collect();
                        *sets.lock().unwrap() = next;
                        ctx.emit(ToolProviderChanged { name: tool_provider_changes(session) }).map_err(|_| failed())?;
                        previous = Some(lists);
                        }
                        ready.send_replace(Some(Ok(())));
                        tokio::select! {
                            () = stop.cancelled()=>return Ok(()),
                            () = lifecycle.closing()=>return Ok(()),
                            _ = select_all(watches.iter_mut().map(|(state,tools)| Box::pin(async move { tokio::select! { r=state.changed()=>r,r=tools.changed()=>r } })))=>{},
                        }
                    }
                };
                tokio::pin!(operation);
                let result = tokio::select! { ()=stop.cancelled()=>Ok(()), ()=lifecycle.closing()=>Ok(()), result=&mut operation=>result };
                stop.cancel();
                if let Err(error) = result { ready.send_replace(Some(Err(error))); changed.notify_one(); }
            });
            Ok(Effect::Done)
        })
    }
}
