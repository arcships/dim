//! Serialized host authority for source selection and cross-Session retirement.
//! Connection resources and disposal belong exclusively to loader instances.
use super::config::{self, McpPluginServers, McpProjectEnablement, McpServerSettings, McpSettings};
use super::{
    McpAuth, McpConnection, McpError, McpSource, Result,
    server::{McpServer, McpServerError, McpServerPlugin, McpServerState},
};
use crate::settings::{SettingsScope, SettingsService};
use dim_agent_runtime::{
    Modules, Names, SessionLifecycle, SessionLoader, SessionPhase, StateCell,
    names::ServiceCatalog, register_part,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis::{Ctx, Plugin};
use rutis_loader::Loader;
use rutis_loader::edit::InstanceResult;
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

pub(super) const GROUP: &str = "McpConnections";

/// Explicit connection owner. Session plugins construct this from their declared
/// Session loader; standalone hosts must supply their own loader and context.
#[derive(Clone)]
pub struct McpOwner {
    id: String,
    ctx: Ctx,
    loader: Arc<SessionLoader>,
    scope: rutis::host::InstanceId,
    closed: CancellationToken,
    session: Option<SessionLifecycle>,
}
impl McpOwner {
    pub fn new(ctx: Ctx, loader: Loader) -> Self {
        let loader = Arc::new(SessionLoader::new(loader, ctx.clone()));
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            closed: ctx.cancellation_token(),
            scope: ctx.instance(),
            ctx,
            loader,
            session: None,
        }
    }
    /// The owner of the Session `instance`, whose loader creates the
    /// connection instances inside it.
    pub(super) fn in_session(
        ctx: Ctx,
        loader: Arc<SessionLoader>,
        instance: rutis::host::InstanceId,
        session: SessionLifecycle,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            closed: ctx.cancellation_token(),
            scope: instance,
            ctx,
            loader,
            session: Some(session),
        }
    }
    /// A closing Session keeps its generations until it unloads, but no
    /// new instance is created in it.
    fn admits(&self) -> bool {
        !self.closed.is_cancelled()
            && self
                .session
                .as_ref()
                .is_none_or(|session| session.phase() < SessionPhase::Closing)
    }
}

/// Private immutable instance value. Never encoded in loader options.
#[derive(Clone)]
pub(super) struct ConnectionConfig {
    id: String,
    configuration: McpServerSettings,
    key: Key<McpServer>,
}
pub(super) struct ConnectionPlugin(ConnectionConfig);
impl ConnectionPlugin {
    pub(super) fn new(value: &ConnectionConfig) -> Self {
        Self(value.clone())
    }
}
impl Plugin for ConnectionPlugin {
    type Deps = <McpServerPlugin as Plugin>::Deps;
    fn name(&self) -> &str {
        "McpServer"
    }
    async fn apply(&self, ctx: &Ctx, auth: Arc<McpAuth>) -> rutis::Result {
        let plugin =
            McpServerPlugin::new(&self.0.id, self.0.configuration.clone(), self.0.key.clone())?;
        Plugin::apply(&plugin, ctx, auth).await
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpApplication {
    pub applied_revision: Option<String>,
    pub error: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
}
enum Command {
    Acquire {
        owner: McpOwner,
        project: Option<usize>,
        id: String,
        reply: oneshot::Sender<Result<Arc<McpConnection>>>,
    },
    Disconnect {
        source: McpSource,
        id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Replies after a refresh, so a write's withdrawals are settled.
    Settle { reply: oneshot::Sender<Result<()>> },
}
pub struct Publication {
    commands: mpsc::Sender<Command>,
    released: watch::Sender<()>,
    applications: StateCell<BTreeMap<McpSource, McpApplication>>,
}
impl Publication {
    pub(super) fn application(&self, source: McpSource) -> McpApplication {
        self.applications.get().remove(&source).unwrap_or_default()
    }
    pub(super) fn refresh(&self) {
        self.released.send_replace(());
    }
    /// Changes when a source's applied state changes, including entries that
    /// the publisher observed in external edits; Sessions re-read then.
    pub(super) fn applied(&self) -> watch::Receiver<BTreeMap<McpSource, McpApplication>> {
        self.applications.subscribe()
    }
    /// Waits until the worker has refreshed after this call, so generations
    /// the latest write withdrew are retired in every Session.
    pub(super) async fn settle(&self, cancel: &CancellationToken) -> Result<()> {
        let (reply, received) = oneshot::channel();
        self.request(Command::Settle { reply }, received, cancel)
            .await
    }
    async fn request<T>(
        &self,
        command: Command,
        received: oneshot::Receiver<Result<T>>,
        cancel: &CancellationToken,
    ) -> Result<T> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(McpError::Cancelled),
            result = async {
                self.commands.send(command).await.map_err(|_| McpError::Closed)?;
                received.await.map_err(|_| McpError::Closed)?
            } => result,
        }
    }
    pub(super) async fn acquire(
        &self,
        owner: McpOwner,
        project: Option<usize>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<Arc<McpConnection>> {
        let (reply, received) = oneshot::channel();
        self.request(
            Command::Acquire {
                owner,
                project,
                id,
                reply,
            },
            received,
            cancel,
        )
        .await
    }
    pub(super) async fn disconnect(
        &self,
        source: McpSource,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (reply, received) = oneshot::channel();
        self.request(Command::Disconnect { source, id, reply }, received, cancel)
            .await
    }
}

pub struct McpPublication {
    settings: Key<SettingsService>,
}
struct Worker {
    port: Arc<Publication>,
    commands: mpsc::Receiver<Command>,
    released: watch::Receiver<()>,
    settings_changed: watch::Receiver<()>,
}
impl McpPublication {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
        })
    }
    /// Registers the process row, keyed from `catalog`, and returns its name.
    pub fn register(
        builtins: &mut Modules,
        catalog: &ServiceCatalog,
    ) -> dim_agent_runtime::Result<String> {
        register_part(builtins, catalog, |names| Ok(Box::new(Self::new(names)?)))
    }
    /// Explicit standalone hosts register and install this top-level template.
    /// Agents instead register the template nested in their Session assembly.
    pub fn standalone_template(builtins: &mut Modules) -> serde_json::Value {
        builtins.builtins.register_with::<serde_json::Value, _, _>(
            "StandaloneMcpServer",
            |build, _| {
                let value = build.value::<ConnectionConfig>().ok_or_else(|| {
                    rutis::Error::from(ErrorKind::PluginFailed("missing MCP instance value".into()))
                })?;
                Ok(ConnectionPlugin::new(&value))
            },
        );
        json!({"id":GROUP,"group":true,"instanced":true,"config":[{"id":"StandaloneMcpServer","name":"StandaloneMcpServer"}]})
    }
}
impl Worker {
    async fn run(mut self, settings: Arc<SettingsService>, stop: CancellationToken) -> Result<()> {
        let mut projection = Projection {
            port: self.port.clone(),
            sources: BTreeMap::new(),
            generations: BTreeMap::new(),
            disconnected: BTreeMap::new(),
            cleanup_failed: BTreeSet::new(),
            application_failed: BTreeMap::new(),
        };
        let mut changes = self.settings_changed.clone();
        let operation = CancellationToken::new();
        loop {
            let command = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                command = self.commands.recv() => command,
                _ = self.released.changed() => None,
                _ = changes.changed() => None,
            };
            projection.refresh(&settings, &operation).await;
            match command {
                Some(Command::Acquire {
                    owner,
                    project,
                    id,
                    reply,
                }) => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = projection
                        .acquire(&settings, owner, project, id, &operation)
                        .await;
                    if let Err(Ok(lease)) = reply.send(result) {
                        let references = projection
                            .generations
                            .iter()
                            .filter(|(_, g)| {
                                Arc::ptr_eq(&g.service, &lease.service)
                                    && g.lease.strong_count() == 1
                            })
                            .map(|(reference, _)| reference.clone())
                            .collect();
                        drop(lease);
                        projection.retire(references).await;
                        projection.publish();
                    }
                }
                Some(Command::Disconnect { source, id, reply }) => {
                    let result = projection.disconnect(source, id).await;
                    let _ = reply.send(result);
                }
                Some(Command::Settle { reply }) => {
                    let _ = reply.send(Ok(()));
                }
                None => {}
            }
        }
        self.commands.close();
        while self.commands.try_recv().is_ok() {}
        let references = projection.generations.keys().cloned().collect();
        let failed = projection.retire(references).await;
        projection.publish();
        if failed || !projection.cleanup_failed.is_empty() {
            Err(McpServerError::Cleanup.into())
        } else {
            Ok(())
        }
    }
}
impl DynPlugin for McpPublication {
    fn name(&self) -> &str {
        "McpPublication"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let (commands, receiver) = mpsc::channel(64);
            let (released, releases) = watch::channel(());
            let port = Arc::new(Publication {
                commands,
                released,
                applications: StateCell::new(BTreeMap::new()),
            });
            let worker = Worker {
                port: port.clone(),
                commands: receiver,
                released: releases,
                settings_changed: config::settings_changes(ctx)?,
            };
            ctx.provide_as::<Publication>(port)?;
            let stop = ctx.cancellation_token();
            ctx.effect(move || {
                let task = tokio::spawn(worker.run(settings, stop.clone()));
                async move {
                    stop.cancel();
                    Ok(task.await??)
                }
            })?;
            Ok(Effect::Done)
        })
    }
}

struct Source {
    revision: String,
    configured: BTreeSet<String>,
    /// Effective configurations, by server id.
    current: BTreeMap<String, McpServerSettings>,
    /// The source could not be read: selection from it is unavailable.
    error: Option<&'static str>,
    /// Read, but applied with every project entry disabled because the user's
    /// enablement record could not be read. Selection stays available.
    degraded: Option<&'static str>,
}
struct Generation {
    source: McpSource,
    id: String,
    configuration: McpServerSettings,
    owner: McpOwner,
    instance: rutis_loader::edit::Instance,
    service: Arc<McpServer>,
    lease: Weak<McpConnection>,
    /// Set before a withdrawal retires it, so its holders report the entry
    /// as disabled rather than as a failed connection.
    withdrawn: Arc<AtomicBool>,
}
struct Projection {
    port: Arc<Publication>,
    sources: BTreeMap<McpSource, Source>,
    generations: BTreeMap<String, Generation>,
    disconnected: BTreeMap<(McpSource, String), String>,
    cleanup_failed: BTreeSet<McpSource>,
    application_failed: BTreeMap<(String, McpSource, String), (String, CancellationToken)>,
}
impl Projection {
    /// The entry no longer exists enabled. A known entry that is disabled is
    /// withdrawn even while its source is unreadable: the user's decision does
    /// not depend on the project file. A missing entry is withdrawn only when
    /// its source was read.
    fn withdrawn(&self, generation: &Generation) -> bool {
        self.sources.get(&generation.source).is_some_and(|source| {
            match source.current.get(&generation.id) {
                Some(config) => !enabled(config),
                None => source.error.is_none(),
            }
        })
    }
    async fn refresh(&mut self, settings: &SettingsService, cancel: &CancellationToken) {
        let projects = settings.project_sources();
        let enablement = if projects.is_empty() {
            Ok(McpProjectEnablement::default())
        } else {
            settings
                .read::<McpProjectEnablement>(SettingsScope::User, cancel)
                .await
                .map(|snapshot| snapshot.document)
        };
        let unreadable = enablement.is_err();
        let enablement = enablement.unwrap_or_default();
        for source in [McpSource::User, McpSource::Plugins].into_iter().chain(
            projects
                .into_iter()
                .map(|(root, _)| McpSource::Project(root)),
        ) {
            let read = match source.scope() {
                Some(scope) => settings
                    .read::<McpSettings>(scope, cancel)
                    .await
                    .map(|snapshot| (snapshot.revision, snapshot.document.mcp_servers)),
                None => settings
                    .read::<McpPluginServers>(SettingsScope::User, cancel)
                    .await
                    .map(|snapshot| (snapshot.revision, snapshot.document.entries().collect())),
            };
            let (revision, servers) = match read {
                Ok(read) => read,
                Err(_) => {
                    let intent = self.sources.entry(source).or_insert_with(|| Source {
                        revision: String::new(),
                        configured: BTreeSet::new(),
                        current: BTreeMap::new(),
                        error: None,
                        degraded: None,
                    });
                    intent.error = Some("MCP source could not be read");
                    if let McpSource::Project(root) = source {
                        let workspace =
                            config::workspace(settings, SettingsScope::Project(root)).ok();
                        for (id, config) in &mut intent.current {
                            let (McpServerSettings::Stdio { enabled, .. }
                            | McpServerSettings::Remote { enabled, .. }) = config;
                            *enabled = Some(
                                workspace
                                    .as_deref()
                                    .is_some_and(|workspace| enablement.contains(workspace, id)),
                            );
                        }
                    }
                    continue;
                }
            };
            self.disconnected
                .retain(|(owner, _), disconnected| *owner != source || *disconnected == revision);
            let configured = servers.keys().cloned().collect();
            let mut current = BTreeMap::new();
            let mut error = None;
            for (id, config) in servers {
                let config = match source.scope() {
                    Some(scope) => config::effective(settings, scope, &id, config, &enablement),
                    None => Ok(config),
                };
                let Ok(config) = config else {
                    error = Some("MCP configuration could not be resolved");
                    continue;
                };
                if !self.disconnected.contains_key(&(source, id.clone())) {
                    current.insert(id, config);
                }
            }
            let degraded = (unreadable && matches!(source, McpSource::Project(_)))
                .then_some("MCP project enablement could not be read");
            self.sources.insert(
                source,
                Source {
                    revision,
                    configured,
                    current,
                    error,
                    degraded,
                },
            );
        }
        // Bindings keep a superseded generation while its entry stays enabled,
        // so a replacement never breaks a request in flight. Disabling the
        // entry retires it in every Session, even with bindings and while its
        // source is unreadable; removing it does so once the source is read.
        // An unused generation closes even while current (ADR 004).
        let retired: Vec<_> = self
            .generations
            .iter()
            .filter(|(_, g)| {
                let withdrawn = self.withdrawn(g);
                if withdrawn {
                    g.withdrawn.store(true, Ordering::SeqCst);
                }
                g.owner.closed.is_cancelled() || g.lease.strong_count() == 0 || withdrawn
            })
            .map(|(reference, _)| reference.clone())
            .collect();
        self.retire(retired).await;
        self.application_failed
            .retain(|(_, source, _), (revision, closed)| {
                !closed.is_cancelled()
                    && self
                        .sources
                        .get(source)
                        .is_some_and(|intent| intent.revision == *revision)
            });
        self.publish();
    }
    fn publish(&self) {
        let applications: BTreeMap<_, _> = self
            .sources
            .iter()
            .map(|(source, intent)| {
                let inactive = intent.current.iter().any(|(id, configuration)| {
                    enabled(configuration)
                        && McpServerPlugin::new(id, configuration.clone(), Key::of()).is_err()
                }) || self.application_failed.keys().any(
                    |(_, failed_source, id)| {
                        failed_source == source && intent.current.get(id).is_some_and(enabled)
                    },
                );
                let error = intent
                    .error
                    .or(intent.degraded)
                    .or(inactive.then_some("MCP configuration is not active"));
                (
                    *source,
                    McpApplication {
                        applied_revision: error.is_none().then(|| intent.revision.clone()),
                        error,
                        cleanup_error: self
                            .cleanup_failed
                            .contains(source)
                            .then_some("MCP generation cleanup failed"),
                    },
                )
            })
            .collect();
        if self.port.applications.get() != applications {
            self.port.applications.set(applications);
        }
    }
    async fn retire(&mut self, references: Vec<String>) -> bool {
        let mut failed = false;
        for reference in references {
            if let Some(generation) = self.generations.remove(&reference)
                && (generation.instance.view.shutdown().await.is_err()
                    || generation.instance.view.state().error.is_some())
            {
                self.cleanup_failed.insert(generation.source);
                failed = true;
            }
        }
        failed
    }
    async fn acquire(
        &mut self,
        settings: &SettingsService,
        owner: McpOwner,
        project: Option<usize>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<Arc<McpConnection>> {
        if !owner.admits() {
            return Err(McpError::Closed);
        }
        let mut selected = None;
        for source in [McpSource::User, McpSource::Plugins]
            .into_iter()
            .chain(project.map(McpSource::Project))
        {
            let intent =
                self.sources
                    .get(&source)
                    .ok_or(crate::settings::SettingsError::Invalid(
                        "unauthorized project",
                    ))?;
            if intent.error.is_some() {
                return Err(McpError::Unavailable);
            }
            if intent.configured.contains(&id) {
                selected = Some(source);
            }
        }
        let source = selected.ok_or(McpError::UnknownServer)?;
        if self.disconnected.remove(&(source, id.clone())).is_some() {
            self.refresh(settings, cancel).await;
        }
        let configuration = self
            .sources
            .get(&source)
            .and_then(|s| s.current.get(&id))
            .ok_or(McpError::UnknownServer)?
            .clone();
        if !enabled(&configuration) {
            return Err(McpError::Disabled);
        }
        if let Some(generation) = self.generations.values_mut().find(|g| {
            g.owner.id == owner.id
                && g.source == source
                && g.id == id
                && g.configuration == configuration
                && !matches!(
                    g.service.state(),
                    McpServerState::Closed | McpServerState::Failed
                )
        }) {
            if let Some(lease) = generation.lease.upgrade() {
                return Ok(lease);
            }
            let lease = connection(&self.port, generation);
            generation.lease = Arc::downgrade(&lease);
            return Ok(lease);
        }
        let reference = format!("mcp:{}", uuid::Uuid::new_v4());
        let key = Key::<McpServer>::dynamic(reference.clone()).instance(owner.scope);
        McpServerPlugin::new(&id, configuration.clone(), key.clone())?;
        let created = owner
            .loader
            .create_instance(GROUP)
            .with(ConnectionConfig {
                id: id.clone(),
                configuration: configuration.clone(),
                key: key.clone(),
            })
            .await;
        let instance = match created {
            Ok(instance) => instance,
            Err(_) => {
                self.application_failed.insert(
                    (owner.id.clone(), source, id.clone()),
                    (self.sources[&source].revision.clone(), owner.closed.clone()),
                );
                self.publish();
                return Err(McpError::Unavailable);
            }
        };
        let service = owner.ctx.get_at(&key);
        if !owner.admits()
            || cancel.is_cancelled()
            || service.is_none()
            || instance
                .report
                .iter()
                .any(|(_, result)| !matches!(result, InstanceResult::Active))
        {
            if instance.view.shutdown().await.is_err() || instance.view.state().error.is_some() {
                self.cleanup_failed.insert(source);
            }
            if owner.admits() && !cancel.is_cancelled() {
                self.application_failed.insert(
                    (owner.id.clone(), source, id.clone()),
                    (self.sources[&source].revision.clone(), owner.closed.clone()),
                );
            }
            self.publish();
            return Err(if !owner.admits() {
                McpError::Closed
            } else {
                McpError::Unavailable
            });
        }
        self.application_failed
            .remove(&(owner.id.clone(), source, id.clone()));
        self.publish();
        let mut generation = Generation {
            source,
            id,
            configuration,
            owner,
            instance,
            service: service.unwrap(),
            lease: Weak::new(),
            withdrawn: Default::default(),
        };
        let lease = connection(&self.port, &generation);
        generation.lease = Arc::downgrade(&lease);
        self.generations.insert(reference, generation);
        Ok(lease)
    }
    async fn disconnect(&mut self, source: McpSource, id: String) -> Result<()> {
        if let Some(intent) = self.sources.get_mut(&source) {
            self.disconnected
                .insert((source, id.clone()), intent.revision.clone());
            intent.current.remove(&id);
        }
        let references = self
            .generations
            .iter()
            .filter(|(_, g)| g.source == source && g.id == id)
            .map(|(reference, _)| reference.clone())
            .collect();
        let failed = self.retire(references).await;
        self.publish();
        if failed {
            Err(McpServerError::Cleanup.into())
        } else {
            Ok(())
        }
    }
}
fn enabled(config: &McpServerSettings) -> bool {
    match config {
        McpServerSettings::Stdio { enabled, .. } | McpServerSettings::Remote { enabled, .. } => {
            enabled.unwrap_or(true)
        }
    }
}
fn connection(port: &Publication, generation: &Generation) -> Arc<McpConnection> {
    Arc::new(McpConnection {
        service: generation.service.clone(),
        released: Some(port.released.clone()),
        withdrawn: generation.withdrawn.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpChange, McpPlugin, McpService};
    use dim_agent_runtime::wait_active;
    use rutis_loader::Layer;
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Faults {
        fail: AtomicBool,
        block: AtomicBool,
        entered: Notify,
        release: Notify,
        cleaned: AtomicUsize,
    }
    struct Probe(ConnectionPlugin, Arc<Faults>);
    impl Plugin for Probe {
        type Deps = <ConnectionPlugin as Plugin>::Deps;
        fn name(&self) -> &str {
            "McpServer"
        }
        async fn apply(&self, ctx: &Ctx, deps: Self::Deps) -> rutis::Result {
            let faults = self.1.clone();
            ctx.effect(move || async move {
                faults.cleaned.fetch_add(1, Ordering::SeqCst);
                if faults.fail.load(Ordering::SeqCst) {
                    Err(McpServerError::Cleanup.into())
                } else {
                    Ok(())
                }
            })?;
            if self.1.block.load(Ordering::SeqCst) {
                self.1.entered.notify_one();
                let cancel = ctx.cancellation_token();
                tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = self.1.release.notified() => {} }
            }
            Plugin::apply(&self.0, ctx, deps).await
        }
    }
    struct MissingService;
    struct Waiting;
    impl Plugin for Waiting {
        type Deps = Arc<MissingService>;
        fn name(&self) -> &str {
            "Waiting"
        }
        async fn apply(&self, _: &Ctx, _: Self::Deps) -> rutis::Result {
            unreachable!("the undeclared host service must remain unavailable")
        }
    }
    struct Fixture {
        root: Ctx,
        loader: Loader,
        owner: McpOwner,
        service: Arc<McpService>,
        faults: Arc<Faults>,
        settings_view: rutis::host::FiberView,
        stop: CancellationToken,
        home: std::path::PathBuf,
    }
    impl Fixture {
        async fn new() -> Self {
            Self::with_extra(None).await
        }
        async fn with_extra(extra: Option<&str>) -> Self {
            let home = std::env::temp_dir().join(format!("mcp-instance-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&home).unwrap();
            let home = home.canonicalize().unwrap();
            std::fs::write(
                home.join("mcp.json"),
                json!({"mcpServers":{"docs":{"command":"first"}}}).to_string(),
            )
            .unwrap();
            let root = Ctx::root().unwrap();
            let faults = Arc::new(Faults::default());
            let catalog = crate::catalog();
            let mut builtins = Modules::new();
            let name = McpPublication::register(&mut builtins, &catalog).unwrap();
            let mut template = McpPublication::standalone_template(&mut builtins);
            if let Some(extra) = extra {
                let name = if extra == "waiting" {
                    register_part(&mut builtins, &catalog, |_| Ok(Box::new(Waiting))).unwrap()
                } else {
                    "MissingPlugin".to_owned()
                };
                template["config"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"extra","name":name}));
            }
            let fault = faults.clone();
            builtins.builtins.register_with::<serde_json::Value, _, _>(
                "StandaloneMcpServer",
                move |build, _| {
                    let config = build.value::<ConnectionConfig>().unwrap();
                    Ok(Probe(ConnectionPlugin::new(&config), fault.clone()))
                },
            );
            let plugin = rutis_loader::LoaderPlugin::new(builtins, Default::default());
            let loader = plugin.handle();
            let view = root.plugin(plugin);
            let stop = CancellationToken::new();
            wait_active(&root, &[view], Duration::from_secs(5), &stop)
                .await
                .unwrap();
            let assembly = Layer::new(
                "assembly",
                vec![rutis_loader::Patch {
                    insert: Some(vec![json!({"id":name,"name":name}), template]),
                    ..Default::default()
                }],
            );
            loader
                .reconcile(vec![assembly.clone()], None)
                .await
                .unwrap();
            let owner = McpOwner::new(root.clone(), loader.clone());
            root.provide_as::<dim_agent_runtime::PartRegistry>(Arc::new(
                dim_agent_runtime::PartRegistry::default(),
            ))
            .unwrap();
            let views = [
                root.plugin(
                    dim_agent_capabilities::file_storage::FileStoragePlugin::new(vec![
                        home.clone(),
                    ]),
                ),
                root.plugin(
                    crate::settings::SettingsPlugin::new(
                        &Names::check(&catalog, &[], None),
                        0,
                        vec![0],
                    )
                    .unwrap(),
                ),
                root.plugin(crate::methods::ManagementMethodsPlugin::typed(&catalog).unwrap()),
                root.plugin(McpPlugin::new(&Names::check(&catalog, &[], None), 0).unwrap()),
            ];
            wait_active(&root, &views, Duration::from_secs(5), &stop)
                .await
                .unwrap();
            let service = root.get::<McpService>().unwrap();
            Self {
                root,
                loader,
                owner,
                service,
                faults,
                settings_view: views[1].clone(),
                stop,
                home,
            }
        }
        async fn acquire(&self) -> Arc<McpConnection> {
            self.service
                .server(self.owner.clone(), None, "docs".into(), &self.stop)
                .await
                .unwrap()
        }
        async fn close(self) {
            self.faults.fail.store(false, Ordering::SeqCst);
            self.stop.cancel();
            tokio::time::timeout(Duration::from_secs(5), self.root.shutdown())
                .await
                .unwrap()
                .unwrap();
            std::fs::remove_dir_all(self.home).unwrap();
        }
    }
    #[tokio::test]
    async fn publication_reload_and_stop_retire_retained_connections() {
        let mut fixture = Fixture::new().await;
        let old = fixture.acquire().await;
        let publication = fixture
            .loader
            .entries()
            .into_iter()
            .find(|entry| entry.id == "McpPublication")
            .unwrap()
            .view
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), publication.restart())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.state(), McpServerState::Closed);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(service) = fixture.root.get::<McpService>()
                    && !Arc::ptr_eq(&service, &fixture.service)
                {
                    fixture.service = service;
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let replacement = fixture.acquire().await;
        assert!(!Arc::ptr_eq(&old, &replacement));
        tokio::time::timeout(Duration::from_secs(5), fixture.settings_view.restart())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replacement.state(), McpServerState::Closed);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(service) = fixture.root.get::<McpService>()
                    && !Arc::ptr_eq(&service, &fixture.service)
                {
                    fixture.service = service;
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let replacement = fixture.acquire().await;
        tokio::time::timeout(Duration::from_secs(5), publication.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replacement.state(), McpServerState::Closed);
        assert!(matches!(
            fixture
                .service
                .server(fixture.owner.clone(), None, "docs".into(), &fixture.stop)
                .await,
            Err(McpError::Closed)
        ));
        fixture.close().await;
    }
    #[tokio::test]
    async fn publication_shutdown_reports_retirement_cleanup_failure() {
        let fixture = Fixture::new().await;
        let retained = fixture.acquire().await;
        fixture.faults.fail.store(true, Ordering::SeqCst);
        let publication = fixture
            .loader
            .entries()
            .into_iter()
            .find(|entry| entry.id == "McpPublication")
            .unwrap()
            .view
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), publication.shutdown())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(retained.state(), McpServerState::Closed);
        assert!(
            fixture
                .service
                .application(SettingsScope::User)
                .cleanup_error
                .is_some()
        );
        fixture.close().await;
    }
    #[tokio::test]
    async fn every_failed_or_waiting_instance_row_rejects_and_rolls_back_the_connection() {
        for extra in ["failed", "waiting"] {
            let fixture = Fixture::with_extra(Some(extra)).await;
            fixture.faults.fail.store(true, Ordering::SeqCst);
            let result = fixture
                .service
                .server(fixture.owner.clone(), None, "docs".into(), &fixture.stop)
                .await;
            assert!(matches!(result, Err(McpError::Unavailable)));
            assert_eq!(fixture.faults.cleaned.load(Ordering::SeqCst), 1);
            let application = fixture.service.application(SettingsScope::User);
            assert!(
                application.applied_revision.is_none()
                    && application.error.is_some()
                    && application.cleanup_error.is_some()
            );
            assert!(
                fixture
                    .loader
                    .entries()
                    .iter()
                    .all(|entry| entry.id != "StandaloneMcpServer" || entry.view.is_none())
            );
            fixture.close().await;
        }
    }
    #[tokio::test]
    async fn project_disconnect_drains_all_owners_despite_cleanup_failure() {
        let fixture = Fixture::new().await;
        let cancel = &fixture.stop;
        let catalog = fixture.service.catalog(Some(0), cancel).await.unwrap();
        let revision = fixture
            .service
            .update(
                SettingsScope::Project(0),
                catalog.project_revision.unwrap(),
                "docs".into(),
                McpChange::Put(serde_json::from_value(json!({"command":"project"})).unwrap()),
                cancel,
            )
            .await
            .unwrap();
        fixture
            .service
            .update(
                SettingsScope::Project(0),
                revision,
                "docs".into(),
                McpChange::SetEnabled(true),
                cancel,
            )
            .await
            .unwrap();
        let first = fixture
            .service
            .server(fixture.owner.clone(), Some(0), "docs".into(), cancel)
            .await
            .unwrap();
        let other = fixture
            .service
            .server(
                McpOwner::new(fixture.root.clone(), fixture.loader.clone()),
                Some(0),
                "docs".into(),
                cancel,
            )
            .await
            .unwrap();
        fixture.faults.fail.store(true, Ordering::SeqCst);
        let result = fixture
            .service
            .disconnect(SettingsScope::Project(0), "docs".into(), cancel)
            .await;
        assert!(matches!(
            result,
            Err(McpError::Server(McpServerError::Cleanup))
        ));
        assert_eq!(fixture.faults.cleaned.load(Ordering::SeqCst), 2);
        for connection in [first, other] {
            assert_eq!(connection.state(), McpServerState::Closed);
        }
        assert!(
            fixture
                .service
                .application(SettingsScope::Project(0))
                .cleanup_error
                .is_some()
        );
        fixture.close().await;
    }
    #[tokio::test]
    async fn disconnect_drains_every_generation_after_a_plugin_cleanup_error() {
        let fixture = Fixture::new().await;
        fixture.faults.fail.store(true, Ordering::SeqCst);
        let first = fixture.acquire().await;
        let other_owner = McpOwner::new(fixture.root.clone(), fixture.loader.clone());
        let other = fixture
            .service
            .server(other_owner, None, "docs".into(), &fixture.stop)
            .await
            .unwrap();
        let catalog = fixture.service.catalog(None, &fixture.stop).await.unwrap();
        fixture
            .service
            .update(
                SettingsScope::User,
                catalog.user_revision,
                "docs".into(),
                McpChange::Put(serde_json::from_value(json!({"command":"second"})).unwrap()),
                &fixture.stop,
            )
            .await
            .unwrap();
        let second = fixture.acquire().await;
        assert!(!Arc::ptr_eq(&first, &second) && !Arc::ptr_eq(&first, &other));
        let result = fixture
            .service
            .disconnect(SettingsScope::User, "docs".into(), &fixture.stop)
            .await;
        assert!(matches!(
            result,
            Err(McpError::Server(McpServerError::Cleanup))
        ));
        assert_eq!(fixture.faults.cleaned.load(Ordering::SeqCst), 3);
        for connection in [first, second, other] {
            assert_eq!(connection.state(), McpServerState::Closed);
        }
        fixture.faults.fail.store(false, Ordering::SeqCst);
        let replacement = fixture.acquire().await;
        assert_eq!(replacement.state(), McpServerState::Disconnected);
        assert!(
            fixture
                .service
                .application(SettingsScope::User)
                .cleanup_error
                .is_some()
        );
        fixture.close().await;
    }
    #[tokio::test]
    async fn cancelled_instance_creation_is_rolled_back_and_cleanup_failure_is_visible() {
        let fixture = Fixture::new().await;
        fixture.faults.block.store(true, Ordering::SeqCst);
        fixture.faults.fail.store(true, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let request = {
            let (service, owner, cancel) = (
                fixture.service.clone(),
                fixture.owner.clone(),
                cancel.clone(),
            );
            tokio::spawn(async move { service.server(owner, None, "docs".into(), &cancel).await })
        };
        tokio::time::timeout(Duration::from_secs(5), fixture.faults.entered.notified())
            .await
            .unwrap();
        cancel.cancel();
        assert!(matches!(request.await.unwrap(), Err(McpError::Cancelled)));
        fixture.faults.block.store(false, Ordering::SeqCst);
        fixture.faults.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture
                .service
                .application(SettingsScope::User)
                .cleanup_error
                .is_none()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.faults.cleaned.load(Ordering::SeqCst), 1);
        assert!(
            fixture
                .loader
                .entries()
                .iter()
                .all(|entry| entry.view.is_none() || entry.id != "StandaloneMcpServer")
        );
        fixture.faults.fail.store(false, Ordering::SeqCst);
        fixture.acquire().await;
        fixture.close().await;
    }
}
