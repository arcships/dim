//! Plugin management (docs/design/plugins/PluginManager.md): the plugin
//! directory format, the loader's layers and the editable `user` layer's
//! persistence, and the `plugin` management namespace.
//!
//! The loader composes `assembly` (host rows), `defaults` (the default config
//! file and the embedder's rows), `user` (the only editable layer, persisted
//! as `plugins.json` in the App Server home) and `locked` (rows that cannot
//! be disabled). Whether a plugin is installed, enabled or locked is read
//! only from the composed layers.
mod directory;
mod manager;
mod operations;
mod persist;
mod tools;

pub use directory::{DefaultsFile, read_plugin};
pub(crate) use manager::PluginManagerPlugin;
pub(crate) use operations::Runtimes;
pub use persist::UserLayer;

use rutis_loader::{Layer, Patch};
use serde_json::{Value, json};

/// Layer names, lowest first.
pub const ASSEMBLY: &str = "assembly";
pub const DEFAULTS: &str = "defaults";
pub const USER: &str = "user";
pub const LOCKED: &str = "locked";

/// What the default config file decides besides its plugins' rows.
#[derive(Clone, Debug, Default)]
pub struct PluginDefaults {
    /// Rows that start disabled.
    pub disabled: Vec<String>,
    /// Rows that cannot be disabled or replaced.
    pub locked: Vec<String>,
}

/// The `defaults` layer: the default plugins' rows and the rows that start
/// disabled.
pub(crate) fn defaults_layer(rows: Vec<Value>, defaults: &PluginDefaults) -> Layer {
    let mut patches = vec![];
    if !rows.is_empty() {
        patches.push(Patch {
            insert: Some(rows),
            ..Default::default()
        });
    }
    patches.extend(defaults.disabled.iter().map(|id| set(id, true)));
    Layer::new(DEFAULTS, patches)
}

/// The `locked` layer: above the editable one, so rutis refuses to disable
/// these rows.
pub(crate) fn locked_layer(defaults: &PluginDefaults) -> Layer {
    Layer::new(
        LOCKED,
        defaults.locked.iter().map(|id| set(id, false)).collect(),
    )
}

fn set(id: &str, disabled: bool) -> Patch {
    let mut overrides = serde_json::Map::new();
    overrides.insert("disabled".into(), json!(disabled));
    Patch {
        id: Some(id.to_owned()),
        overrides,
        ..Default::default()
    }
}

/// Whether `row` is a plugin directory's group row: `{id, group: true,
/// config: [children]}` (§3.2). `Loader::create` also gives it a `name`.
fn is_plugin_group(row: &Value) -> bool {
    row["group"] == true && row["instanced"] != true && row["config"].is_array()
}

/// The rows of other languages among `rows`: a plugin group's children, or
/// the row itself (an embedder may give one without a group). Any other row
/// that names a group stays a code row, which the foreign host refuses.
pub(crate) fn code_rows(rows: &[Value]) -> Vec<&Value> {
    rows.iter()
        .flat_map(|row| match is_plugin_group(row) {
            true => row["config"].as_array().expect("checked").iter().collect(),
            false => vec![row],
        })
        .collect()
}

/// Creates the directories the plugins' code rows declare under the product
/// root, owner-only: installing a plugin provisions them.
pub(crate) fn provision(rows: &[&Value], product: &std::path::Path) -> std::io::Result<()> {
    for directory in rows
        .iter()
        .filter_map(|row| row["directories"].as_array())
        .flatten()
        .filter_map(Value::as_str)
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(product.join(directory))?;
    }
    Ok(())
}

/// The rows the `user` layer inserts at the top level.
pub(crate) fn inserted(patches: &[Patch]) -> Vec<Value> {
    patches
        .iter()
        .filter(|patch| patch.id.is_none())
        .filter_map(|patch| patch.insert.clone())
        .flatten()
        .collect()
}
