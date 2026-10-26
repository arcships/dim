//! PluginManager: the `plugin` management namespace (PluginManager §8.1).
//! Each operation is a call to the running loader, which serializes
//! operations, persists the editable layer and returns once the tree has
//! settled.
use crate::methods::ManagementMethods;
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, params, reply};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, Key, TypeKey};
use rutis_loader::report::{EntryInfo, EntryStatus};
use rutis_loader::{Loader, LoaderError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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

pub struct PluginManagerPlugin {
    loader: Key<Loader>,
    methods: Key<ManagementMethods>,
}
impl PluginManagerPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            loader: names.key()?,
            methods: names.key()?,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Plugin {
    id: String,
    kind: &'static str,
    enabled: bool,
    locked: bool,
    status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    waiting_for: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabled {
    id: String,
    enabled: bool,
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
fn list(loader: &Loader) -> Vec<Plugin> {
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

fn wire(error: LoaderError) -> RpcError {
    let kind = match &error {
        LoaderError::UnknownEntry(_) => "plugin_not_found",
        LoaderError::OverriddenByLayer { .. } => "plugin_locked",
        LoaderError::NotOwned { .. } => "not_removable",
        _ => "plugin_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

impl DynPlugin for PluginManagerPlugin {
    fn name(&self) -> &str {
        "PluginManager"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.loader.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let loader = ctx.require_at(&self.loader)?;
            let methods = ctx.require_at(&self.methods)?;
            methods.register(
                ctx,
                "plugin",
                vec![
                    MethodDecl::new("plugin.list", false, Interruption::Drain),
                    MethodDecl::new("plugin.set_enabled", true, Interruption::Report),
                ],
                Arc::new(move |method, value, _stopping, _| {
                    let loader = loader.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "plugin.list" => reply(list(&loader)),
                            "plugin.set_enabled" => {
                                let SetEnabled { id, enabled } = params(value)?;
                                // Only top-level rows are plugins.
                                if loader.get(&id).is_none_or(|entry| entry.parent.is_some()) {
                                    return Err(RpcError::app(
                                        -32000,
                                        "plugin_not_found",
                                        format!("no plugin {id:?}"),
                                    ));
                                }
                                loader.set_disabled(&id, !enabled).await.map_err(wire)?;
                                let plugin =
                                    list(&loader).into_iter().find(|plugin| plugin.id == id);
                                reply(serde_json::json!({
                                    "status": plugin.map_or("inactive", |plugin| plugin.status)
                                }))
                            }
                            _ => unreachable!("registered plugin management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
