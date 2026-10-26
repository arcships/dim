//! The plugin directory and the default config file (PluginManager §2, §7),
//! read once where they enter the App Server.
use super::PluginDefaults;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The row file of a plugin with code: the fields its row has, with `entry`
/// relative to the directory.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RowFile {
    id: Option<String>,
    entry: String,
    #[serde(default)]
    inject: Vec<String>,
    #[serde(default)]
    provides: Map<String, Value>,
    #[serde(default)]
    instance_names: Map<String, Value>,
    #[serde(default = "empty")]
    config: Value,
    /// Directories under the product root its code keeps files in, which
    /// installation provisions (process-typescript §5.2).
    #[serde(default)]
    directories: Vec<PathBuf>,
}
fn empty() -> Value {
    json!({})
}

const ROW_FILE: &str = "plugin.json";

/// The group row of the plugin in `directory`, an absolute path: its code
/// row, if the directory has `plugin.json`. `id` overrides the row file's
/// and the directory's name.
pub fn read_plugin(directory: &Path, id: Option<&str>) -> Result<Value, String> {
    let directory = std::fs::canonicalize(directory)
        .map_err(|error| format!("cannot read plugin {}: {error}", directory.display()))?;
    let file = directory.join(ROW_FILE);
    let row: Option<RowFile> = match std::fs::read(&file) {
        Ok(bytes) => Some(
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid {}: {error}", file.display()))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot read {}: {error}", file.display())),
    };
    let Some(row) = row else {
        return Err(format!(
            "{} has no {ROW_FILE}: a plugin needs code or content",
            directory.display()
        ));
    };
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let id = id.or(row.id.as_deref()).unwrap_or(name).to_owned();
    if !valid_id(&id) {
        return Err(format!(
            "invalid plugin id {id:?}: use [a-z][a-z0-9-]{{0,63}}"
        ));
    }
    let entry = match row.entry.strip_prefix("py:") {
        Some(module) if !module.is_empty() => row.entry.clone(),
        Some(_) => return Err(format!("{}: empty py: entry", file.display())),
        None => {
            let path = directory.join(&row.entry);
            if !path.starts_with(&directory) || !path.is_file() {
                return Err(format!(
                    "{}: entry {} is not a file in the plugin",
                    file.display(),
                    row.entry
                ));
            }
            path.to_string_lossy().into_owned()
        }
    };
    if let Some(directory) = row.directories.iter().find(|directory| {
        directory.is_absolute()
            || directory
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
    }) {
        return Err(format!(
            "{}: directory {} must be relative, inside the product root",
            file.display(),
            directory.display()
        ));
    }
    let mut code = json!({
        "id": format!("{id}.code"),
        "name": entry,
        "root": directory,
        "inject": row.inject,
        "provides": row.provides,
        "instanceNames": row.instance_names,
        "config": row.config,
    });
    if !row.directories.is_empty() {
        code["directories"] = json!(row.directories);
    }
    Ok(json!({ "id": id, "group": true, "config": [code] }))
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The default config file: the plugins on by default, the rows that start
/// disabled and the rows that cannot be disabled. Paths are relative to the
/// file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsFile {
    #[serde(default)]
    plugins: Vec<PathBuf>,
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    locked: Vec<String>,
}
impl DefaultsFile {
    /// The default plugins' rows and the file's other decisions.
    pub fn read(path: &Path) -> Result<(Vec<Value>, PluginDefaults), String> {
        let unreadable = |error: std::io::Error| {
            format!("cannot read plugin defaults {}: {error}", path.display())
        };
        let path = std::fs::canonicalize(path).map_err(unreadable)?;
        let bytes = std::fs::read(&path).map_err(unreadable)?;
        let file: DefaultsFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid plugin defaults {}: {error}", path.display()))?;
        let base = path.parent().expect("a file has a directory");
        let rows = file
            .plugins
            .iter()
            .map(|plugin| read_plugin(&base.join(plugin), None))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            rows,
            PluginDefaults {
                disabled: file.disabled,
                locked: file.locked,
            },
        ))
    }
}
