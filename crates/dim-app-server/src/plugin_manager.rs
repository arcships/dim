//! PluginManager (docs/design/plugins/PluginManager.md): the plugin
//! configuration of a host, kept in `<home>/plugins.json`. It keeps no
//! runtime state: rows are rutis-loader's, content is its owners'. It
//! carries out a configuration change by giving the loader and the content
//! owners (SkillsCatalog, Mcp) commands.
mod acquire;
mod record;
mod tools;

use crate::mcp::McpService;
use crate::methods::ManagementMethods;
use crate::settings::{SettingsService, SourceDeclaration};
use acquire::PluginsDir;
use dim_agent_capabilities::methods::{Interruption, MethodDecl, Progress, RpcError};
use dim_agent_capabilities::skills::{SkillIssue, SkillsCatalog};
use dim_agent_runtime::{Names, PartRegistry, PartScope};
pub use record::Defaults;
use record::{Choice, Installed, Kind, Manifest, Record, Source};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_loader::report::{EntryInfo, EntryStatus};
use rutis_loader::{Loader, LoaderError, Patch, ServiceCatalog, resolve::Build};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

/// The editable loader layer PluginManager's rows and overrides live in.
pub const LAYER: &str = "plugins";

/// A refused call: its wire code (A.2) and message.
#[derive(Debug)]
pub struct PluginError {
    pub code: &'static str,
    pub message: String,
}
impl PluginError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl From<PluginError> for RpcError {
    fn from(error: PluginError) -> Self {
        RpcError::service(error.code, error.message)
    }
}

fn not_found(what: &str) -> PluginError {
    PluginError::new("plugin_not_found", format!("no plugin {what:?}"))
}

/// The plugin a name uses (§2.2 `names.<name>.use`, resolved).
#[derive(Clone, Debug, PartialEq)]
enum Use {
    Builtin,
    Default,
    Installed(String),
}
impl Use {
    fn wire(&self) -> &str {
        match self {
            Use::Builtin => "builtin",
            Use::Default => "default",
            Use::Installed(id) => id,
        }
    }
}

/// The row id of the plugin a name uses, other than its built-in.
fn row_id(name: &str) -> String {
    format!("plugin:{name}")
}

/// The id a used plugin's own files are named by: its installed id, or a
/// default plugin's name.
fn own_id<'a>(name: &'a str, used: &'a Use) -> Option<&'a str> {
    match used {
        Use::Installed(id) => Some(id),
        Use::Default => Some(name),
        Use::Builtin => None,
    }
}

/// A Python plugin's package: its id with `-` as `_`.
fn python_package(id: &str) -> String {
    id.replace('-', "_")
}

/// The loader module of a plugin's code. A Python entry is
/// `py:<package>.<module>`, imported through the plugin's link under
/// `<home>/plugins/.python/` (§2.5); another entry is its file, which the
/// Node runtime loads.
fn module(id: &str, dir: &Path, code: &record::Code) -> String {
    if code.entry.extension().and_then(|e| e.to_str()) != Some("py") {
        return code.entry.to_string_lossy().into_owned();
    }
    let relative = code.entry.strip_prefix(dir).unwrap_or(&code.entry);
    let module: Vec<_> = relative
        .with_extension("")
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    format!("py:{}.{}", python_package(id), module.join("."))
}

fn is_python(code: &record::Code) -> bool {
    code.entry.extension().and_then(|e| e.to_str()) == Some("py")
}

/// The configuration PluginManager works on; it outlives a generation of
/// its row. What the assembly settles after reading it is set once, before
/// the loader starts.
pub(crate) struct State {
    record_path: PathBuf,
    dirs: PluginsDir,
    /// The canonical `pluginFiles` root, when the host has one.
    files: Option<PathBuf>,
    defaults: Defaults,
    /// The host's row-configuration patches by plugin name (§2.1),
    /// deep-merged over each manifest's `config`.
    configs: BTreeMap<String, Value>,
    record: Mutex<Record>,
    /// The built-in rows of the assembly.
    builtins: OnceLock<BTreeSet<String>>,
    /// The catalog the runtime started with (§4 `instanceNames`).
    catalog: OnceLock<ServiceCatalog>,
    /// The Node rows' resolver, told to forget a module before a reload.
    node: OnceLock<Option<Arc<rutis_loader::runtime::RuntimeResolver>>>,
    /// Held while the layer is derived from the record and reconciled, so
    /// the layer reconciled last is derived from the latest record.
    applying: tokio::sync::Mutex<()>,
}

/// The `plugins` layer startup reconciles with (§3.1).
pub(crate) struct StartupLayer {
    pub patches: Vec<Patch>,
    /// Built-in rows that are off: startup does not require them.
    pub off: Vec<String>,
    /// Rows of locked default plugins: startup requires them.
    pub locked: Vec<String>,
}

impl State {
    /// Startup (§3.1), first part: reads `plugins.json` and the default
    /// configuration file, and registers the `instanceNames` and process
    /// names of the plugins that may be used with the catalog. `home` is
    /// canonical.
    pub fn read(
        home: &Path,
        files: Option<&Path>,
        defaults: Option<&Path>,
        configs: &BTreeMap<String, Value>,
        catalog: &mut ServiceCatalog,
    ) -> Result<Arc<Self>, String> {
        let record_path = home.join("plugins.json");
        let record = Record::read(&record_path).map_err(|e| e.to_string())?;
        let mut defaults = match defaults {
            Some(path) => Defaults::read(path)?,
            None => Defaults::default(),
        };
        for (name, dir) in &mut defaults.plugins {
            *dir = std::fs::canonicalize(&*dir)
                .map_err(|error| format!("default plugin {name} at {}: {error}", dir.display()))?;
        }
        let none = BTreeSet::new();
        for name in names(&record, &defaults, &none) {
            let dir = using(&record, &defaults, &none, &name)
                .and_then(|used| dir_of(&record, &defaults, &name, &used));
            if let Some(Ok(Manifest {
                code: Some(code), ..
            })) = dir.as_deref().map(Manifest::read)
            {
                register_names(catalog, &code);
            }
        }
        Ok(Arc::new(Self {
            record_path,
            dirs: PluginsDir::new(home),
            files: files.map(Path::to_owned),
            defaults,
            configs: configs.clone(),
            record: Mutex::new(record),
            builtins: OnceLock::new(),
            catalog: OnceLock::new(),
            node: OnceLock::new(),
            applying: tokio::sync::Mutex::new(()),
        }))
    }

    /// Startup, second part, once the assembly's rows are known: the
    /// initial `plugins` layer, the built-in rows that are off, and the rows
    /// of locked default plugins, which startup requires as it does the
    /// built-in rows. Locked names must have a plugin.
    pub fn layer(
        &self,
        builtins: BTreeSet<String>,
        catalog: ServiceCatalog,
        node: Option<Arc<rutis_loader::runtime::RuntimeResolver>>,
    ) -> Result<StartupLayer, String> {
        let mut locked_rows = Vec::new();
        for name in &self.defaults.locked {
            if self.defaults.plugins.contains_key(name) && !builtins.contains(name) {
                locked_rows.push(row_id(name));
            } else if !builtins.contains(name) {
                return Err(format!("locked plugin {name} is not part of this build"));
            }
        }
        let _ = self.builtins.set(builtins);
        let _ = self.catalog.set(catalog);
        let _ = self.node.set(node);
        let record = self.record.lock().unwrap().clone();
        let (patches, off) = self.derive(&record);
        Ok(StartupLayer {
            patches,
            off,
            locked: locked_rows,
        })
    }

    /// The row configuration of the plugin `name` uses: the manifest's
    /// `config` with the host's patch for that name deep-merged over it
    /// (§2.1).
    fn config_of(&self, name: &str, config: &Value) -> Value {
        let mut config = config.clone();
        if let Some(patch) = self.configs.get(name) {
            merge(&mut config, patch);
        }
        config
    }

    /// The `plugins` layer `record` implies (§3): a row for each name that
    /// uses a default or installed plugin with code, and each built-in row
    /// whose name is off or uses another plugin turned off; with those
    /// built-in names.
    fn derive(&self, record: &Record) -> (Vec<Patch>, Vec<String>) {
        let builtins = self.builtins.get().expect("set at startup");
        let catalog = self.catalog.get().expect("set at startup");
        let mut rows = Vec::new();
        let mut patches = Vec::new();
        let mut off = Vec::new();
        for name in names(record, &self.defaults, builtins) {
            let used = using(record, &self.defaults, builtins, &name);
            if builtins.contains(&name) && used != Some(Use::Builtin) {
                patches.push(Patch {
                    id: Some(name.clone()),
                    overrides: [("disabled".to_owned(), Value::Bool(true))]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                });
                off.push(name.clone());
            }
            let Some(dir) = used
                .as_ref()
                .and_then(|used| dir_of(record, &self.defaults, &name, used))
            else {
                continue;
            };
            match Manifest::read(&dir) {
                Ok(Manifest {
                    code: Some(code), ..
                }) if missing_instance_names(catalog, &code).is_empty() => {
                    let used = used.as_ref().expect("a plugin in use");
                    self.provide_data_dir(&name, used);
                    let id = own_id(&name, used).expect("not a built-in");
                    if is_python(&code)
                        && let Err(error) = self.dirs.python_link(&python_package(id), &dir)
                    {
                        tracing::warn!(%name, %error, "Python plugin link refused");
                    }
                    let config = self.config_of(&name, &code.config);
                    rows.push(row_value(&name, id, &dir, &code, config));
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%name, %error, "plugin manifest unreadable"),
            }
        }
        patches.insert(
            0,
            Patch {
                insert: Some(rows),
                ..Default::default()
            },
        );
        (patches, off)
    }
}
impl State {
    /// Startup (§3.1), last part, once the process rows are up: brings every
    /// content owner to the record.
    pub(crate) async fn startup_content(&self, owners: &Owners) {
        self.reconcile_content(owners, &CONTENT).await;
    }

    /// Brings what the present owners hold of `kinds` to the record (§3.7):
    /// registers the content of every plugin in use and withdraws it for
    /// every other plugin id an owner holds. Turning an owner's name on calls
    /// it for that owner's kind.
    async fn reconcile_content(&self, owners: &Owners, kinds: &[Kind]) {
        let record = self.record.lock().unwrap().clone();
        let builtins = self.builtins.get().expect("set at startup");
        let mut in_use = BTreeSet::new();
        for name in names(&record, &self.defaults, builtins) {
            let Some(used @ (Use::Default | Use::Installed(_))) =
                using(&record, &self.defaults, builtins, &name)
            else {
                continue;
            };
            let Some(dir) = dir_of(&record, &self.defaults, &name, &used) else {
                continue;
            };
            let plugin = own_id(&name, &used).expect("not a built-in").to_owned();
            match Manifest::read(&dir) {
                Ok(manifest) => {
                    owners
                        .register(&plugin, &dir, &manifest, kinds, &mut Outcome::default())
                        .await;
                }
                Err(error) => tracing::warn!(%name, %error, "plugin manifest unreadable"),
            }
            in_use.insert(plugin);
        }
        for plugin in owners.held(kinds).await {
            if !in_use.contains(&plugin) {
                owners.withdraw(&plugin, kinds).await;
            }
        }
    }

    /// A plugin with code that is turned on gets `<pluginFiles root>/<id>/`
    /// (§2.1): its installed id, or a default plugin's name. It is created
    /// if missing and never deleted. An existing entry is used only if it
    /// is a directory, not a symbolic link, inside the root.
    fn provide_data_dir(&self, name: &str, used: &Use) {
        let (Some(root), Some(id)) = (&self.files, own_id(name, used)) else {
            return;
        };
        if let Err(error) = acquire::check_id(id) {
            tracing::warn!(%name, %error, "no plugin data directory");
            return;
        }
        let dir = root.join(id);
        let result = match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                match std::fs::canonicalize(&dir) {
                    Ok(resolved) if resolved == dir => Ok(()),
                    _ => Err("it leaves the plugin files root".to_owned()),
                }
            }
            Ok(_) => Err("it exists and is not a directory".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private_dir(&dir).map_err(|error| error.to_string())
            }
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = result {
            tracing::warn!(dir = %dir.display(), %error, "plugin data directory refused");
        }
    }
}

/// Creates one directory (not its parents), owner-only on Unix.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn register_names(catalog: &mut ServiceCatalog, code: &record::Code) {
    for (service, group) in &code.instance_names {
        if matches!(
            catalog.key_in(service, &Build::default()),
            Err(LoaderError::UnknownService(_))
        ) {
            catalog.register_shared_instance(service, group.as_str().unwrap_or_default());
        }
    }
    for service in code.provides.keys() {
        if let Err(LoaderError::UnknownService(_)) = catalog.key_in(service, &Build::default()) {
            catalog.register_shared(service);
        }
    }
}

/// `instanceNames` the running catalog does not hold in their group.
fn missing_instance_names(catalog: &ServiceCatalog, code: &record::Code) -> Vec<String> {
    code.instance_names
        .iter()
        .filter(|(service, group)| {
            !matches!(
                catalog.key_in(service, &Build::default()),
                Err(LoaderError::OutsideInstance { group: held, .. }) if Some(held.as_str()) == group.as_str()
            )
        })
        .map(|(service, _)| service.clone())
        .collect()
}

/// Deep-merges `patch` into `value`: objects merge member by member, and
/// every other value is replaced.
fn merge(value: &mut Value, patch: &Value) {
    match (value, patch) {
        (Value::Object(value), Value::Object(patch)) => {
            for (key, member) in patch {
                match value.get_mut(key) {
                    Some(existing) => merge(existing, member),
                    None => {
                        value.insert(key.clone(), member.clone());
                    }
                }
            }
        }
        (value, patch) => *value = patch.clone(),
    }
}

fn row_value(name: &str, id: &str, dir: &Path, code: &record::Code, config: Value) -> Value {
    json!({
        "id": row_id(name),
        "name": module(id, dir, code),
        "inject": code.inject,
        "config": config,
    })
}

/// Every name PluginManager knows: built-in, default, installed, chosen.
fn names(record: &Record, defaults: &Defaults, builtins: &BTreeSet<String>) -> BTreeSet<String> {
    builtins
        .iter()
        .chain(defaults.plugins.keys())
        .chain(record.installed.iter().map(|entry| &entry.name))
        .chain(record.names.keys())
        .cloned()
        .collect()
}

/// The plugin name shipped with: its built-in, else its default plugin.
fn shipped(defaults: &Defaults, builtins: &BTreeSet<String>, name: &str) -> Option<Use> {
    if builtins.contains(name) {
        Some(Use::Builtin)
    } else if defaults.plugins.contains_key(name) {
        Some(Use::Default)
    } else {
        None
    }
}

/// The plugin `name` uses; `None` when it is off or uses nothing. A locked
/// name always uses what it was shipped with.
fn using(
    record: &Record,
    defaults: &Defaults,
    builtins: &BTreeSet<String>,
    name: &str,
) -> Option<Use> {
    if defaults.locked.iter().any(|locked| locked == name) {
        return shipped(defaults, builtins, name);
    }
    let choice = record.names.get(name).cloned().unwrap_or_default();
    if choice.off {
        return None;
    }
    match choice.use_.as_deref() {
        None => shipped(defaults, builtins, name),
        Some("builtin") => builtins.contains(name).then_some(Use::Builtin),
        Some("default") => defaults.plugins.contains_key(name).then_some(Use::Default),
        Some(id) => record
            .installed(id)
            .filter(|entry| entry.name == name)
            .map(|_| Use::Installed(id.to_owned())),
    }
}

fn dir_of(record: &Record, defaults: &Defaults, name: &str, used: &Use) -> Option<PathBuf> {
    match used {
        Use::Builtin => None,
        Use::Default => defaults.plugins.get(name).cloned(),
        Use::Installed(id) => record.installed(id).map(|entry| entry.dir.clone()),
    }
}

/// What a command's loader and owner steps came to (A.1 results).
#[derive(Default)]
struct Outcome {
    status: Option<Value>,
    skipped: Vec<Kind>,
    /// Content an owner refused: `{kind, error}`.
    refused: Vec<Value>,
    restart_required: bool,
}
impl Outcome {
    fn into_map(self) -> Map<String, Value> {
        let mut map = self
            .status
            .and_then(|s| s.as_object().cloned())
            .unwrap_or_default();
        if !self.skipped.is_empty() {
            map.insert("skipped".into(), json!(self.skipped));
        }
        if !self.refused.is_empty() {
            map.insert("refused".into(), json!(self.refused));
        }
        if self.restart_required {
            map.insert("restartRequired".into(), json!(true));
        }
        map
    }
}

/// What a name used before and after a change of the record.
struct Change {
    before: Option<Use>,
    after: Option<Use>,
}

/// The operations of one PluginManager generation.
#[derive(Clone)]
pub(crate) struct Manager {
    state: Arc<State>,
    loader: Arc<Loader>,
    ctx: Ctx,
    runtimes: Key<crate::runtime_manager::RuntimeService>,
    owners: Owners,
    settings: Key<SettingsService>,
}

/// The content owners, read only if present (§1): an absent owner skips
/// only its kind of content.
#[derive(Clone)]
pub(crate) struct Owners {
    ctx: Ctx,
    skills: Key<SkillsCatalog>,
    mcp: Key<McpService>,
}

/// The row names of the content owners and the kind each takes.
const OWNERS: [(&str, Kind); 2] = [("SkillsCatalog", Kind::Skills), ("Mcp", Kind::Mcp)];

fn refused(kind: Kind, error: impl std::fmt::Display) -> Value {
    json!({"kind": kind, "error": error.to_string()})
}

fn skill_issues(issues: Vec<SkillIssue>) -> Option<String> {
    let issues: Vec<String> = issues
        .into_iter()
        .map(|issue| match issue {
            SkillIssue::InvalidManifest { path, reason }
            | SkillIssue::Unreadable { path, reason } => format!("{}: {reason}", path.display()),
            SkillIssue::RootUnreadable { root, reason } => {
                format!("{}: {reason}", root.display())
            }
            SkillIssue::ScanLimitReached { root } => {
                format!("{}: Skill scan limit reached", root.display())
            }
        })
        .collect();
    (!issues.is_empty()).then(|| issues.join("; "))
}

impl Owners {
    /// The owners as `ctx` reads them, keyed from `names`.
    pub fn new(ctx: Ctx, names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            ctx,
            skills: names.key()?,
            mcp: names.key()?,
        })
    }

    /// The plugin ids the present owners hold content of `kinds` for.
    async fn held(&self, kinds: &[Kind]) -> BTreeSet<String> {
        let mut held = BTreeSet::new();
        if kinds.contains(&Kind::Skills)
            && let Some(catalog) = self.ctx.get_at(&self.skills)
        {
            match catalog.plugins().await {
                Ok(plugins) => held.extend(plugins),
                Err(error) => tracing::warn!(%error, "Skills plugin roots unreadable"),
            }
        }
        if kinds.contains(&Kind::Mcp)
            && let Some(mcp) = self.ctx.get_at(&self.mcp)
        {
            match mcp.plugins(&self.ctx.cancellation_token()).await {
                Ok(plugins) => held.extend(plugins),
                Err(error) => tracing::warn!(%error, "MCP plugin documents unreadable"),
            }
        }
        held
    }

    /// Registers each content kind `manifest` carries with its owner, and
    /// withdraws the kinds it does not carry (§3.7). `kinds` limits the
    /// owners called.
    async fn register(
        &self,
        plugin: &str,
        dir: &Path,
        manifest: &Manifest,
        kinds: &[Kind],
        outcome: &mut Outcome,
    ) {
        let cancel = self.ctx.cancellation_token();
        if kinds.contains(&Kind::Skills) {
            match (self.ctx.get_at(&self.skills), &manifest.skills) {
                (None, Some(_)) => outcome.skipped.push(Kind::Skills),
                (None, None) => {}
                (Some(catalog), Some(root)) => {
                    match catalog.register_plugin(plugin, root.clone()).await {
                        Ok(issues) => {
                            if let Some(error) = skill_issues(issues) {
                                outcome.refused.push(refused(Kind::Skills, error));
                            }
                        }
                        Err(error) => outcome.refused.push(refused(Kind::Skills, error)),
                    }
                }
                (Some(catalog), None) => {
                    if let Err(error) = catalog.withdraw_plugin(plugin).await {
                        tracing::warn!(%plugin, %error, "Skills withdrawal refused");
                    }
                }
            }
        }
        if kinds.contains(&Kind::Mcp) {
            match (self.ctx.get_at(&self.mcp), &manifest.mcp) {
                (None, Some(_)) => outcome.skipped.push(Kind::Mcp),
                (None, None) => {}
                (Some(mcp), Some(path)) => {
                    let document = std::fs::read(path)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            serde_json::from_slice::<Value>(&bytes)
                                .map_err(|error| error.to_string())
                        });
                    let result = match document {
                        Ok(document) => mcp
                            .register_plugin(plugin.into(), dir.into(), document, &cancel)
                            .await
                            .map_err(|error| error.to_string()),
                        Err(error) => {
                            let _ = mcp.withdraw_plugin(plugin.into(), &cancel).await;
                            Err(format!("{}: {error}", path.display()))
                        }
                    };
                    if let Err(error) = result {
                        outcome.refused.push(refused(Kind::Mcp, error));
                    }
                }
                (Some(mcp), None) => {
                    if let Err(error) = mcp.withdraw_plugin(plugin.into(), &cancel).await {
                        tracing::warn!(%plugin, %error, "MCP withdrawal refused");
                    }
                }
            }
        }
    }

    /// Withdraws `plugin`'s content of `kinds` from each present owner.
    async fn withdraw(&self, plugin: &str, kinds: &[Kind]) {
        let cancel = self.ctx.cancellation_token();
        if kinds.contains(&Kind::Skills)
            && let Some(catalog) = self.ctx.get_at(&self.skills)
            && let Err(error) = catalog.withdraw_plugin(plugin).await
        {
            tracing::warn!(%plugin, %error, "Skills withdrawal refused");
        }
        if kinds.contains(&Kind::Mcp)
            && let Some(mcp) = self.ctx.get_at(&self.mcp)
            && let Err(error) = mcp.withdraw_plugin(plugin.into(), &cancel).await
        {
            tracing::warn!(%plugin, %error, "MCP withdrawal refused");
        }
    }

    /// What the present owners hold refused for `plugin` now.
    async fn refused(&self, plugin: &str, manifest: &Manifest) -> Vec<Value> {
        let mut found = Vec::new();
        if manifest.skills.is_some()
            && let Some(catalog) = self.ctx.get_at(&self.skills)
        {
            match catalog.plugin_issues(plugin).await {
                Ok(issues) => found.extend(skill_issues(issues).map(|e| refused(Kind::Skills, e))),
                Err(error) => found.push(refused(Kind::Skills, error)),
            }
        }
        if manifest.mcp.is_some()
            && let Some(error) = self
                .ctx
                .get_at(&self.mcp)
                .and_then(|mcp| mcp.plugin_refusal(plugin))
        {
            found.push(refused(Kind::Mcp, error));
        }
        found
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallParams {
    source: SourceParams,
    id: Option<String>,
    enable: Option<bool>,
}
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum SourceParams {
    Path {
        path: PathBuf,
    },
    Git {
        git: String,
        #[serde(rename = "ref")]
        reference: Option<String>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameParams {
    name: String,
    #[serde(rename = "use")]
    use_: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdParams {
    id: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn decode<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, PluginError> {
    serde_json::from_value(params)
        .map_err(|error| PluginError::new("invalid_params", error.to_string()))
}

/// The methods of the `plugin` namespace (A.1), shared with the tools.
pub(crate) const METHODS: [&str; 7] = [
    "plugin.list",
    "plugin.install",
    "plugin.enable",
    "plugin.disable",
    "plugin.reload",
    "plugin.update",
    "plugin.remove",
];

impl Manager {
    fn builtins(&self) -> &BTreeSet<String> {
        self.state
            .builtins
            .get()
            .expect("built-in rows are set before the loader starts")
    }

    fn locked(&self, name: &str) -> bool {
        self.state
            .defaults
            .locked
            .iter()
            .any(|locked| locked == name)
    }

    fn using(&self, record: &Record, name: &str) -> Option<Use> {
        using(record, &self.state.defaults, self.builtins(), name)
    }

    fn shipped(&self, name: &str) -> Option<Use> {
        shipped(&self.state.defaults, self.builtins(), name)
    }

    /// Changes the in-memory record with `change`, writes it, and returns
    /// `change`'s value and what the name uses before and afterwards. A
    /// failed write leaves the table as it was.
    fn commit<T>(
        &self,
        name: impl Fn(&Record, &T) -> String,
        change: impl FnOnce(&mut Record) -> Result<T, PluginError>,
    ) -> Result<(T, Change), PluginError> {
        let mut record = self.state.record.lock().unwrap();
        let mut next = record.clone();
        let value = change(&mut next)?;
        let name = name(&next, &value);
        let change = Change {
            before: self.using(&record, &name),
            after: self.using(&next, &name),
        };
        next.write(&self.state.record_path).map_err(|error| {
            PluginError::new("acquire_failed", format!("plugins.json: {error}"))
        })?;
        *record = next;
        Ok((value, change))
    }

    pub async fn call(
        &self,
        method: &str,
        params: Value,
        progress: Option<&Progress>,
    ) -> Result<Value, PluginError> {
        let phase = |phase: &str| {
            if let Some(progress) = progress {
                progress.send(json!({"phase": phase}));
            }
        };
        match method {
            "plugin.list" => {
                let Empty {} = decode(params)?;
                Ok(self.list().await)
            }
            "plugin.install" => self.install(decode(params)?, &phase).await,
            "plugin.enable" => self.enable(decode(params)?).await,
            "plugin.disable" => {
                let NameParams { name, use_: None } = decode(params)? else {
                    return Err(PluginError::new("invalid_params", "disable takes no use"));
                };
                self.disable(&name).await
            }
            "plugin.reload" => {
                let NameParams { name, use_: None } = decode(params)? else {
                    return Err(PluginError::new("invalid_params", "reload takes no use"));
                };
                self.reload(&name).await
            }
            "plugin.update" => self.update(decode(params)?, &phase).await,
            "plugin.remove" => {
                let IdParams {
                    id,
                    reference: None,
                } = decode(params)?
                else {
                    return Err(PluginError::new("invalid_params", "remove takes no ref"));
                };
                self.remove(&id).await
            }
            _ => Err(PluginError::new("unknown_method", method.to_owned())),
        }
    }

    /// `plugin.install` (§3.2).
    async fn install(
        &self,
        params: InstallParams,
        phase: &(dyn Fn(&str) + Sync),
    ) -> Result<Value, PluginError> {
        let enable = params.enable.unwrap_or(true);
        if let Some(id) = &params.id {
            acquire::check_id(id)?;
            self.refuse_installed(id)?;
        }
        phase("fetch");
        let mut placed: Option<PathBuf> = None;
        let result = async {
            let (id, manifest, source, dir) = match params.source {
                SourceParams::Path { path } => {
                    let dir = std::fs::canonicalize(&path).map_err(|error| {
                        PluginError::new("invalid_source", format!("{}: {error}", path.display()))
                    })?;
                    phase("check");
                    let manifest = Manifest::read(&dir)?;
                    let id = self.new_id(params.id.as_deref(), &manifest)?;
                    (id, manifest, Source::Path { path: dir.clone() }, dir)
                }
                SourceParams::Git { git, reference } => {
                    acquire::check_git_url(&git)?;
                    let staging = self.state.dirs.stage()?;
                    let commit = acquire::clone(&git, reference.as_deref(), &staging).await?;
                    phase("check");
                    let manifest = acquire::manifest_of(&staging)?;
                    let id = self.new_id(params.id.as_deref(), &manifest)?;
                    let target = self.state.dirs.clone_dir(&id, &commit)?;
                    let dir = staging.place(&self.state.dirs, &target)?;
                    placed = Some(dir.clone());
                    let manifest = Manifest::read(&dir)?;
                    let source = Source::Git {
                        git,
                        reference,
                        commit,
                    };
                    (id, manifest, source, dir)
                }
            };
            let entry = Installed {
                id: id.clone(),
                name: manifest.name.clone(),
                source,
                dir,
                kinds: manifest.kinds(),
            };
            let locked = self.locked(&manifest.name);
            let turn_on = enable && !locked;
            let ((), change) = self.commit(
                |_, ()| manifest.name.clone(),
                |record| {
                    if record.installed(&id).is_some() {
                        return Err(PluginError::new(
                            "plugin_exists",
                            format!("{id} is installed"),
                        ));
                    }
                    record.installed.push(entry);
                    if turn_on {
                        record.names.insert(
                            manifest.name.clone(),
                            Choice {
                                use_: Some(id.clone()),
                                off: false,
                            },
                        );
                    }
                    Ok(())
                },
            )?;
            Ok::<_, PluginError>((id, manifest.name, locked, change))
        }
        .await;
        let (id, name, locked, change) = match result {
            Ok(done) => done,
            Err(error) => {
                if let Some(dir) = placed {
                    let _ = self.state.dirs.delete(&dir);
                    if let Some(parent) = dir.parent()
                        && std::fs::read_dir(parent).is_ok_and(|mut d| d.next().is_none())
                    {
                        let _ = self.state.dirs.delete(parent);
                    }
                }
                return Err(error);
            }
        };
        phase("load");
        let enabled = change.after == Some(Use::Installed(id.clone()));
        let outcome = self.switch(&name, change).await;
        let mut reply = outcome.into_map();
        reply.insert("id".into(), json!(id));
        reply.insert("name".into(), json!(name));
        reply.insert("enabled".into(), json!(enabled));
        if locked {
            reply.insert("locked".into(), json!(true));
        }
        Ok(Value::Object(reply))
    }

    fn refuse_installed(&self, id: &str) -> Result<(), PluginError> {
        if self.state.record.lock().unwrap().installed(id).is_some() {
            return Err(PluginError::new(
                "plugin_exists",
                format!("{id} is installed"),
            ));
        }
        Ok(())
    }

    fn new_id(&self, given: Option<&str>, manifest: &Manifest) -> Result<String, PluginError> {
        match given {
            Some(id) => Ok(id.to_owned()),
            None => {
                let id = manifest.name.to_lowercase();
                acquire::check_id(&id)?;
                self.refuse_installed(&id)?;
                Ok(id)
            }
        }
    }

    /// Whether `name` is a name PluginManager knows.
    fn known(&self, record: &Record, name: &str) -> bool {
        self.shipped(name).is_some() || record.installed.iter().any(|entry| entry.name == name)
    }

    /// `plugin.enable` (§3.3).
    async fn enable(&self, params: NameParams) -> Result<Value, PluginError> {
        let NameParams { name, use_ } = params;
        if self.locked(&name) {
            let current = self.shipped(&name).map(|used| used.wire().to_owned());
            if use_.is_some() && use_ != current {
                return Err(PluginError::new(
                    "plugin_locked",
                    format!("{name} is locked"),
                ));
            }
            return Ok(Value::Object(
                self.status_of(&name, self.shipped(&name).as_ref())
                    .into_map(),
            ));
        }
        let ((), change) = self.commit(
            |_, ()| name.clone(),
            |record| {
                if !self.known(record, &name) {
                    return Err(not_found(&name));
                }
                let choice = record.names.entry(name.clone()).or_default();
                if let Some(use_) = &use_ {
                    choice.use_ = Some(use_.clone());
                }
                choice.off = false;
                let choice = choice.clone();
                let valid = match choice.use_.as_deref() {
                    None => self.shipped(&name).is_some(),
                    Some("builtin") => self.builtins().contains(&name),
                    Some("default") => self.state.defaults.plugins.contains_key(&name),
                    Some(id) => record.installed(id).is_some_and(|entry| entry.name == name),
                };
                if !valid {
                    return Err(match &choice.use_ {
                        Some(use_) => not_found(use_),
                        None => PluginError::new(
                            "plugin_not_found",
                            format!("{name} has no plugin to use: give one with use"),
                        ),
                    });
                }
                if choice == Choice::default() {
                    record.names.remove(&name);
                }
                Ok(())
            },
        )?;
        Ok(Value::Object(self.switch(&name, change).await.into_map()))
    }

    /// `plugin.disable` (§3.3).
    async fn disable(&self, name: &str) -> Result<Value, PluginError> {
        if self.locked(name) {
            return Err(PluginError::new(
                "plugin_locked",
                format!("{name} is locked"),
            ));
        }
        let ((), change) = self.commit(
            |_, ()| name.to_owned(),
            |record| {
                if !self.known(record, name) {
                    return Err(not_found(name));
                }
                record.names.entry(name.to_owned()).or_default().off = true;
                Ok(())
            },
        )?;
        self.switch(name, change).await;
        Ok(json!({}))
    }

    /// `plugin.remove` (§3.6).
    async fn remove(&self, id: &str) -> Result<Value, PluginError> {
        if id == "builtin" || id == "default" {
            return Err(PluginError::new(
                "not_removable",
                "built-in and default plugins can be turned off, not removed",
            ));
        }
        let (entry, change) = self.commit(
            |_, entry: &Installed| entry.name.clone(),
            |record| {
                let index = record
                    .installed
                    .iter()
                    .position(|entry| entry.id == id)
                    .ok_or_else(|| not_found(id))?;
                let entry = record.installed.remove(index);
                if let Some(choice) = record.names.get_mut(&entry.name)
                    && choice.use_.as_deref() == Some(id)
                {
                    choice.use_ = None;
                    if self.shipped(&entry.name).is_none() || *choice == Choice::default() {
                        record.names.remove(&entry.name);
                    }
                }
                Ok(entry)
            },
        )?;
        self.switch(&entry.name, change).await;
        self.state
            .dirs
            .remove_python_link(&python_package(&entry.id));
        if let Source::Git { .. } = entry.source {
            self.state
                .dirs
                .delete(&self.state.dirs.id_dir(&entry.id)?)?;
        }
        Ok(json!({}))
    }

    /// `plugin.reload` (§3.4).
    async fn reload(&self, name: &str) -> Result<Value, PluginError> {
        let used = {
            let record = self.state.record.lock().unwrap();
            if !self.known(&record, name) {
                return Err(not_found(name));
            }
            self.using(&record, name)
        };
        let dir = match &used {
            None => {
                return Err(PluginError::new(
                    "plugin_not_found",
                    format!("{name} is off"),
                ));
            }
            Some(Use::Builtin) => {
                return Err(PluginError::new(
                    "not_reloadable",
                    format!("{name} is built in: it needs a rebuild; replace it instead"),
                ));
            }
            Some(used) => {
                let record = self.state.record.lock().unwrap();
                dir_of(&record, &self.state.defaults, name, used)
                    .expect("a used plugin has a directory")
            }
        };
        let manifest = Manifest::read(&dir)?;
        if manifest.name != name {
            return Err(PluginError::new(
                "name_changed",
                format!(
                    "the manifest now names {:?}; install it again",
                    manifest.name
                ),
            ));
        }
        if let Some(Use::Installed(id)) = &used {
            let kinds = manifest.kinds();
            let id = id.clone();
            self.commit(
                |_, ()| name.to_owned(),
                |record| {
                    if let Some(entry) = record.installed.iter_mut().find(|entry| entry.id == id) {
                        entry.kinds = kinds;
                    }
                    Ok(())
                },
            )?;
        }
        let mut outcome = Outcome::default();
        let plugin = own_id(name, used.as_ref().expect("in use")).expect("not a built-in");
        self.owners
            .register(plugin, &dir, &manifest, &CONTENT, &mut outcome)
            .await;
        let row = row_id(name);
        match &manifest.code {
            None => self.apply().await,
            Some(code)
                if !missing_instance_names(
                    self.state.catalog.get().expect("set at startup"),
                    code,
                )
                .is_empty() =>
            {
                outcome.restart_required = true;
            }
            Some(code) => {
                let current = self.loader.get(&row);
                let module = module(
                    own_id(name, used.as_ref().expect("in use")).expect("not a built-in"),
                    &dir,
                    code,
                );
                let same = current.as_ref().is_some_and(|entry| {
                    entry.options["name"] == json!(module)
                        && entry.options.get("inject").cloned().unwrap_or(json!([]))
                            == json!(code.inject)
                        && entry.options.get("config")
                            == Some(&self.state.config_of(name, &code.config))
                });
                if same {
                    if let Some(Some(node)) = self.state.node.get() {
                        node.invalidate(&module);
                    }
                    if let Err(error) = self.loader.reload(&row).await {
                        tracing::warn!(%row, %error, "plugin reload refused");
                    }
                } else {
                    self.apply().await;
                }
            }
        }
        outcome.status = Some(self.status(&row));
        Ok(Value::Object(outcome.into_map()))
    }

    /// `plugin.update` (§3.5).
    async fn update(
        &self,
        params: IdParams,
        phase: &(dyn Fn(&str) + Sync),
    ) -> Result<Value, PluginError> {
        let IdParams { id, reference } = params;
        let entry = self
            .state
            .record
            .lock()
            .unwrap()
            .installed(&id)
            .cloned()
            .ok_or_else(|| not_found(&id))?;
        let Source::Git {
            git,
            reference: recorded,
            commit: old,
        } = &entry.source
        else {
            return Err(PluginError::new(
                "not_updatable",
                format!("{id} is a local directory: reload it"),
            ));
        };
        let reference = reference.or_else(|| recorded.clone());
        phase("fetch");
        let staging = self.state.dirs.stage()?;
        let commit = acquire::clone(git, reference.as_deref(), &staging).await?;
        if &commit == old {
            if &reference != recorded {
                self.commit(
                    |_, ()| entry.name.clone(),
                    |record| {
                        if let Some(Installed {
                            source:
                                Source::Git {
                                    reference: stored, ..
                                },
                            ..
                        }) = record.installed.iter_mut().find(|current| current.id == id)
                        {
                            stored.clone_from(&reference);
                        }
                        Ok(())
                    },
                )?;
            }
            return Ok(json!({"commit": commit}));
        }
        phase("check");
        let manifest = acquire::manifest_of(&staging)?;
        if manifest.name != entry.name {
            return Err(PluginError::new(
                "name_changed",
                format!("the new commit names {:?}; install it again", manifest.name),
            ));
        }
        let target = self.state.dirs.clone_dir(&id, &commit)?;
        let dir = staging.place(&self.state.dirs, &target)?;
        let manifest = match Manifest::read(&dir) {
            Ok(manifest) => manifest,
            Err(error) => {
                let _ = self.state.dirs.delete(&dir);
                return Err(error);
            }
        };
        let committed = self.commit(
            |_, ()| entry.name.clone(),
            |record| {
                let current = record
                    .installed
                    .iter_mut()
                    .find(|current| current.id == id)
                    .ok_or_else(|| not_found(&id))?;
                current.source = Source::Git {
                    git: git.clone(),
                    reference: reference.clone(),
                    commit: commit.clone(),
                };
                current.dir = dir.clone();
                current.kinds = manifest.kinds();
                Ok(())
            },
        );
        let ((), change) = match committed {
            Ok(done) => done,
            Err(error) => {
                let _ = self.state.dirs.delete(&dir);
                return Err(error);
            }
        };
        phase("load");
        let mut reply = Map::new();
        if change.after == Some(Use::Installed(id.clone())) {
            reply = self.switch(&entry.name, change).await.into_map();
        }
        let _ = self.state.dirs.delete(&entry.dir);
        reply.insert("commit".into(), json!(commit));
        Ok(Value::Object(reply))
    }

    /// Brings the loader to the record (§3): derives the `plugins` layer
    /// from `plugins.json` and reconciles it. A row rutis refuses stays in
    /// the loader with its reason, as after a restart.
    async fn apply(&self) {
        let _applying = self.state.applying.lock().await;
        let record = self.state.record.lock().unwrap().clone();
        let (patches, _) = self.state.derive(&record);
        let mut layers = self.loader.layers();
        match layers.iter_mut().find(|layer| layer.name == LAYER) {
            Some(layer) => layer.patches = patches,
            None => layers.push(rutis_loader::Layer::new(LAYER, patches)),
        }
        if let Err(error) = self.loader.reconcile(layers, None).await {
            tracing::warn!(%error, "plugin layer reconcile refused");
        }
    }

    /// Applies the record, moves the name's content from the plugin it used
    /// to the one it uses now (§3.7), and says what that came to: its row's
    /// status, the content kinds skipped or refused, `restartRequired`. A
    /// content owner's name turned on gets that kind of content of every
    /// plugin.
    async fn switch(&self, name: &str, change: Change) -> Outcome {
        self.apply().await;
        let before = change.before.as_ref().and_then(|used| own_id(name, used));
        let after = change.after.as_ref().and_then(|used| own_id(name, used));
        if let Some(before) = before
            && Some(before) != after
        {
            self.owners.withdraw(before, &CONTENT).await;
        }
        if change.after.is_some()
            && let Some((_, kind)) = OWNERS.iter().find(|(owner, _)| *owner == name)
        {
            self.state.reconcile_content(&self.owners, &[*kind]).await;
        }
        let used = change.after.as_ref();
        let Some(used @ (Use::Default | Use::Installed(_))) = used else {
            return self.status_of(name, used);
        };
        let dir = {
            let record = self.state.record.lock().unwrap();
            dir_of(&record, &self.state.defaults, name, used)
        };
        let manifest = match dir.as_deref().map(Manifest::read) {
            Some(Ok(manifest)) => manifest,
            Some(Err(error)) => {
                return Outcome {
                    status: Some(json!({"status": "failed", "error": error.to_string()})),
                    ..Default::default()
                };
            }
            None => return Outcome::default(),
        };
        let mut outcome = Outcome::default();
        let plugin = own_id(name, used).expect("not a built-in");
        let dir = dir.expect("a used plugin has a directory");
        self.owners
            .register(plugin, &dir, &manifest, &CONTENT, &mut outcome)
            .await;
        if let Some(code) = &manifest.code {
            // RuntimeManager R1: a Python plugin turned on asks for the Python runtime.
            if is_python(code)
                && let Some(runtimes) = self.ctx.get_at(&self.runtimes)
            {
                runtimes.install(crate::runtime_manager::Kind::Python);
            }
            if missing_instance_names(self.state.catalog.get().expect("set at startup"), code)
                .is_empty()
            {
                outcome.status = Some(self.status(&row_id(name)));
            } else {
                outcome.restart_required = true;
            }
        }
        outcome
    }

    fn status_of(&self, name: &str, used: Option<&Use>) -> Outcome {
        let row = match used {
            Some(Use::Builtin) => name.to_owned(),
            Some(_) => row_id(name),
            None => return Outcome::default(),
        };
        Outcome {
            status: Some(self.status(&row)),
            ..Default::default()
        }
    }

    /// A row's status as A.1 shows it, read from the loader now.
    fn status(&self, row: &str) -> Value {
        let Some(entry) = self.loader.get(row) else {
            return json!({"status": "failed", "error": "the row is not loaded"});
        };
        status_value(&self.loader, &entry)
    }

    /// Settings, if present, and the declared sources the plugin `name`
    /// uses registered (§6 `config`, §9 R4); none when the name is off or
    /// its row is not running.
    pub(crate) fn sources(
        &self,
        name: &str,
    ) -> Result<(Option<Arc<SettingsService>>, Vec<SourceDeclaration>), PluginError> {
        let record = self.state.record.lock().unwrap().clone();
        if !names(&record, &self.state.defaults, self.builtins()).contains(name) {
            return Err(not_found(name));
        }
        let row = match self.using(&record, name) {
            None => return Ok((None, Vec::new())),
            Some(Use::Builtin) => name.to_owned(),
            Some(_) => row_id(name),
        };
        let Some(settings) = self.ctx.get_at(&self.settings) else {
            return Ok((None, Vec::new()));
        };
        let sources = self
            .loader
            .get(&row)
            .and_then(|entry| entry.view)
            .map(|view| settings.declared_by(view.instance()))
            .unwrap_or_default();
        Ok((Some(settings), sources))
    }

    /// `plugin.list` (§3.6).
    async fn list(&self) -> Value {
        let record = self.state.record.lock().unwrap().clone();
        let mut items = Vec::new();
        for name in names(&record, &self.state.defaults, self.builtins()) {
            items.push({
                let choice = record.names.get(&name).cloned().unwrap_or_default();
                let used = self.using(&record, &name);
                let mut candidates = Vec::new();
                if self.builtins().contains(&name) {
                    candidates.push(json!({"id": "builtin", "kinds": ["code"]}));
                }
                if let Some(dir) = self.state.defaults.plugins.get(&name) {
                    let kinds = Manifest::read(dir).map(|m| m.kinds()).unwrap_or_default();
                    candidates.push(json!({"id": "default", "kinds": kinds}));
                }
                for entry in record.installed.iter().filter(|entry| entry.name == name) {
                    candidates.push(
                        json!({"id": entry.id, "source": entry.source, "kinds": entry.kinds}),
                    );
                }
                let mut item = json!({
                    "name": name,
                    "use": used.as_ref().map(Use::wire),
                    "off": choice.off && !self.locked(&name),
                    "locked": self.locked(&name),
                    "candidates": candidates,
                });
                let fields = item.as_object_mut().unwrap();
                match &used {
                    None if choice.off => {
                        fields.insert("status".into(), json!("off"));
                    }
                    None => {}
                    Some(Use::Builtin) => fields.extend(object(self.status(&name))),
                    Some(used) => {
                        match dir_of(&record, &self.state.defaults, &name, used)
                            .map(|dir| Manifest::read(&dir))
                        {
                            Some(Ok(manifest)) => {
                                if manifest.code.is_some() {
                                    fields.extend(object(self.status(&row_id(&name))));
                                }
                                let plugin = own_id(&name, used).expect("not a built-in");
                                let refused = self.owners.refused(plugin, &manifest).await;
                                if !refused.is_empty() {
                                    fields.insert("refused".into(), json!(refused));
                                }
                            }
                            Some(Err(error)) => {
                                fields.insert("status".into(), json!("failed"));
                                fields.insert("error".into(), json!(error.to_string()));
                            }
                            None => {}
                        }
                    }
                }
                item
            });
        }
        Value::Array(items)
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// The content kinds PluginManager registers with owners. Hooks is M2.
const CONTENT: [Kind; 2] = [Kind::Skills, Kind::Mcp];

fn status_value(loader: &Loader, entry: &EntryInfo) -> Value {
    let pending = || json!({"status": "pending", "waitingFor": loader.waiting_for(entry)});
    match &entry.status {
        EntryStatus::Disabled => json!({"status": "off"}),
        EntryStatus::Inactive => pending(),
        EntryStatus::Unresolved(error) => json!({"status": "failed", "error": error.to_string()}),
        EntryStatus::Running(snapshot) => match snapshot.state {
            rutis::host::FiberState::Active => json!({"status": "running"}),
            rutis::host::FiberState::Failed => json!({
                "status": "failed",
                "error": snapshot.error.as_ref().map(|e| e.to_string()),
            }),
            _ => pending(),
        },
        EntryStatus::Stopped => json!({"status": "stopped"}),
        _ => json!({"status": "unknown"}),
    }
}

/// PluginManager's process row (§1): registers the `plugin` namespace and
/// the `PluginTools` Session part. It is locked.
pub(crate) struct PluginManagerPlugin {
    state: Arc<State>,
    loader: Key<Loader>,
    methods: Key<ManagementMethods>,
    parts: Key<PartRegistry>,
    skills: Key<SkillsCatalog>,
    mcp: Key<McpService>,
    runtimes: Key<crate::runtime_manager::RuntimeService>,
    settings: Key<SettingsService>,
}

impl PluginManagerPlugin {
    pub fn new(names: &Names<'_>, state: Arc<State>) -> rutis::Result<Self> {
        Ok(Self {
            state,
            loader: names.key()?,
            methods: names.key()?,
            parts: names.key()?,
            skills: names.key()?,
            mcp: names.key()?,
            runtimes: names.key()?,
            settings: names.key()?,
        })
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
            let manager = Manager {
                state: self.state.clone(),
                loader: ctx.require_at(&self.loader)?,
                owners: Owners {
                    ctx: ctx.clone(),
                    skills: self.skills.clone(),
                    mcp: self.mcp.clone(),
                },
                ctx: ctx.clone(),
                runtimes: self.runtimes.clone(),
                settings: self.settings.clone(),
            };
            let keep: Vec<PathBuf> = manager
                .state
                .record
                .lock()
                .unwrap()
                .installed
                .iter()
                .map(|entry| entry.dir.clone())
                .collect();
            manager.state.dirs.clean(&keep);
            let methods = ctx.require_at(&self.methods)?;
            let handler = manager.clone();
            methods.register(
                ctx,
                "plugin",
                METHODS
                    .iter()
                    .map(|name| MethodDecl {
                        progress: matches!(*name, "plugin.install" | "plugin.update"),
                        ..MethodDecl::new(name, *name != "plugin.list", Interruption::Report)
                    })
                    .collect(),
                Arc::new(move |method, params, _stopping, progress| {
                    let manager = handler.clone();
                    Box::pin(async move {
                        manager
                            .call(&method, params, Some(&progress))
                            .await
                            .map_err(RpcError::from)
                    })
                }),
            )?;
            let parts = ctx.require_at(&self.parts)?;
            parts.register(ctx, PartScope::Session, "PluginTools", move |mount| {
                Ok(Some(Box::new(tools::PluginTools::new(
                    mount.names,
                    manager.clone(),
                )?)))
            })?;
            Ok(Effect::Done)
        })
    }
}
