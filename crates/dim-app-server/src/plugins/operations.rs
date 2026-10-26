//! The operations behind `plugin.*` and the agent's `plugin_*` tools
//! (PluginManager §4): install, reload, update and remove, each a call to the
//! running loader. The loader serializes them, persists the `user` layer and
//! returns once the tree has settled.
use super::read_plugin;
use crate::session_typescript::SessionResolver;
use rutis_loader::edit::NewEntry;
use rutis_loader::runtime::RuntimeResolver;
use rutis_loader::{Loader, LoaderError, ServiceCatalog};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A failed operation: its wire code and message.
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}
impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl From<LoaderError> for Failure {
    fn from(error: LoaderError) -> Self {
        let code = match &error {
            LoaderError::UnknownEntry(_) => "plugin_not_found",
            LoaderError::OverriddenByLayer { .. } => "plugin_locked",
            LoaderError::NotOwned { .. } => "not_removable",
            LoaderError::InvalidEntry(message) if message.starts_with("duplicate id") => {
                "plugin_exists"
            }
            _ => "plugin_failed",
        };
        Self::new(code, error.to_string())
    }
}

/// Where a plugin comes from (`plugin.install`'s `source`).
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Source {
    Path {
        path: PathBuf,
    },
    Git {
        git: String,
        #[serde(rename = "ref")]
        reference: Option<String>,
    },
}

/// The foreign runtimes' resolvers, to admit a new code row and to forget a
/// changed module before it reloads. Absent without a Node runtime.
pub(crate) struct Runtimes {
    pub resolver: Arc<SessionResolver>,
    pub node: Arc<RuntimeResolver>,
    pub python: Option<Arc<RuntimeResolver>>,
}

pub(crate) struct Operations {
    pub loader: Arc<Loader>,
    /// `<home>/plugins`, where Git plugins are cloned; absent without a home.
    pub clones: Option<PathBuf>,
    /// The product root, where installation provisions the directories a
    /// plugin declares.
    pub product: Option<PathBuf>,
    pub runtimes: Option<Arc<Runtimes>>,
    /// The catalog the loader was built with: names a new plugin needs that
    /// it lacks work from the next start.
    pub catalog: ServiceCatalog,
}

impl Operations {
    pub async fn install(&self, source: Source, id: Option<String>) -> Result<Value, Failure> {
        let row = match source {
            Source::Path { path } => {
                if !path.is_absolute() {
                    return Err(Failure::new("invalid_plugin", "the path must be absolute"));
                }
                read_plugin(&path, id.as_deref())
                    .map_err(|error| Failure::new("invalid_plugin", error))?
            }
            Source::Git { git, reference } => {
                let id = match id {
                    Some(id) => id,
                    None => repository_name(&git)?,
                };
                let clones = self
                    .clones
                    .as_ref()
                    .ok_or_else(|| Failure::new("source_unavailable", "Git plugins need --home"))?;
                let directory = clones.join(&id);
                if self.loader.get(&id).is_some() {
                    return Err(Failure::new(
                        "plugin_exists",
                        format!("plugin {id:?} exists"),
                    ));
                }
                // No row names it: an interrupted install left it.
                if directory.exists() {
                    std::fs::remove_dir_all(&directory)
                        .map_err(|error| Failure::new("plugin_failed", error.to_string()))?;
                }
                std::fs::create_dir_all(clones)
                    .map_err(|error| Failure::new("plugin_failed", error.to_string()))?;
                clone(&git, reference.as_deref(), &directory).await?;
                match read_plugin(&directory, Some(&id)) {
                    Ok(row) => row,
                    Err(error) => {
                        let _ = std::fs::remove_dir_all(&directory);
                        return Err(Failure::new("invalid_plugin", error));
                    }
                }
            }
        };
        let id = row["id"].as_str().expect("plugin id").to_owned();
        if let Some(product) = &self.product {
            super::provision(&super::code_rows(std::slice::from_ref(&row)), product)
                .map_err(|error| Failure::new("plugin_failed", error.to_string()))?;
        }
        let restart = self.admit(&row)?;
        let created = self
            .loader
            .create(
                NewEntry {
                    id: Some(id.clone()),
                    name: id.clone(),
                    config: row["config"].clone(),
                    group: true,
                    ..Default::default()
                },
                None,
                None,
            )
            .await;
        if let Err(error) = created {
            if let Some(directory) = self.clone_of(&id) {
                let _ = std::fs::remove_dir_all(directory);
            }
            return Err(error.into());
        }
        Ok(json!({ "id": id, "restartRequired": restart }))
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value, Failure> {
        // Only top-level rows are plugins.
        if self
            .loader
            .get(id)
            .is_none_or(|entry| entry.parent.is_some())
        {
            return Err(Failure::new(
                "plugin_not_found",
                format!("no plugin {id:?}"),
            ));
        }
        self.loader.set_disabled(id, !enabled).await?;
        Ok(json!({}))
    }

    /// Reads the plugin's directory again and brings in its new code.
    pub async fn reload(&self, id: &str) -> Result<Value, Failure> {
        let (root, children) = self.plugin(id)?;
        let row =
            read_plugin(&root, Some(id)).map_err(|error| Failure::new("invalid_plugin", error))?;
        let restart = self.admit(&row)?;
        for child in row["config"].as_array().expect("children") {
            let child_id = child["id"].as_str().expect("child id");
            let Some(current) = children.iter().find(|entry| entry["id"] == child_id) else {
                return Err(Failure::new(
                    "restart_required",
                    format!("{child_id} is new; reinstall {id}"),
                ));
            };
            // Row fields the plugin changed are edits of the user layer.
            if current["config"] != child["config"] {
                self.loader
                    .update(child_id, child["config"].clone())
                    .await?;
            }
            if current["inject"] != child["inject"] {
                let inject = child["inject"]
                    .as_array()
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(|n| n.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                self.loader.set_inject(child_id, inject).await?;
            }
            if let (Some(runtimes), Some(name)) = (&self.runtimes, child["name"].as_str()) {
                let runtime = match &runtimes.python {
                    Some(python) if name.starts_with("py:") => python,
                    _ => &runtimes.node,
                };
                runtime.invalidate(name);
            }
            self.loader.reload(child_id).await?;
        }
        Ok(json!({ "restartRequired": restart }))
    }

    /// Moves a Git plugin's clone to the newest commit of the branch it
    /// tracks, then reloads it. A path plugin is only reloaded.
    pub async fn update(&self, id: &str) -> Result<Value, Failure> {
        self.plugin(id)?;
        let mut commit = Value::Null;
        if let Some(directory) = self.clone_of(id) {
            git(&directory, &["pull", "--ff-only", "--quiet"]).await?;
            commit = json!(git(&directory, &["rev-parse", "HEAD"]).await?.trim());
        }
        let mut result = self.reload(id).await?;
        result["commit"] = commit;
        Ok(result)
    }

    pub async fn remove(&self, id: &str) -> Result<Value, Failure> {
        self.plugin(id).or_else(|failure| match failure.code {
            // A built-in plugin has no directory; the loader refuses it.
            "not_reloadable" => Ok((PathBuf::new(), vec![])),
            _ => Err(failure),
        })?;
        self.loader.remove(id).await?;
        let mut result = json!({});
        if let Some(directory) = self.clone_of(id)
            && let Err(error) = std::fs::remove_dir_all(&directory)
        {
            result["cleanupError"] = json!(error.to_string());
        }
        Ok(result)
    }

    /// A plugin's directory and its children's rows as composed.
    fn plugin(&self, id: &str) -> Result<(PathBuf, Vec<Value>), Failure> {
        let entry = self
            .loader
            .get(id)
            .filter(|entry| entry.parent.is_none())
            .ok_or_else(|| Failure::new("plugin_not_found", format!("no plugin {id:?}")))?;
        let children: Vec<Value> = self
            .loader
            .entries()
            .into_iter()
            .filter(|child| child.parent.as_deref() == Some(id) && child.instance.is_none())
            .map(|child| child.options)
            .collect();
        let root = children
            .iter()
            .find_map(|child| child["root"].as_str())
            .filter(|_| entry.options["group"] == true)
            .ok_or_else(|| {
                Failure::new(
                    "not_reloadable",
                    format!("{id} is compiled into Dim: rebuild it, or replace it"),
                )
            })?;
        Ok((PathBuf::from(root), children))
    }

    /// The clone of a Git plugin, if `id` is one.
    fn clone_of(&self, id: &str) -> Option<PathBuf> {
        let directory = self.clones.as_ref()?.join(id);
        directory.join(".git").exists().then_some(directory)
    }

    /// Lets the runtime resolve the plugin's code row, and says whether the
    /// plugin needs names the running catalog lacks, which work from the
    /// next start (§4).
    fn admit(&self, row: &Value) -> Result<bool, Failure> {
        let mut restart = false;
        for child in row["config"].as_array().expect("children") {
            let Some(name) = child["name"].as_str() else {
                continue;
            };
            let runtimes = self.runtimes.as_ref().ok_or_else(|| {
                Failure::new(
                    "runtime_unavailable",
                    "plugins with code need the App Server's Node runtime",
                )
            })?;
            if name.starts_with("py:") {
                if runtimes.python.is_none() {
                    return Err(Failure::new(
                        "runtime_unavailable",
                        "Python plugins need the App Server's Python runtime",
                    ));
                }
                restart |= !runtimes.resolver.known(name);
            }
            let provides = child["provides"].as_object().cloned().unwrap_or_default();
            let build = rutis_loader::resolve::Build::default();
            restart |= provides
                .keys()
                .any(|service| self.catalog.key_in(service, &build).is_err());
            restart |= child["instanceNames"]
                .as_object()
                .is_some_and(|names| !names.is_empty())
                && !runtimes.resolver.known(name);
            restart |= child.get("settings").is_some() && !runtimes.resolver.known(name);
            runtimes.resolver.admit(name, provides);
        }
        Ok(restart)
    }
}

/// The last path segment of a Git URL, without `.git`.
fn repository_name(url: &str) -> Result<String, Failure> {
    url.trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .map(|name| name.trim_end_matches(".git").to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Failure::new("invalid_plugin", format!("cannot name a plugin from {url}")))
}

async fn clone(url: &str, reference: Option<&str>, directory: &Path) -> Result<(), Failure> {
    let mut args = vec!["clone", "--quiet", "--depth", "1"];
    if let Some(reference) = reference {
        args.extend(["--branch", reference]);
    }
    let directory = directory.to_string_lossy();
    args.extend([url, &directory]);
    git(Path::new("."), &args).await.map(drop)
}

/// Runs `git` in `directory` without repository hooks.
async fn git(directory: &Path, args: &[&str]) -> Result<String, Failure> {
    let output = tokio::process::Command::new("git")
        .current_dir(directory)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| Failure::new("source_unavailable", format!("git: {error}")))?;
    if !output.status.success() {
        return Err(Failure::new(
            "source_unavailable",
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
