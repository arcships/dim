//! Host-side source publication into the existing loader. Never run this worker
//! in a plugin's apply/dispose: reconciliation may need to dispose its callers.
use super::{
    McpConnection, McpError, Result,
    server::{McpServerError, McpServerPlugin, McpServerState},
};
use crate::settings::{McpServerSettings, SettingsDocument, SettingsService, SettingsSource};
use dim_agent_runtime::{Capability, KeysOf, StateCell, register_capability};
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypeKey, TypedPlugin, TypedPluginFactory};
use rutis_loader::{Builtins, Layer, Loader, Patch};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

type Configurations = Arc<Mutex<BTreeMap<String, (SettingsSource, Arc<McpServerSettings>)>>>;

/// Loader application is distinct from source persistence and connection state.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpApplication {
    pub applied_revision: Option<String>,
    pub error: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
}

enum Command {
    Acquire {
        project: Option<usize>,
        id: String,
        reply: oneshot::Sender<Result<Arc<McpConnection>>>,
    },
    Disconnect {
        source: SettingsSource,
        id: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

pub struct Publication {
    commands: mpsc::Sender<Command>,
    binding: watch::Sender<Option<Arc<SettingsService>>>,
    released: watch::Sender<()>,
    applications: StateCell<BTreeMap<SettingsSource, McpApplication>>,
}
impl Publication {
    pub(super) fn bind(&self, settings: Arc<SettingsService>) {
        self.binding.send_replace(Some(settings));
    }

    pub(super) fn application(&self, source: SettingsSource) -> McpApplication {
        self.applications.get().remove(&source).unwrap_or_default()
    }

    pub(super) fn refresh(&self) {
        self.released.send_replace(());
    }

    pub(super) async fn acquire(
        &self,
        project: Option<usize>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<Arc<McpConnection>> {
        let (reply, received) = oneshot::channel();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(McpError::Cancelled),
            result = async {
                self.commands.send(Command::Acquire { project, id, reply }).await.map_err(|_| McpError::Closed)?;
                received.await.map_err(|_| McpError::Closed)?
            } => result,
        }
    }

    pub(super) async fn disconnect(
        &self,
        source: SettingsSource,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let (reply, received) = oneshot::channel();
        // The host owns an admitted disconnect even if this waiter disappears.
        // Cancellation must release a disposing plugin that reconcile waits on.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(McpError::Cancelled),
            result = async {
                self.commands.send(Command::Disconnect { source, id, reply }).await.map_err(|_| McpError::Closed)?;
                received.await.map_err(|_| McpError::Closed)?
            } => result,
        }
    }
}

/// Registration is pure. The host starts `run` after its assembly is active and
/// joins it before shutting down the loader. Standalone hosts can do the same.
pub struct McpPublication {
    port: Arc<Publication>,
    commands: mpsc::Receiver<Command>,
    binding: watch::Receiver<Option<Arc<SettingsService>>>,
    released: watch::Receiver<()>,
    configurations: Configurations,
    module: String,
}
impl McpPublication {
    pub fn register(builtins: &mut Builtins) -> (Self, String) {
        let (commands, receiver) = mpsc::channel(64);
        let (binding, bindings) = watch::channel(None);
        let (released, releases) = watch::channel(());
        let port = Arc::new(Publication {
            commands,
            binding,
            released,
            applications: StateCell::new(BTreeMap::new()),
        });
        let configurations = Configurations::default();
        let module = "McpServer".to_owned();
        builtins.register_raw(
            module.clone(),
            rutis::TypedFactory::with_keys(ServerFactory(configurations.clone()), ((),)),
            None,
        );
        let host = Host(port.clone());
        let name = register_capability(builtins, move || host.clone());
        (
            Self {
                port,
                commands: receiver,
                binding: bindings,
                released: releases,
                configurations,
                module,
            },
            name,
        )
    }

    pub async fn run(
        mut self,
        root: Ctx,
        loader: Loader,
        assembly: Layer,
        stop: CancellationToken,
    ) {
        let mut projection = Projection {
            root,
            loader,
            assembly,
            port: self.port.clone(),
            configurations: self.configurations,
            module: self.module,
            sources: BTreeMap::new(),
            generations: BTreeMap::new(),
            disconnected: BTreeMap::new(),
            cleanup_failed: BTreeSet::new(),
        };
        let mut changes = watch::channel(0).1;
        let mut settings = None;
        loop {
            let command = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = self.binding.changed() => {
                    settings = self.binding.borrow_and_update().clone();
                    if let Some(settings) = &settings { changes = settings.subscribe(); }
                    None
                },
                command = self.commands.recv() => command,
                _ = self.released.changed() => None,
                _ = changes.changed(), if settings.is_some() => None,
            };
            let Some(settings) = &settings else { continue };
            projection.refresh(settings, &stop).await;
            match command {
                Some(Command::Acquire { project, id, reply }) => {
                    if reply.is_closed() {
                        continue;
                    }
                    let result = projection.acquire(settings, project, id, &stop).await;
                    let _ = reply.send(result);
                }
                Some(Command::Disconnect { source, id, reply }) => {
                    let result = projection.disconnect(source, id).await;
                    let _ = reply.send(result);
                }
                None => {}
            }
        }
        self.commands.close();
        // Dropping queued reply senders rejects waiters. Loader remains the only
        // owner of plugin disposal, including on process shutdown.
    }
}

#[derive(Clone)]
struct Host(Arc<Publication>);
impl TypedPlugin for Host {
    type Deps = ();
    fn name(&self) -> &str {
        "McpPublication"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): (),
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.provide_as(TypeKey::of::<Publication>(), self.0.clone())?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for Host {
    fn keys(&self) -> KeysOf<Self> {}
}

struct ServerFactory(Configurations);
impl TypedPluginFactory<Value> for ServerFactory {
    type Plugin = McpServerPlugin;
    fn name(&self) -> &str {
        "McpServer"
    }
    fn build(&self, config: &Value) -> std::result::Result<McpServerPlugin, CordisError> {
        let reference = config
            .get("reference")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CordisError::PluginFailed("invalid MCP configuration reference".into())
            })?;
        let configuration = self
            .0
            .lock()
            .unwrap()
            .get(reference)
            .map(|(_, configuration)| configuration.clone())
            .ok_or_else(|| {
                CordisError::PluginFailed("MCP configuration reference expired".into())
            })?;
        McpServerPlugin::new((*configuration).clone(), key(reference))
            .map_err(|error| CordisError::PluginFailed(Box::new(error)))
    }
}

fn key(reference: &str) -> TypeKey {
    TypeKey::keyed_dynamic::<super::server::McpServer>(reference.to_owned())
}

struct Source {
    revision: String,
    configured: BTreeSet<String>,
    current: BTreeMap<String, String>,
    error: Option<&'static str>,
}
struct Generation {
    source: SettingsSource,
    id: String,
    configuration: Arc<McpServerSettings>,
    lease: Weak<McpConnection>,
    row: Value,
}
struct Projection {
    root: Ctx,
    loader: Loader,
    assembly: Layer,
    port: Arc<Publication>,
    configurations: Configurations,
    module: String,
    sources: BTreeMap<SettingsSource, Source>,
    generations: BTreeMap<String, Generation>,
    disconnected: BTreeMap<(SettingsSource, String), String>,
    cleanup_failed: BTreeSet<SettingsSource>,
}
impl Projection {
    async fn refresh(&mut self, settings: &SettingsService, cancel: &CancellationToken) {
        for (source, path) in settings.mcp_sources() {
            let snapshot = match settings.read(source, cancel).await {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    let intent = self.sources.entry(source).or_insert_with(|| Source {
                        revision: String::new(),
                        configured: BTreeSet::new(),
                        current: BTreeMap::new(),
                        error: None,
                    });
                    intent.error = Some("MCP source could not be read");
                    if matches!(source, SettingsSource::ProjectMcp(_)) {
                        intent.current.clear();
                    }
                    continue;
                }
            };
            let SettingsDocument::Mcp(document) = snapshot.document else {
                unreachable!("MCP source")
            };
            let configured = document.mcp_servers.keys().cloned().collect();
            self.disconnected
                .retain(|(owner, _), revision| *owner != source || *revision == snapshot.revision);
            let mut current = BTreeMap::new();
            let mut error = None;
            for (id, configuration) in document.mcp_servers {
                let Ok(configuration) = settings.mcp_effective(source, configuration) else {
                    error = Some("MCP configuration could not be resolved");
                    continue;
                };
                if self.disconnected.get(&(source, id.clone())) == Some(&snapshot.revision) {
                    continue;
                }
                if let Some(reference) = self.sources.get(&source).and_then(|s| s.current.get(&id))
                    && let Some(generation) = self.generations.get(reference)
                    && *generation.configuration == configuration
                {
                    current.insert(id, reference.clone());
                    continue;
                }
                // A rejected edit followed by restoration can reuse a healthy
                // generation still retained by accepted bindings.
                if let Some((reference, _)) =
                    self.generations.iter().find(|(reference, generation)| {
                        generation.source == source
                            && generation.id == id
                            && *generation.configuration == configuration
                            && self
                                .root
                                .get_as::<super::server::McpServer>(key(reference))
                                .is_some_and(|service| {
                                    !matches!(
                                        service.state(),
                                        McpServerState::Closed | McpServerState::Failed
                                    )
                                })
                    })
                {
                    current.insert(id, reference.clone());
                    continue;
                }
                // Logical identity is scope-qualified and restart-stable. The
                // suffix identifies a runtime generation, never a durable source.
                let scope = path
                    .as_ref()
                    .map(|p| p.to_str().expect("validated project path"))
                    .unwrap_or("user");
                let logical = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&(scope, &id)).expect("string tuple"))
                );
                let reference = format!("mcp:{logical}:{}", uuid::Uuid::new_v4());
                let disabled = match &configuration {
                    McpServerSettings::Stdio { enabled, .. }
                    | McpServerSettings::Remote { enabled, .. } => !enabled.unwrap_or(true),
                };
                let configuration = Arc::new(configuration);
                self.configurations
                    .lock()
                    .unwrap()
                    .insert(reference.clone(), (source, configuration.clone()));
                self.generations.insert(reference.clone(), Generation {
                    source, id: id.clone(), configuration, lease: Weak::new(),
                    row: json!({"id":reference,"name":self.module,"disabled":disabled,"config":{"reference":reference}}),
                });
                current.insert(id, reference);
            }
            self.sources.insert(
                source,
                Source {
                    revision: snapshot.revision,
                    configured,
                    current,
                    error,
                },
            );
        }
        self.publish().await;
    }

    async fn publish(&mut self) {
        self.generations.retain(|reference, generation| {
            self.sources
                .get(&generation.source)
                .and_then(|s| s.current.get(&generation.id))
                == Some(reference)
                || generation.lease.strong_count() > 0
        });
        let rows = self.generations.values().map(|g| g.row.clone()).collect();
        let layer = Layer::new(
            "mcp",
            vec![Patch {
                insert: Some(rows),
                ..Default::default()
            }],
        );
        let layers = vec![self.assembly.clone(), layer];
        let removed: Vec<_> = self
            .loader
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.owner == rutis_loader::Owner::Layer(1)
                    && !self.generations.contains_key(&entry.id)
            })
            .filter_map(|entry| {
                let source = self.configurations.lock().unwrap().get(&entry.id)?.0;
                Some((source, entry.view?))
            })
            .collect();
        let failed = if self.loader.layers() != layers {
            self.loader.reconcile(layers, None).await.is_err()
        } else {
            false
        };
        // Retain immutable private configuration until loader removal completes.
        let entries = self.loader.entries();
        self.configurations.lock().unwrap().retain(|reference, _| {
            self.generations.contains_key(reference)
                || entries.iter().any(|entry| &entry.id == reference)
        });
        // Reconcile drains every removed row but does not return disposal errors.
        // Observe terminal receipts; never initiate a second shutdown from leases.
        for (source, view) in removed {
            let state = view.state();
            if state.state != rutis::FiberState::Disposed || state.error.is_some() {
                self.cleanup_failed.insert(source);
            }
        }
        self.port.applications.set(
            self.sources
                .iter()
                .map(|(source, intent)| {
                    let applied = !failed
                        && intent.error.is_none()
                        && intent.current.values().all(|reference| {
                            entries.iter().any(|entry| {
                                entry.id == *reference
                                    && entry.rejected.is_none()
                                    && (entry.options["disabled"] == true
                                        || entry.view.as_ref().is_some_and(|v| {
                                            v.state().state == rutis::FiberState::Active
                                        }))
                            })
                        });
                    (
                        *source,
                        McpApplication {
                            applied_revision: applied.then(|| intent.revision.clone()),
                            error: intent
                                .error
                                .or((!applied).then_some("MCP configuration is not active")),
                            cleanup_error: self
                                .cleanup_failed
                                .contains(source)
                                .then_some("MCP generation cleanup failed"),
                        },
                    )
                })
                .collect(),
        );
    }

    async fn acquire(
        &mut self,
        settings: &SettingsService,
        project: Option<usize>,
        id: String,
        cancel: &CancellationToken,
    ) -> Result<Arc<McpConnection>> {
        // Resolve precedence from the same source snapshots that produced the
        // rows, not from a catalog read before this request entered the queue.
        let mut selected = None;
        for source in
            std::iter::once(SettingsSource::UserMcp).chain(project.map(SettingsSource::ProjectMcp))
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
        let failed = self
            .sources
            .get(&source)
            .and_then(|intent| intent.current.get(&id))
            .and_then(|reference| self.root.get_as::<super::server::McpServer>(key(reference)))
            .is_some_and(|service| {
                matches!(
                    service.state(),
                    McpServerState::Closed | McpServerState::Failed
                )
            });
        if failed {
            self.sources
                .get_mut(&source)
                .expect("existing source")
                .current
                .remove(&id);
        }
        if self.disconnected.remove(&(source, id.clone())).is_some() || failed {
            self.refresh(settings, cancel).await;
        }
        let source = self.sources.get(&source).ok_or(McpError::InvalidSource)?;
        if source.error.is_some() {
            return Err(McpError::Unavailable);
        }
        let reference = source.current.get(&id).ok_or(McpError::UnknownServer)?;
        let generation = self
            .generations
            .get_mut(reference)
            .expect("current generation");
        let disabled = match &*generation.configuration {
            McpServerSettings::Stdio { enabled, .. }
            | McpServerSettings::Remote { enabled, .. } => *enabled == Some(false),
        };
        if disabled {
            return Err(McpError::Disabled);
        }
        let Some(service) = self.root.get_as::<super::server::McpServer>(key(reference)) else {
            // Keep typed, secret-free configuration errors at the public port;
            // do not downcast or forward arbitrary loader diagnostics.
            McpServerPlugin::new((*generation.configuration).clone(), key(reference))?;
            return Err(McpError::Unavailable);
        };
        if let Some(lease) = generation.lease.upgrade()
            && Arc::ptr_eq(&lease.service, &service)
        {
            return Ok(lease);
        }
        let lease = Arc::new(McpConnection {
            service,
            released: self.port.released.clone(),
        });
        generation.lease = Arc::downgrade(&lease);
        Ok(lease)
    }

    async fn disconnect(&mut self, source: SettingsSource, id: String) -> Result<()> {
        let references: BTreeSet<_> = self
            .generations
            .iter()
            .filter(|(_, generation)| generation.source == source && generation.id == id)
            .map(|(reference, _)| reference.clone())
            .collect();
        let views: Vec<_> = self
            .loader
            .entries()
            .into_iter()
            .filter(|entry| references.contains(&entry.id))
            .filter_map(|entry| entry.view)
            .collect();
        if let Some(intent) = self.sources.get_mut(&source) {
            self.disconnected
                .insert((source, id.clone()), intent.revision.clone());
            intent.current.remove(&id);
        }
        self.generations
            .retain(|_, generation| generation.source != source || generation.id != id);
        self.publish().await;
        if views.iter().any(|view| {
            let state = view.state();
            state.state != rutis::FiberState::Disposed || state.error.is_some()
        }) {
            Err(McpServerError::Cleanup.into())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpChange, McpPlugin, McpService};
    use dim_agent_runtime::{capability_plugin, wait_active};
    use std::time::Duration;

    // Real OS reap failures are not portable to induce. Inject only the cleanup
    // fault; configure, replace and disconnect through the public directory.
    struct CleanupFailure(McpServerPlugin);
    impl TypedPlugin for CleanupFailure {
        type Deps = <McpServerPlugin as TypedPlugin>::Deps;
        fn name(&self) -> &str {
            "McpCleanupFailure"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
            deps: Self::Deps,
        ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
            Box::pin(async move {
                ctx.effect(|| {
                    Effect::AsyncDisposer(Box::new(|| {
                        Box::pin(async {
                            Err(CordisError::PluginFailed(Box::new(McpServerError::Cleanup)))
                        })
                    }))
                })?;
                self.0.apply(ctx, deps).await
            })
        }
    }
    struct FaultFactory(ServerFactory);
    impl TypedPluginFactory<Value> for FaultFactory {
        type Plugin = CleanupFailure;
        fn name(&self) -> &str {
            "McpCleanupFailure"
        }
        fn build(&self, config: &Value) -> std::result::Result<Self::Plugin, CordisError> {
            self.0.build(config).map(CleanupFailure)
        }
    }

    #[tokio::test]
    async fn disconnect_drains_every_generation_after_a_plugin_cleanup_error() {
        let home = std::env::temp_dir().join(format!("mcp-cleanup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&home).unwrap();
        let home = home.canonicalize().unwrap();
        std::fs::write(
            home.join("mcp.json"),
            json!({"mcpServers":{"docs":{"command":"first"}}}).to_string(),
        )
        .unwrap();
        let root = Ctx::root().unwrap();
        let mut builtins = Builtins::new();
        let (publication, name) = McpPublication::register(&mut builtins);
        builtins.register_raw(
            publication.module.clone(),
            rutis::TypedFactory::with_keys(
                FaultFactory(ServerFactory(publication.configurations.clone())),
                ((),),
            ),
            None,
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
            vec![Patch {
                insert: Some(vec![json!({"id":name,"name":name})]),
                ..Default::default()
            }],
        );
        loader
            .reconcile(vec![assembly.clone()], None)
            .await
            .unwrap();
        let worker = tokio::spawn(publication.run(root.clone(), loader, assembly, stop.clone()));
        let views = [
            root.plugin(capability_plugin(
                dim_agent_capabilities::file_storage::FileStoragePlugin::new(vec![home.clone()]),
            )),
            root.plugin(capability_plugin(crate::settings::SettingsPlugin::new(
                0,
                vec![],
            ))),
            root.plugin(capability_plugin(McpPlugin)),
        ];
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let directory = root.get::<McpService>().unwrap();
        let first = directory
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        let catalog = directory.catalog(None, &cancel).await.unwrap();
        directory
            .update(
                SettingsSource::UserMcp,
                catalog.user_revision,
                "docs".into(),
                McpChange::Put(serde_json::from_value(json!({"command":"second"})).unwrap()),
                &cancel,
            )
            .await
            .unwrap();
        let second = directory
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        let result = directory
            .disconnect(SettingsSource::UserMcp, "docs".into(), &cancel)
            .await;
        assert!(matches!(
            result,
            Err(McpError::Server(McpServerError::Cleanup))
        ));
        assert_eq!(first.state(), McpServerState::Closed);
        assert_eq!(second.state(), McpServerState::Closed);
        assert!(
            directory
                .application(SettingsSource::UserMcp)
                .cleanup_error
                .is_some()
        );
        let replacement = directory
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &replacement) && !Arc::ptr_eq(&second, &replacement));
        assert_eq!(replacement.state(), McpServerState::Disconnected);
        // A later successful application must not erase failed cleanup evidence.
        assert!(
            directory
                .application(SettingsSource::UserMcp)
                .cleanup_error
                .is_some()
        );
        stop.cancel();
        worker.await.unwrap();
        let _ = root.shutdown().await;
        std::fs::remove_dir_all(home).unwrap();
    }
}
