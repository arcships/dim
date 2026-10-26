//! PluginManager: the `plugin` management namespace (PluginManager §8.1).
//! Each operation is a call to the running loader, which serializes
//! operations, persists the editable layer and returns once the tree has
//! settled.
use super::operations::{Failure, Operations, Runtimes, Source};
use super::tools::PluginTools;
use crate::methods::ManagementMethods;
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, params, reply};
use dim_agent_runtime::{Names, PartRegistry, PartScope};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, Key, TypeKey};
use rutis_loader::report::{EntryInfo, EntryStatus};
use rutis_loader::{Loader, ServiceCatalog};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Which layer put a row in place, read from the loader's layers.
struct Origins {
    /// Rows the `assembly` layer inserts.
    builtin: HashSet<String>,
    /// Rows the `defaults` layer inserts.
    default: HashSet<String>,
    /// Rows the `locked` layer locks.
    locked: HashSet<String>,
}
impl Origins {
    fn of(loader: &Loader) -> Self {
        let layers = loader.layers();
        let layer = |name: &str| layers.iter().find(|layer| layer.name == name);
        let inserted = |name: &str| -> HashSet<String> {
            layer(name)
                .map(|layer| super::inserted(&layer.patches))
                .unwrap_or_default()
                .iter()
                .filter_map(|row| row["id"].as_str().map(str::to_owned))
                .collect()
        };
        Self {
            builtin: inserted(super::ASSEMBLY),
            default: inserted(super::DEFAULTS),
            locked: layer(super::LOCKED)
                .map(|layer| {
                    layer
                        .patches
                        .iter()
                        .filter_map(|patch| patch.id.clone())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

pub(crate) struct PluginManagerPlugin {
    loader: Key<Loader>,
    methods: Key<ManagementMethods>,
    parts: Key<PartRegistry>,
    clones: Option<PathBuf>,
    product: Option<PathBuf>,
    runtimes: Option<Arc<Runtimes>>,
    catalog: ServiceCatalog,
}
impl PluginManagerPlugin {
    /// `clones` is where Git plugins are cloned; `runtimes` the foreign
    /// runtimes, absent without Node; `catalog` the loader's.
    pub fn new(
        names: &Names<'_>,
        clones: Option<PathBuf>,
        product: Option<PathBuf>,
        runtimes: Option<Arc<Runtimes>>,
        catalog: ServiceCatalog,
    ) -> rutis::Result<Self> {
        Ok(Self {
            loader: names.key()?,
            methods: names.key()?,
            parts: names.key()?,
            clones,
            product,
            runtimes,
            catalog,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Plugin {
    pub id: String,
    pub kind: &'static str,
    enabled: bool,
    pub locked: bool,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    waiting_for: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabled {
    id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Install {
    source: Source,
    id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ById {
    id: String,
}

/// A row's status, and its error when it has one.
fn status(entry: &EntryInfo) -> (&'static str, Option<String>) {
    match &entry.status {
        EntryStatus::Disabled => ("disabled", None),
        EntryStatus::Inactive | EntryStatus::Stopped => ("inactive", None),
        EntryStatus::Unresolved(error) => ("failed", Some(error.to_string())),
        EntryStatus::Running(snapshot) => match snapshot.state {
            FiberState::Active => ("running", None),
            FiberState::Pending => ("pending", None),
            FiberState::Failed => (
                "failed",
                Some(
                    snapshot
                        .error
                        .as_ref()
                        .map_or_else(|| "failed".into(), |error| error.to_string()),
                ),
            ),
            _ => ("loading", None),
        },
        _ => ("inactive", None),
    }
}

/// The plugins: the top-level rows. A plugin group's status is the worst of
/// its own and its children's.
pub(super) fn list(loader: &Loader) -> Vec<Plugin> {
    let origins = Origins::of(loader);
    let entries: Vec<EntryInfo> = loader
        .entries()
        .into_iter()
        .filter(|entry| entry.instance.is_none())
        .collect();
    let rank = |status: &str| match status {
        "failed" => 4,
        "pending" => 3,
        "loading" => 2,
        "running" => 1,
        _ => 0,
    };
    entries
        .iter()
        // An instanced group holds instances, not a plugin.
        .filter(|entry| entry.parent.is_none() && entry.options["instanced"] != true)
        .map(|entry| {
            let (mut state, mut error) = status(entry);
            let mut waiting_for = loader.waiting_for(entry);
            if state == "running" {
                for child in entries
                    .iter()
                    .filter(|child| child.parent.as_deref() == Some(&entry.id))
                {
                    let (child_state, child_error) = status(child);
                    if rank(child_state) > rank(state) {
                        (state, error) = (child_state, child_error);
                    }
                    waiting_for.extend(loader.waiting_for(child));
                }
            }
            let kind = if origins.builtin.contains(&entry.id) {
                "builtin"
            } else if origins.default.contains(&entry.id) {
                "default"
            } else {
                "installed"
            };
            Plugin {
                id: entry.id.clone(),
                kind,
                enabled: !matches!(entry.status, EntryStatus::Disabled),
                locked: origins.locked.contains(&entry.id),
                status: state,
                waiting_for,
                error,
            }
        })
        .collect()
}

/// An operation's result with the plugin's settled status.
fn settled(loader: &Loader, id: &str, mut result: Value) -> Value {
    let plugin = list(loader).into_iter().find(|plugin| plugin.id == id);
    result["id"] = json!(id);
    result["status"] = json!(plugin.as_ref().map_or("removed", |plugin| plugin.status));
    if let Some(error) = plugin.and_then(|plugin| plugin.error) {
        result["error"] = json!(error);
    }
    result
}

fn wire(failure: Failure) -> RpcError {
    match failure.code {
        "invalid_params" => RpcError::invalid(failure.message),
        code => RpcError::app(-32000, code, failure.message),
    }
}

impl DynPlugin for PluginManagerPlugin {
    fn name(&self) -> &str {
        "PluginManager"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.loader.erase(),
            self.methods.erase(),
            self.parts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let parts = ctx.require_at(&self.parts)?;
            let operations = Arc::new(Operations {
                loader: ctx.require_at(&self.loader)?,
                clones: self.clones.clone(),
                product: self.product.clone(),
                runtimes: self.runtimes.clone(),
                catalog: self.catalog.clone(),
            });
            // The agent's tools, in every Session (§8.3).
            let tools = operations.clone();
            parts.register(ctx, PartScope::Session, "PluginTools", move |mount| {
                Ok(Some(Box::new(PluginTools::new(
                    mount.names,
                    tools.clone(),
                )?)))
            })?;
            methods.register(
                ctx,
                "plugin",
                vec![
                    MethodDecl::new("plugin.list", false, Interruption::Drain),
                    MethodDecl::new("plugin.install", true, Interruption::Report),
                    MethodDecl::new("plugin.set_enabled", true, Interruption::Report),
                    MethodDecl::new("plugin.reload", true, Interruption::Report),
                    MethodDecl::new("plugin.update", true, Interruption::Report),
                    MethodDecl::new("plugin.remove", true, Interruption::Report),
                ],
                Arc::new(move |method, value, _stopping, _| {
                    let operations = operations.clone();
                    Box::pin(async move {
                        let loader = &operations.loader;
                        let (id, result) = match method.as_str() {
                            "plugin.list" => return reply(list(loader)),
                            "plugin.install" => {
                                let Install { source, id } = params(value)?;
                                let result = operations.install(source, id).await.map_err(wire)?;
                                (result["id"].as_str().unwrap_or_default().to_owned(), result)
                            }
                            "plugin.set_enabled" => {
                                let SetEnabled { id, enabled } = params(value)?;
                                let result =
                                    operations.set_enabled(&id, enabled).await.map_err(wire)?;
                                (id, result)
                            }
                            method => {
                                let ById { id } = params(value)?;
                                let result = match method {
                                    "plugin.reload" => operations.reload(&id).await,
                                    "plugin.update" => operations.update(&id).await,
                                    "plugin.remove" => operations.remove(&id).await,
                                    _ => unreachable!("registered plugin management method"),
                                }
                                .map_err(wire)?;
                                (id, result)
                            }
                        };
                        reply(settled(loader, &id, result))
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
