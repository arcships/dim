//! Hooks configuration: the `hooks`, `hooks-projects` and `hooks-plugins`
//! Settings sources, the project file `.dimcode/hooks.json`, the `hooks.*`
//! management methods and the Session binding of Hooks' `HookSourcesHost`
//! (Hooks A.1, A.2, A.7).
use crate::{
    methods::{ManagementMethods, SelectionScope},
    protocol::canonical_cwd,
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService, SettingsSnapshot},
};
use dim_agent_capabilities::{
    file_storage::{FileStorage, FileStorageError},
    hooks::{
        HookGroup, HookSourceId, HookSourceSnapshot, HookSourceState, HookSourcesHost,
        HookSourcesSnapshot, HooksDocument, ReadHookSources, entries,
    },
    methods::{Interruption, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::{AgentBuilder, Feature, Names};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

/// The project's hook document in its repository (A.1).
const PROJECT_FILE: &str = ".dimcode/hooks.json";
/// A hook document larger than this is refused without reading it.
const PROJECT_FILE_LIMIT: usize = 1024 * 1024;

impl BusinessDocument for HooksDocument {
    const NAMESPACE: &'static str = "hooks";
    const USER_FILE: &'static str = "hooks.json";

    /// Every handler is checked separately and reported by the catalog, so a
    /// document is valid whenever it parses.
    fn validate(&self) -> crate::settings::Result<()> {
        Ok(())
    }
}

/// The user's enablement of project hooks (ADR 003 D3), keyed by canonical
/// workspace. A project source starts disabled: its own document cannot
/// enable it, and only this user-local record does.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksProjects {
    #[serde(default)]
    pub enabled: BTreeSet<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}
impl BusinessDocument for HooksProjects {
    const NAMESPACE: &'static str = "hooks-projects";
    const USER_FILE: &'static str = "hooks-projects.json";

    fn validate(&self) -> crate::settings::Result<()> {
        if self.extra.contains_key("enabled")
            || self
                .enabled
                .iter()
                .any(|workspace| !Path::new(workspace).is_absolute())
        {
            return Err(SettingsError::Invalid("invalid hook project enablement"));
        }
        Ok(())
    }
}

/// The hook documents plugins registered, by plugin id (A.2). PluginManager's
/// owner operations write them and Hooks keeps them here, so a registration
/// survives Hooks' reload; its commands read the entry's `dir` as
/// `DIM_PLUGIN_ROOT`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksPlugins {
    #[serde(default)]
    pub plugins: BTreeMap<String, HooksPluginDocument>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HooksPluginDocument {
    pub dir: PathBuf,
    pub hooks: BTreeMap<String, Vec<HookGroup>>,
}
impl BusinessDocument for HooksPlugins {
    const NAMESPACE: &'static str = "hooks-plugins";
    const USER_FILE: &'static str = "hooks-plugins.json";

    fn validate(&self) -> crate::settings::Result<()> {
        if self.extra.contains_key("plugins")
            || self
                .plugins
                .iter()
                .any(|(plugin, document)| plugin.trim().is_empty() || !document.dir.is_absolute())
        {
            return Err(SettingsError::Invalid("invalid hook plugin registration"));
        }
        Ok(())
    }
}

/// The sources plugins registered, after the user's and the project's (A.2).
/// A plugin source is enabled while PluginManager keeps it registered, so it
/// needs no workspace enablement.
fn plugin_sources(documents: HooksPlugins) -> Vec<HookSourceSnapshot> {
    documents
        .plugins
        .into_iter()
        .map(|(plugin, registration)| HookSourceSnapshot {
            id: HookSourceId::Plugin(plugin),
            path: Some(registration.dir),
            enabled: true,
            state: HookSourceState::Ok,
            document: Some(HooksDocument {
                hooks: registration.hooks,
                disable_all_hooks: false,
            }),
        })
        .collect()
}

/// Reads the effective sources for a working directory. The user document is
/// a Settings source; the project file is read through FileStorage under the
/// root Settings authorized for the workspace.
struct SettingsHookSources {
    settings: Arc<SettingsService>,
    files: Arc<FileStorage>,
    home: Option<PathBuf>,
}

/// The user source of one read: missing until something is stored.
fn user_source(revision: &str, document: HooksDocument) -> HookSourceSnapshot {
    let missing = revision == "missing";
    HookSourceSnapshot {
        id: HookSourceId::User,
        path: None,
        enabled: true,
        state: if missing {
            HookSourceState::Missing
        } else {
            HookSourceState::Ok
        },
        document: (!missing).then_some(document),
    }
}

impl ReadHookSources for SettingsHookSources {
    fn sources(
        &self,
        cwd: &Path,
        cancel: CancellationToken,
    ) -> dim_agent_capabilities::host::HostFuture<'_, HookSourcesSnapshot> {
        let cwd = cwd.to_path_buf();
        Box::pin(async move {
            let user = self
                .settings
                .read::<HooksDocument>(SettingsScope::User, &cancel)
                .await
                .map_err(host_error)?;
            let projects = self
                .settings
                .read::<HooksProjects>(SettingsScope::User, &cancel)
                .await
                .map_err(host_error)?;
            let plugins = self
                .settings
                .read::<HooksPlugins>(SettingsScope::User, &cancel)
                .await
                .map_err(host_error)?;
            let mut sources = vec![user_source(&user.revision, user.document)];
            // A hook point reads; it never authorizes. The Session's own
            // creation registered its root.
            if let Some(root) = self.settings.project_root(&cwd) {
                sources.push(self.project(root, &projects.document, &cancel).await?);
            }
            sources.extend(plugin_sources(plugins.document));
            Ok(HookSourcesSnapshot { sources })
        })
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
}

impl SettingsHookSources {
    /// The sources of a workspace whose root the caller authorized.
    async fn authorized(
        &self,
        root: usize,
        user: &SettingsSnapshot<HooksDocument>,
        projects: &SettingsSnapshot<HooksProjects>,
        plugins: &SettingsSnapshot<HooksPlugins>,
        cancel: &CancellationToken,
    ) -> Result<HookSourcesSnapshot, dim_agent_capabilities::host::HostError> {
        let mut sources = vec![
            user_source(&user.revision, user.document.clone()),
            self.project(root, &projects.document, cancel).await?,
        ];
        sources.extend(plugin_sources(plugins.document.clone()));
        Ok(HookSourcesSnapshot { sources })
    }

    /// The workspace's source, from the root Settings authorized for it.
    async fn project(
        &self,
        root: usize,
        projects: &HooksProjects,
        cancel: &CancellationToken,
    ) -> Result<HookSourceSnapshot, dim_agent_capabilities::host::HostError> {
        let workspace = self.settings.project_path(root).map_err(host_error)?;
        let workspace_key = workspace.to_string_lossy().into_owned();
        let path = self
            .files
            .path(root, PROJECT_FILE)
            .map_err(|error| host_error_message(error.to_string()))?;
        let state = match self
            .files
            .read_limited(&path, PROJECT_FILE_LIMIT, cancel)
            .await
        {
            Ok(bytes) => match serde_json::from_slice::<HooksDocument>(&bytes) {
                Ok(document) => {
                    return Ok(HookSourceSnapshot {
                        id: HookSourceId::Project,
                        path: Some(workspace.join(PROJECT_FILE)),
                        enabled: projects.enabled.contains(&workspace_key),
                        state: HookSourceState::Ok,
                        document: Some(document),
                    });
                }
                Err(error) => HookSourceState::Invalid(format!(
                    "invalid hook document at line {}, column {}",
                    error.line(),
                    error.column()
                )),
            },
            Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                HookSourceState::Missing
            }
            Err(FileStorageError::Cancelled) => {
                return Err(dim_agent_capabilities::host::HostError::Cancelled(
                    "hook sources read cancelled".into(),
                ));
            }
            Err(error) => HookSourceState::Invalid(error.to_string()),
        };
        Ok(HookSourceSnapshot {
            id: HookSourceId::Project,
            path: Some(workspace.join(PROJECT_FILE)),
            enabled: projects.enabled.contains(&workspace_key),
            state,
            document: None,
        })
    }
}

fn host_error(error: SettingsError) -> dim_agent_capabilities::host::HostError {
    match error {
        SettingsError::Cancelled => {
            dim_agent_capabilities::host::HostError::Cancelled("hook sources read cancelled".into())
        }
        error => host_error_message(error.to_string()),
    }
}
fn host_error_message(message: String) -> dim_agent_capabilities::host::HostError {
    dim_agent_capabilities::host::HostError::failed(message)
}

/// Registers the Session part that binds the hook sources; the host registers
/// `HooksSettingsPlugin` with its Settings.
pub fn install(builder: &mut AgentBuilder, product_home: Option<PathBuf>) {
    crate::register_names(builder.catalog_mut());
    builder.require_feature(
        Feature::new("HookSources").session("HookSources", move |names| {
            Ok(Box::new(HookSourcesPlugin::new(
                names,
                product_home.clone(),
            )?))
        }),
    );
}

struct HookSourcesPlugin {
    host: Key<HookSourcesHost>,
    settings: Key<SettingsService>,
    files: Key<FileStorage>,
    home: Option<PathBuf>,
}
impl HookSourcesPlugin {
    fn new(names: &Names<'_>, home: Option<PathBuf>) -> rutis::Result<Self> {
        Ok(Self {
            host: names.key()?,
            settings: names.key()?,
            files: names.key()?,
            home,
        })
    }
}
impl DynPlugin for HookSourcesPlugin {
    fn name(&self) -> &str {
        "HookSources"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), self.files.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let files = ctx.require_at(&self.files)?;
            ctx.provide_at(
                self.host.clone(),
                Arc::new(HookSourcesHost(Arc::new(SettingsHookSources {
                    settings,
                    files,
                    home: self.home.clone(),
                }))),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Revision {
    revision: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    user_revision: String,
    /// The user's document as stored, so a client can edit and replace it
    /// with the revision it read.
    user_document: HooksDocument,
    projects_revision: String,
    disable_all_hooks: bool,
    sources: Vec<SourceView>,
    hooks: Vec<dim_agent_capabilities::hooks::HookEntry>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceView {
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    enabled: bool,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}
impl SourceView {
    fn of(source: &HookSourceSnapshot) -> Self {
        let (state, message) = match &source.state {
            HookSourceState::Ok => ("ok", None),
            HookSourceState::Missing => ("missing", None),
            HookSourceState::Invalid(message) => ("invalid", Some(message.clone())),
        };
        Self {
            source: source.id.to_string(),
            path: source
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            enabled: source.enabled,
            state,
            message,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveHooks {
    expected_revision: String,
    document: HooksDocument,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetHookProject {
    cwd: String,
    enabled: bool,
    expected_revision: String,
}

fn error(error: SettingsError) -> RpcError {
    let kind = match &error {
        SettingsError::Closed => return RpcError::reloading(),
        SettingsError::Cancelled => "operation_interrupted",
        SettingsError::Conflict => "hooks_conflict",
        SettingsError::Invalid(_) => "hooks_invalid",
        _ => "hooks_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

/// The port over this host's Settings and files.
fn reader(
    settings: &Arc<SettingsService>,
    files: &Arc<FileStorage>,
    home: Option<PathBuf>,
) -> Arc<SettingsHookSources> {
    Arc::new(SettingsHookSources {
        settings: settings.clone(),
        files: files.clone(),
        home,
    })
}

/// Hooks' owner operations (PluginManager §3.7): PluginManager registers and
/// withdraws a plugin's hook document here, and Hooks keeps it in
/// `hooks-plugins`.
pub struct HooksService {
    settings: Arc<SettingsService>,
    /// Why the last registration of a plugin was refused, by plugin id.
    refused: Mutex<BTreeMap<String, String>>,
}
impl HooksService {
    /// Adds or replaces the hook document of `plugin` (a plugin id, §3.7),
    /// whose commands read `dir` as `DIM_PLUGIN_ROOT` (Hooks A.2, A.3). The
    /// registration is kept in `hooks-plugins.json` and survives Hooks'
    /// reload. A document that does not parse is refused: the plugin then has
    /// none, and [`Self::plugin_refusal`] reports why.
    pub async fn register_plugin(
        &self,
        plugin: String,
        dir: PathBuf,
        document: serde_json::Value,
        cancel: &CancellationToken,
    ) -> crate::settings::Result<()> {
        let registration = serde_json::from_value::<HooksDocument>(document)
            .map_err(|_| SettingsError::Invalid("invalid hook document"))
            .and_then(|document| {
                let registration = HooksPluginDocument {
                    dir,
                    hooks: document.hooks,
                };
                HooksPlugins {
                    plugins: BTreeMap::from([(plugin.clone(), registration.clone())]),
                    extra: BTreeMap::new(),
                }
                .validate()
                .map(|()| registration)
            });
        let result = match registration {
            Ok(registration) => {
                self.set_plugin(plugin.clone(), Some(registration), cancel)
                    .await
            }
            Err(error) => {
                let _ = self.set_plugin(plugin.clone(), None, cancel).await;
                Err(error)
            }
        };
        let mut refused = self.refused.lock().unwrap();
        match &result {
            Err(error) => refused.insert(plugin, error.to_string()),
            Ok(()) => refused.remove(&plugin),
        };
        result
    }

    /// Withdraws the hook document of `plugin`; nothing when it registered
    /// none.
    pub async fn withdraw_plugin(
        &self,
        plugin: String,
        cancel: &CancellationToken,
    ) -> crate::settings::Result<()> {
        self.refused.lock().unwrap().remove(&plugin);
        self.set_plugin(plugin, None, cancel).await
    }

    /// Why Hooks refused the last document `plugin` registered.
    pub fn plugin_refusal(&self, plugin: &str) -> Option<String> {
        self.refused.lock().unwrap().get(plugin).cloned()
    }

    /// The plugin ids whose hook documents Hooks holds.
    pub async fn plugins(
        &self,
        cancel: &CancellationToken,
    ) -> crate::settings::Result<Vec<String>> {
        Ok(self
            .settings
            .read::<HooksPlugins>(SettingsScope::User, cancel)
            .await?
            .document
            .plugins
            .into_keys()
            .collect())
    }

    /// Registers or withdraws one entry. Registrations of different plugins
    /// commute, so a lost race on the shared document is retried rather than
    /// reported as a conflict the caller cannot observe.
    async fn set_plugin(
        &self,
        plugin: String,
        registration: Option<HooksPluginDocument>,
        cancel: &CancellationToken,
    ) -> crate::settings::Result<()> {
        loop {
            let current = self
                .settings
                .read::<HooksPlugins>(SettingsScope::User, cancel)
                .await?;
            let mut document = current.document;
            if document.plugins.get(&plugin) == registration.as_ref() {
                return Ok(());
            }
            match &registration {
                Some(registration) => {
                    document
                        .plugins
                        .insert(plugin.clone(), registration.clone());
                }
                None => {
                    document.plugins.remove(&plugin);
                }
            }
            match self
                .settings
                .update(SettingsScope::User, &current.revision, document, cancel)
                .await
            {
                Ok(_) => return Ok(()),
                Err(SettingsError::Conflict) => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

/// Process owner of the `hooks` sources and their management methods.
pub struct HooksSettingsPlugin {
    settings: Key<SettingsService>,
    files: Key<FileStorage>,
    methods: Key<ManagementMethods>,
    product_home: Option<PathBuf>,
}
impl HooksSettingsPlugin {
    pub fn new(names: &Names<'_>, product_home: Option<PathBuf>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            files: names.key()?,
            methods: names.key()?,
            product_home,
        })
    }
}
impl DynPlugin for HooksSettingsPlugin {
    fn name(&self) -> &str {
        "HooksSettings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.settings.erase(),
            self.files.erase(),
            self.methods.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            settings.register::<HooksDocument>(ctx)?;
            settings.register::<HooksProjects>(ctx)?;
            settings.register::<HooksPlugins>(ctx)?;
            let service = Arc::new(HooksService {
                settings: settings.clone(),
                refused: Mutex::default(),
            });
            ctx.provide_as::<HooksService>(service)?;
            let reader = reader(&settings, &files, self.product_home.clone());
            methods.register(
                ctx,
                "hooks",
                ["hooks.catalog", "hooks.save", "hooks.set_enabled"]
                    .into_iter()
                    .map(|name| {
                        MethodDecl::new(name, name != "hooks.catalog", Interruption::Report)
                    })
                    .collect(),
                Arc::new(move |method, value, stopping, _| {
                    let settings = settings.clone();
                    let reader = reader.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "hooks.catalog" => {
                                let p: SelectionScope = params(value)?;
                                let user = settings
                                    .read::<HooksDocument>(SettingsScope::User, &stopping)
                                    .await
                                    .map_err(error)?;
                                let projects = settings
                                    .read::<HooksProjects>(SettingsScope::User, &stopping)
                                    .await
                                    .map_err(error)?;
                                let plugins = settings
                                    .read::<HooksPlugins>(SettingsScope::User, &stopping)
                                    .await
                                    .map_err(error)?;
                                let snapshot = match p.cwd {
                                    Some(cwd) => {
                                        let cwd = canonical_cwd(&cwd).await?;
                                        let root = settings
                                            .authorize_project(cwd, &stopping)
                                            .await
                                            .map_err(error)?;
                                        reader
                                            .authorized(root, &user, &projects, &plugins, &stopping)
                                            .await
                                            .map_err(|error| {
                                                RpcError::app(
                                                    -32000,
                                                    "hooks_failed",
                                                    error.to_string(),
                                                )
                                            })?
                                    }
                                    None => {
                                        let mut sources = vec![user_source(
                                            &user.revision,
                                            user.document.clone(),
                                        )];
                                        sources.extend(plugin_sources(plugins.document));
                                        HookSourcesSnapshot { sources }
                                    }
                                };
                                let disable_all_hooks = snapshot
                                    .sources
                                    .iter()
                                    .find(|source| source.id == HookSourceId::User)
                                    .and_then(|source| source.document.as_ref())
                                    .is_some_and(|document| document.disable_all_hooks);
                                reply(Catalog {
                                    user_revision: user.revision,
                                    user_document: user.document,
                                    projects_revision: projects.revision,
                                    disable_all_hooks,
                                    sources: snapshot.sources.iter().map(SourceView::of).collect(),
                                    hooks: entries(&snapshot),
                                })
                            }
                            "hooks.save" => {
                                let p: SaveHooks = params(value)?;
                                let snapshot = settings
                                    .update(
                                        SettingsScope::User,
                                        &p.expected_revision,
                                        p.document,
                                        &stopping,
                                    )
                                    .await
                                    .map_err(error)?;
                                reply(Revision {
                                    revision: snapshot.revision,
                                })
                            }
                            "hooks.set_enabled" => {
                                let p: SetHookProject = params(value)?;
                                let cwd = canonical_cwd(&p.cwd).await?;
                                let root = settings
                                    .authorize_project(cwd, &stopping)
                                    .await
                                    .map_err(error)?;
                                let workspace = settings
                                    .project_path(root)
                                    .map_err(error)?
                                    .to_string_lossy()
                                    .into_owned();
                                let snapshot = settings
                                    .read::<HooksProjects>(SettingsScope::User, &stopping)
                                    .await
                                    .map_err(error)?;
                                if snapshot.revision != p.expected_revision {
                                    return Err(error(SettingsError::Conflict));
                                }
                                let mut document = snapshot.document;
                                let changed = if p.enabled {
                                    document.enabled.insert(workspace)
                                } else {
                                    document.enabled.remove(&workspace)
                                };
                                if !changed {
                                    return reply(Revision {
                                        revision: snapshot.revision,
                                    });
                                }
                                let snapshot = settings
                                    .update(
                                        SettingsScope::User,
                                        &snapshot.revision,
                                        document,
                                        &stopping,
                                    )
                                    .await
                                    .map_err(error)?;
                                reply(Revision {
                                    revision: snapshot.revision,
                                })
                            }
                            _ => unreachable!("registered hooks management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{methods::ManagementMethodsPlugin, settings::SettingsPlugin};
    use dim_agent_capabilities::file_storage::FileStoragePlugin;
    use dim_agent_runtime::wait_active;
    use rutis::Ctx;
    use rutis::host::CtxHost;
    use rutis_loader::resolve::Build;
    use std::time::Duration;

    /// A hook point reads the sources of the workspace the Session was created
    /// in; it never registers a root or emits an authorization. Only the
    /// management methods authorize.
    #[tokio::test]
    async fn a_hook_point_reads_sources_without_authorizing_a_project() {
        let home = std::env::temp_dir().join(format!("dim-hooks-read-{}", uuid::Uuid::new_v4()));
        let project = home.join("project");
        std::fs::create_dir_all(project.join(".dimcode")).unwrap();
        std::fs::write(
            project.join(".dimcode/hooks.json"),
            br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
        )
        .unwrap();
        let home = std::fs::canonicalize(home).unwrap();
        let project = home.join("project");
        let root = Ctx::root().unwrap();
        let catalog = crate::catalog();
        let build = Build::default();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![home.clone()])),
            root.plugin(SettingsPlugin::new(&Names::copy(&catalog, &build), 0, vec![]).unwrap()),
            root.plugin(
                HooksSettingsPlugin::new(&Names::copy(&catalog, &build), Some(home.clone()))
                    .unwrap(),
            ),
            root.plugin(ManagementMethodsPlugin::typed(&catalog).unwrap()),
        ];
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let settings = root.get::<SettingsService>().unwrap();
        let files = root.get::<FileStorage>().unwrap();
        let sources = SettingsHookSources {
            settings: settings.clone(),
            files,
            home: Some(home.clone()),
        };
        let snapshot = sources.sources(&project, cancel.clone()).await.unwrap();
        assert_eq!(
            snapshot.sources.len(),
            1,
            "an unauthorized workspace has no project source"
        );
        assert!(
            settings.project_root(&project).is_none(),
            "a hook point authorized a project"
        );

        // The management path authorizes, then the same read sees the source.
        settings
            .authorize_project(project.clone(), &cancel)
            .await
            .unwrap();
        let snapshot = sources.sources(&project, cancel.clone()).await.unwrap();
        assert_eq!(snapshot.sources.len(), 2);
        assert_eq!(snapshot.sources[1].id, HookSourceId::Project);
        assert!(!snapshot.sources[1].enabled);
        assert!(matches!(snapshot.sources[1].state, HookSourceState::Ok));
        root.shutdown().await.unwrap();
        std::fs::remove_dir_all(&home).unwrap();
    }
}
