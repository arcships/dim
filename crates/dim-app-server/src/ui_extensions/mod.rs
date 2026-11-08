pub mod manifest;
mod resources;
pub(crate) use resources::Read;
pub use resources::{PublishFile, ResourceRef, UiResources};

use crate::methods::ManagementMethods;
use dim_agent_capabilities::{
    file_storage::{EntryKind, FileStorage},
    methods::{Interruption, MethodCall, MethodDecl, params, reply},
};
use dim_agent_runtime::{Names, PartRegistry, PartScope};
use manifest::{Package, Panel};
use resources::{Reads, ResourceError, UiResourcesPlugin, file_error, gone, read_page};
use rutis::{
    Ctx,
    host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{io::AsyncWriteExt, sync::watch};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Extension {
    pub plugin_name: String,
    pub generation: String,
    pub panels: Vec<Panel>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unavailable {
    pub plugin_name: String,
    pub message: String,
}
#[derive(Serialize)]
pub struct Directory {
    pub revision: u64,
    pub extensions: Vec<Extension>,
    pub errors: Vec<Unavailable>,
}
/// The `ui.extensions.read` request: one file of the generation
/// `ui.extensions.list` named, from `offset` (default 0) up to `limit` bytes
/// (default [`resources::DEFAULT_PAGE`], clamped to [`resources::MAX_PAGE`]).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExtensionRead {
    generation: String,
    path: String,
    offset: Option<u64>,
    limit: Option<usize>,
}
struct Snapshot {
    extension: Extension,
    directory: PathBuf,
    paths: BTreeSet<String>,
    reads: Reads,
}
#[derive(Default)]
struct Catalog {
    entries: BTreeMap<String, Arc<Snapshot>>,
    errors: BTreeMap<String, Unavailable>,
}

pub(crate) struct Configuration {
    cache: PathBuf,
    sources: Mutex<BTreeMap<String, (String, Package)>>,
    revision: AtomicU64,
}
impl Configuration {
    pub fn new(cache: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            cache,
            sources: Mutex::default(),
            revision: AtomicU64::new(0),
        })
    }
    fn revision(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::SeqCst) + 1
    }
}

pub struct UiExtensions {
    configuration: Arc<Configuration>,
    files: Arc<FileStorage>,
    cache: PathBuf,
    catalog: Mutex<Catalog>,
    changes: watch::Sender<u64>,
    updating: tokio::sync::Mutex<()>,
    reads: Reads,
}
impl UiExtensions {
    pub fn directory(&self) -> Directory {
        let catalog = self.catalog.lock().unwrap();
        Directory {
            revision: *self.changes.borrow(),
            extensions: catalog
                .entries
                .values()
                .map(|entry| entry.extension.clone())
                .collect(),
            errors: catalog.errors.values().cloned().collect(),
        }
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub fn plugins(&self) -> Vec<String> {
        let catalog = self.catalog.lock().unwrap();
        catalog
            .entries
            .keys()
            .chain(catalog.errors.keys())
            .cloned()
            .collect()
    }
    pub fn plugin_refusal(&self, plugin: &str) -> Option<String> {
        self.catalog
            .lock()
            .unwrap()
            .errors
            .get(plugin)
            .map(|error| error.message.clone())
    }
    fn changed(&self) {
        self.changes.send_replace(self.configuration.revision());
    }
    pub async fn register(
        &self,
        plugin: &str,
        name: &str,
        package: Option<&Package>,
    ) -> Result<(), String> {
        let Some(package) = package else {
            self.withdraw(plugin).await;
            return Ok(());
        };
        let _update = self.updating.lock().await;
        let _operation = self.reads.enter().map_err(|error| error.message)?;
        self.configuration
            .sources
            .lock()
            .unwrap()
            .insert(plugin.into(), (name.into(), package.clone()));
        let prepared = match package {
            Package::Supported { root, panels } => self.prepare(name, root, panels).await,
            Package::Unsupported { version } => Err(format!("Unsupported UI version {version}")),
            Package::Rejected { message } => Err(message.clone()),
        };
        let old = {
            let mut catalog = self.catalog.lock().unwrap();
            let old = catalog.entries.remove(plugin);
            catalog.errors.remove(plugin);
            match &prepared {
                Ok(snapshot) => {
                    catalog.entries.insert(plugin.into(), snapshot.clone());
                }
                Err(message) => {
                    catalog.errors.insert(
                        plugin.into(),
                        Unavailable {
                            plugin_name: name.into(),
                            message: message.clone(),
                        },
                    );
                }
            }
            self.changed();
            old
        };
        if let Some(old) = old {
            self.remove(old).await;
        }
        prepared.map(|_| ())
    }
    pub async fn withdraw(&self, plugin: &str) {
        let _update = self.updating.lock().await;
        self.configuration.sources.lock().unwrap().remove(plugin);
        let old = {
            let mut catalog = self.catalog.lock().unwrap();
            let old = catalog.entries.remove(plugin);
            let error = catalog.errors.remove(plugin);
            if old.is_some() || error.is_some() {
                self.changed();
            }
            old
        };
        if let Some(old) = old {
            self.remove(old).await;
        }
    }
    async fn prepare(
        &self,
        name: &str,
        root: &Path,
        panels: &[Panel],
    ) -> Result<Arc<Snapshot>, String> {
        let generation = uuid::Uuid::new_v4().to_string();
        let directory = self.cache.join(&generation);
        crate::create_private_directory(&directory).map_err(|error| error.to_string())?;
        let result = self.copy(root, &directory, panels).await.and_then(|paths| {
            if panels.iter().any(|panel| !paths.contains(&panel.entry)) {
                Err("UI entry disappeared during registration".into())
            } else {
                Ok(paths)
            }
        });
        match result {
            Ok(paths) => Ok(Arc::new(Snapshot {
                extension: Extension {
                    plugin_name: name.into(),
                    generation,
                    panels: panels
                        .iter()
                        .cloned()
                        .map(|mut panel| {
                            panel.icon = panel.icon.filter(|icon| paths.contains(icon));
                            panel
                        })
                        .collect(),
                },
                directory,
                paths,
                reads: Reads::new(self.reads.stop.child_token()),
            })),
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&directory).await;
                Err(error)
            }
        }
    }
    async fn copy(
        &self,
        root: &Path,
        destination: &Path,
        panels: &[Panel],
    ) -> Result<BTreeSet<String>, String> {
        let cancel = &self.reads.stop;
        let source = self
            .files
            .register_root(
                root.parent().ok_or("UI root needs a parent")?.to_path_buf(),
                cancel,
            )
            .await
            .map_err(|error| error.to_string())?;
        let base = PathBuf::from(root.file_name().ok_or("UI root needs a basename")?);
        let mut pending = vec![base.clone()];
        let mut paths = BTreeSet::new();
        while let Some(directory) = pending.pop() {
            let path = self
                .files
                .path(source, &directory)
                .map_err(|error| error.to_string())?;
            for entry in self
                .files
                .list(&path, cancel)
                .await
                .map_err(|error| error.to_string())?
            {
                let relative = directory.join(&entry.name);
                let target = relative.strip_prefix(&base).expect("descendant of UI root");
                match entry.kind {
                    EntryKind::Directory => {
                        tokio::fs::create_dir(destination.join(target))
                            .await
                            .map_err(|error| error.to_string())?;
                        pending.push(relative);
                    }
                    EntryKind::File => {
                        if target
                            .extension()
                            .is_some_and(|extension| extension == "map")
                        {
                            continue;
                        }
                        let path = self
                            .files
                            .path(source, &relative)
                            .map_err(|error| error.to_string())?;
                        let name = target
                            .to_str()
                            .ok_or("UI resources must have UTF-8 paths")?
                            .replace(std::path::MAIN_SEPARATOR, "/");
                        manifest::relative(&name)?;
                        let copied: Result<(), String> = async {
                            let file = self
                                .files
                                .open_read(&path, cancel)
                                .await
                                .map_err(|error| error.to_string())?;
                            let mut output = tokio::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(destination.join(target))
                                .await
                                .map_err(|error| error.to_string())?;
                            let mut offset = 0;
                            while offset < file.len() {
                                let bytes = file
                                    .read_at(offset, 65536, cancel)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                output
                                    .write_all(&bytes)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                offset += bytes.len() as u64;
                            }
                            output.flush().await.map_err(|error| error.to_string())?;
                            Ok(())
                        }
                        .await;
                        match copied {
                            Ok(()) => {
                                paths.insert(name);
                            }
                            Err(_)
                                if !cancel.is_cancelled()
                                    && panels
                                        .iter()
                                        .any(|panel| panel.icon.as_deref() == Some(&name))
                                    && !panels.iter().any(|panel| panel.entry == name) =>
                            {
                                let _ = tokio::fs::remove_file(destination.join(target)).await;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    EntryKind::Other => {
                        return Err(
                            "UI roots may contain only regular files and directories".into()
                        );
                    }
                }
            }
        }
        Ok(paths)
    }
    async fn remove(&self, snapshot: Arc<Snapshot>) {
        snapshot.reads.close().await;
        if let Err(error) = tokio::fs::remove_dir_all(&snapshot.directory).await {
            tracing::warn!(%error, "UI snapshot cleanup failed");
        }
    }
    pub(crate) async fn read(
        &self,
        generation: &str,
        relative: &str,
    ) -> Result<Read, ResourceError> {
        manifest::relative(relative).map_err(|error| ResourceError::invalid(&error))?;
        let snapshot = self
            .catalog
            .lock()
            .unwrap()
            .entries
            .values()
            .find(|entry| entry.extension.generation == generation)
            .cloned()
            .ok_or_else(gone)?;
        let token = snapshot.reads.enter()?;
        if !snapshot.paths.contains(relative) {
            return Err(gone());
        }
        let path = self
            .files
            .resolve(&snapshot.directory.join(relative))
            .map_err(file_error)?;
        let file = self
            .files
            .open_read(&path, &snapshot.reads.stop)
            .await
            .map_err(file_error)?;
        let (media_type, attachment) = mime(relative);
        Ok(Read {
            file,
            stop: snapshot.reads.stop.clone(),
            token,
            media_type: media_type.into(),
            attachment,
            etag: Some(format!("\"{generation}\"")),
        })
    }
    async fn close(&self) {
        self.reads.close().await;
        let _update = self.updating.lock().await;
        let entries = {
            let mut catalog = self.catalog.lock().unwrap();
            catalog.errors.clear();
            let entries = std::mem::take(&mut catalog.entries);
            self.changed();
            entries
        };
        for entry in entries.into_values() {
            self.remove(entry).await;
        }
        let _ = tokio::fs::remove_dir(&self.cache).await;
    }
}
fn mime(path: &str) -> (&'static str, bool) {
    match Path::new(path).extension().and_then(|part| part.to_str()) {
        Some("js" | "mjs") => ("text/javascript", false),
        Some("css") => ("text/css", false),
        Some("json") => ("application/json", false),
        Some("wasm") => ("application/wasm", false),
        Some("png") => ("image/png", false),
        Some("jpg" | "jpeg") => ("image/jpeg", false),
        Some("svg") => ("image/svg+xml", false),
        Some("webp") => ("image/webp", false),
        Some("gif") => ("image/gif", false),
        Some("avif") => ("image/avif", false),
        Some("woff") => ("font/woff", false),
        Some("woff2") => ("font/woff2", false),
        Some("txt") => ("text/plain", false),
        _ => ("application/octet-stream", true),
    }
}

pub(crate) struct UiExtensionsPlugin {
    files: Key<FileStorage>,
    methods: Key<ManagementMethods>,
    parts: Key<PartRegistry>,
    configuration: Arc<Configuration>,
}
impl UiExtensionsPlugin {
    pub fn new(names: &Names<'_>, configuration: Arc<Configuration>) -> rutis::Result<Self> {
        Ok(Self {
            files: names.key()?,
            methods: names.key()?,
            parts: names.key()?,
            configuration,
        })
    }
}
impl DynPlugin for UiExtensionsPlugin {
    fn name(&self) -> &str {
        "UiExtensions"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.files.erase(), self.methods.erase(), self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let files = ctx.require_at(&self.files)?;
            crate::create_private_directory(&self.configuration.cache)
                .map_err(|error| ErrorKind::PluginFailed(error.into()))?;
            let mut stale = tokio::fs::read_dir(&self.configuration.cache)
                .await
                .map_err(|error| ErrorKind::PluginFailed(error.into()))?;
            while let Some(entry) = stale
                .next_entry()
                .await
                .map_err(|error| ErrorKind::PluginFailed(error.into()))?
            {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| uuid::Uuid::parse_str(name).is_ok())
                {
                    tokio::fs::remove_dir_all(entry.path())
                        .await
                        .map_err(|error| ErrorKind::PluginFailed(error.into()))?;
                }
            }
            let cache = self
                .configuration
                .cache
                .join(uuid::Uuid::new_v4().to_string());
            crate::create_private_directory(&cache)
                .map_err(|error| ErrorKind::PluginFailed(error.into()))?;
            let service = Arc::new(UiExtensions {
                configuration: self.configuration.clone(),
                files,
                cache,
                catalog: Mutex::default(),
                changes: watch::channel(self.configuration.revision()).0,
                updating: tokio::sync::Mutex::new(()),
                reads: Reads::new(ctx.cancellation_token()),
            });
            let cleanup = service.clone();
            ctx.effect(move || async move {
                cleanup.close().await;
                Ok(())
            })?;
            service
                .files
                .register_root(self.configuration.cache.clone(), &ctx.cancellation_token())
                .await
                .map_err(|error| ErrorKind::PluginFailed(error.into()))?;
            let sources = self.configuration.sources.lock().unwrap().clone();
            for (plugin, (name, package)) in sources {
                if let Err(error) = service.register(&plugin, &name, Some(&package)).await {
                    tracing::warn!(%plugin, %error, "UI content restore refused");
                }
            }
            let handler = service.clone();
            ctx.require_at(&self.methods)?.register(
                ctx,
                "ui.extensions",
                vec![
                    MethodDecl::new("ui.extensions.list", false, Interruption::Report),
                    MethodDecl::new("ui.extensions.read", false, Interruption::Report),
                ],
                Arc::new(move |call, stop, _progress| {
                    let MethodCall {
                        method,
                        params: value,
                        ..
                    } = call;
                    let service = handler.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "ui.extensions.list" => {
                                let _: crate::protocol::Empty = params(value)?;
                                reply(service.directory())
                            }
                            "ui.extensions.read" => {
                                let request: ExtensionRead = params(value)?;
                                let read = service
                                    .read(&request.generation, &request.path)
                                    .await
                                    .map_err(|error| error.wire())?;
                                reply(
                                    read_page(read, request.offset, request.limit, stop)
                                        .await
                                        .map_err(|error| error.wire())?,
                                )
                            }
                            _ => unreachable!("registered ui.extensions method"),
                        }
                    })
                }),
            )?;
            ctx.require_at(&self.parts)?.register(
                ctx,
                PartScope::Session,
                "UiResources",
                |mount| Ok(Some(Box::new(UiResourcesPlugin::new(mount.names)?))),
            )?;
            ctx.provide_as::<UiExtensions>(service)?;
            Ok(Effect::Done)
        })
    }
}
