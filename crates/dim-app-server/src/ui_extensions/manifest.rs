use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Title {
    pub default: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MethodScope {
    Session,
    Branch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub name: String,
    pub scope: MethodScope,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub id: String,
    pub title: Title,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub entry: String,
    pub placement: String,
    pub requires: Vec<String>,
    pub methods: Vec<Method>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Package {
    Supported { root: PathBuf, panels: Vec<Panel> },
    Unsupported { version: u64 },
    Rejected { message: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u64,
    root: String,
    panels: Vec<Panel>,
}

pub(super) fn relative(value: &str) -> Result<&Path, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(['\\', '\0', '?', '#'])
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Expected a root-relative resource path".into());
    }
    Ok(path)
}

impl Package {
    pub fn read(dir: &Path, value: Value) -> Result<Self, String> {
        let version = value
            .get("version")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or("UI version must be a positive integer")?;
        if version != 1 {
            return Ok(Self::Unsupported { version });
        }
        let manifest: Manifest =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        debug_assert_eq!(manifest.version, 1);
        let root = dir
            .join(relative(&manifest.root)?)
            .canonicalize()
            .map_err(|_| "UI root is unreadable")?;
        if !root.starts_with(dir) || !root.is_dir() {
            return Err("UI root leaves the plugin directory or is not a directory".into());
        }
        if manifest.panels.is_empty() {
            return Err("UI panels must not be empty".into());
        }
        let mut ids = BTreeSet::new();
        for panel in &manifest.panels {
            if panel.id.is_empty()
                || panel.id.len() > 64
                || !panel.id.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || index > 0 && matches!(byte, b'-' | b'_')
                })
                || !ids.insert(&panel.id)
            {
                return Err("UI panel ids must be unique lowercase identifiers".into());
            }
            if panel.title.default.trim().is_empty()
                || panel
                    .title
                    .translations
                    .values()
                    .any(|value| value.trim().is_empty())
            {
                return Err("UI titles must not be empty".into());
            }
            let resolved = root
                .join(relative(&panel.entry)?)
                .canonicalize()
                .map_err(|_| "UI entry is unreadable")?;
            if !resolved.starts_with(&root) || !resolved.is_file() {
                return Err("UI entry leaves its resource root or is not a file".into());
            }
            if let Some(icon) = &panel.icon {
                let icon = root.join(relative(icon)?);
                if let Ok(resolved) = icon.canonicalize()
                    && !resolved.starts_with(&root)
                {
                    return Err("UI icon leaves its resource root".into());
                }
            }
            let mut required = BTreeSet::new();
            if panel
                .requires
                .iter()
                .any(|name| name.is_empty() || !required.insert(name))
            {
                return Err("UI capability names must be nonempty and unique".into());
            }
            let mut methods = BTreeSet::new();
            if panel.methods.iter().any(|method| {
                !dim_agent_capabilities::methods::valid_name(&method.name)
                    || !methods.insert(&method.name)
            }) {
                return Err("UI method names must be valid and unique".into());
            }
        }
        Ok(Self::Supported {
            root,
            panels: manifest.panels,
        })
    }
}
