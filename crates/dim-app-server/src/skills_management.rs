//! Skills management wire boundary; discovery, enablement and installations
//! belong to SkillsCatalog.
use crate::{
    methods::{ManagementMethods, SelectionScope},
    plugin_manager::{
        PluginError,
        acquire::{self, Staging},
    },
    protocol::{canonical_cwd, optional_non_null},
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, params, reply};
use dim_agent_capabilities::skills::{
    InstallOutcome, RefreshTarget, SelectedSkill, SkillPreview, SkillStatus, SkillsCatalog,
    SkillsError, valid_installation_id,
};
use dim_agent_capabilities::{
    host_platform::HostPlatform,
    skill_settings::{
        SOURCE, SettingsVersion, SkillInstallSource, SkillInstallation, SkillSettings,
        SkillSettingsError, SkillSettingsHost, SkillSettingsSnapshot,
    },
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// Where an installation's source is read and prepared before it is renamed
/// into the user skills root.
const STAGING: &str = ".agents/.staging";

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillDocument {
    disabled_skill_ids: Vec<String>,
    /// Skills roots registered by plugins, by plugin id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    plugin_skills: BTreeMap<String, PathBuf>,
    /// Installations the user made from a local directory or a Git
    /// repository, by installation id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    installations: BTreeMap<String, SkillInstallation>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
impl BusinessDocument for SkillDocument {
    const NAMESPACE: &'static str = SOURCE;
    const USER_FILE: &'static str = "skills.json";
    fn validate(&self) -> crate::settings::Result<()> {
        if self.extra.contains_key("disabledSkillIds")
            || self.extra.contains_key("pluginSkills")
            || self.extra.contains_key("installations")
        {
            return Err(SettingsError::Invalid("reserved Skill configuration field"));
        }
        if self
            .disabled_skill_ids
            .iter()
            .any(|id| id.trim().is_empty())
        {
            return Err(SettingsError::Invalid("empty Skill id"));
        }
        if self
            .plugin_skills
            .iter()
            .any(|(plugin, root)| plugin.trim().is_empty() || !root.is_absolute())
        {
            return Err(SettingsError::Invalid("invalid plugin Skills root"));
        }
        for (id, installation) in &self.installations {
            if !valid_installation_id(id) {
                return Err(SettingsError::Invalid("invalid Skill installation id"));
            }
            if installation.skills.is_empty() {
                return Err(SettingsError::Invalid("Skill installation selects nothing"));
            }
            if installation
                .skills
                .iter()
                .any(|path| !relative_path(path).is_some())
            {
                return Err(SettingsError::Invalid(
                    "invalid Skill installation selection path",
                ));
            }
            if let SkillInstallSource::Path { path } = &installation.source
                && !path.is_absolute()
            {
                return Err(SettingsError::Invalid(
                    "Skill installation source path is not absolute",
                ));
            }
        }
        Ok(())
    }
}

/// A selection or record path: relative, no `.` or `..` component.
fn relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() || path.has_root() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Adapts the owner's business document to the reusable Skill catalog port.
/// Settings announces its writes; the catalog listens to them itself.
struct SkillStorage {
    settings: Arc<SettingsService>,
}
impl SkillSettings for SkillStorage {
    fn skills(
        &self,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<SkillSettingsSnapshot, SkillSettingsError>> {
        Box::pin(async move {
            let snapshot = self
                .settings
                .read::<SkillDocument>(SettingsScope::User, &cancel)
                .await?;
            Ok(SkillSettingsSnapshot {
                version: snapshot.revision,
                disabled: snapshot.document.disabled_skill_ids.into_iter().collect(),
                plugins: snapshot.document.plugin_skills,
                installations: snapshot.document.installations,
            })
        })
    }
    fn update_skills(
        self: Arc<Self>,
        expected: SettingsVersion,
        id: String,
        enabled: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(async move {
            if id.trim().is_empty() {
                return Err(SkillSettingsError::Invalid("empty Skill id".into()));
            }
            let current = self
                .settings
                .read::<SkillDocument>(SettingsScope::User, &cancel)
                .await?;
            if current.revision != expected {
                return Err(SkillSettingsError::Conflict);
            }
            let mut document = current.document;
            if document.disabled_skill_ids.contains(&id) != enabled {
                return Ok(current.revision);
            }
            if enabled {
                document.disabled_skill_ids.retain(|value| value != &id);
            } else {
                document.disabled_skill_ids.push(id);
            }
            Ok(self
                .settings
                .update(SettingsScope::User, &expected, document, &cancel)
                .await?
                .revision)
        })
    }
    fn set_plugin(
        self: Arc<Self>,
        plugin: String,
        root: Option<PathBuf>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(async move { SkillStorage::set_plugin(&self, plugin, root, cancel).await })
    }
    fn set_installation(
        self: Arc<Self>,
        id: String,
        installation: Option<SkillInstallation>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>> {
        Box::pin(
            async move { SkillStorage::set_installation(&self, id, installation, cancel).await },
        )
    }
}
impl SkillStorage {
    async fn set_plugin(
        &self,
        plugin: String,
        root: Option<PathBuf>,
        cancel: CancellationToken,
    ) -> Result<SettingsVersion, SkillSettingsError> {
        loop {
            let current = self
                .settings
                .read::<SkillDocument>(SettingsScope::User, &cancel)
                .await?;
            let mut document = current.document;
            let unchanged = match &root {
                Some(root) => document.plugin_skills.get(&plugin) == Some(root),
                None => !document.plugin_skills.contains_key(&plugin),
            };
            if unchanged {
                return Ok(current.revision);
            }
            match &root {
                Some(root) => document.plugin_skills.insert(plugin.clone(), root.clone()),
                None => document.plugin_skills.remove(&plugin),
            };
            match self
                .settings
                .update(SettingsScope::User, &current.revision, document, &cancel)
                .await
            {
                Ok(snapshot) => return Ok(snapshot.revision),
                Err(SettingsError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn set_installation(
        &self,
        id: String,
        installation: Option<SkillInstallation>,
        cancel: CancellationToken,
    ) -> Result<SettingsVersion, SkillSettingsError> {
        loop {
            let current = self
                .settings
                .read::<SkillDocument>(SettingsScope::User, &cancel)
                .await?;
            let mut document = current.document;
            let unchanged = match &installation {
                Some(installation) => document.installations.get(&id) == Some(installation),
                None => !document.installations.contains_key(&id),
            };
            if unchanged {
                return Ok(current.revision);
            }
            match &installation {
                Some(installation) => document
                    .installations
                    .insert(id.clone(), installation.clone()),
                None => document.installations.remove(&id),
            };
            match self
                .settings
                .update(SettingsScope::User, &current.revision, document, &cancel)
                .await
            {
                Ok(snapshot) => return Ok(snapshot.revision),
                Err(SettingsError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
}
impl From<SettingsError> for SkillSettingsError {
    fn from(error: SettingsError) -> Self {
        match error {
            SettingsError::Conflict => Self::Conflict,
            SettingsError::Invalid(reason) => Self::Invalid(reason.into()),
            SettingsError::Closed => Self::Closed,
            SettingsError::Cancelled => Self::Cancelled,
            error => Self::Failed(error.to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSkillEnabled {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub id: String,
    pub enabled: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub allowed_tools: Vec<String>,
    pub enabled: bool,
    pub shadowed_by: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct SkillIssue {
    pub path: String,
    pub message: String,
}
#[derive(Debug, Serialize)]
pub struct SkillsCatalogResult {
    pub version: u64,
    pub entries: Vec<SkillEntry>,
    pub issues: Vec<SkillIssue>,
}
#[derive(Debug, Serialize)]
pub struct SkillsRefreshResult {
    pub version: u64,
    pub issues: Vec<SkillIssue>,
}
#[derive(Debug, Serialize)]
pub struct SkillsVersion {
    pub version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum SourceParams {
    Path {
        path: PathBuf,
    },
    Git {
        git: String,
        #[serde(rename = "ref", default)]
        reference: Option<String>,
    },
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewParams {
    pub source: SourceParams,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub id: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallParams {
    pub source: SourceParams,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub id: Option<String>,
    pub skills: Vec<SelectedSkillParams>,
    #[serde(default)]
    pub overwrite: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedSkillParams {
    pub path: String,
    pub revision: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationParams {
    pub id: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SourceReply {
    Path {
        path: String,
    },
    Git {
        git: String,
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        commit: String,
    },
}
impl From<&SkillInstallSource> for SourceReply {
    fn from(source: &SkillInstallSource) -> Self {
        match source {
            SkillInstallSource::Path { path } => Self::Path {
                path: path.to_string_lossy().into_owned(),
            },
            SkillInstallSource::Git {
                git,
                reference,
                commit,
            } => Self::Git {
                git: acquire::git_url_safe(git),
                reference: reference.clone(),
                commit: commit.clone(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSkill {
    pub path: String,
    pub name: String,
    pub description: String,
    pub revision: String,
}
#[derive(Debug, Serialize)]
pub struct PreviewNamed {
    pub path: String,
    pub name: String,
    pub by: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsPreviewResult {
    pub source: SourceReply,
    pub skills: Vec<PreviewSkill>,
    pub conflicts: Vec<PreviewNamed>,
    pub shadows: Vec<PreviewNamed>,
    pub issues: Vec<SkillIssue>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallationSkill {
    pub path: String,
    pub id: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallationReply {
    pub id: String,
    pub source: SourceReply,
    pub revision: String,
    pub skills: Vec<InstallationSkill>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub installation: InstallationReply,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<InstallationReply>,
    pub version: u64,
    pub issues: Vec<SkillIssue>,
}
#[derive(Debug, Serialize)]
pub struct InstallationsResult {
    pub installations: Vec<InstallationReply>,
}

fn installation(id: &str, record: &SkillInstallation) -> InstallationReply {
    InstallationReply {
        id: id.to_owned(),
        source: SourceReply::from(&record.source),
        revision: record.revision.clone(),
        skills: record
            .skills
            .iter()
            .map(|path| InstallationSkill {
                path: path.clone(),
                id: format!("user:{id}/{path}"),
            })
            .collect(),
    }
}

pub(crate) fn skills_error(error: SkillsError) -> RpcError {
    let kind = match &error {
        SkillsError::Closed => return RpcError::reloading(),
        SkillsError::UnknownSkill(_) => "skill_unknown",
        SkillsError::SettingsConflict => "skills_conflict",
        SkillsError::SettingsWrite(_) => "skills_write_failed",
        SkillsError::SettingsSync(_) => "skills_sync_failed",
        SkillsError::SettingsRead(_) | SkillsError::RootUnreadable { .. } => "skills_unavailable",
        SkillsError::InstallationExists(_) => "skills_install_exists",
        SkillsError::UnknownInstallation(_) => "skills_install_unknown",
        SkillsError::StalePreview { .. } => "skills_preview_stale",
        SkillsError::SelectionInvalid(_) => "skills_selection_invalid",
        SkillsError::InstallConflict(_) => "skills_install_conflict",
        SkillsError::InstallIo(_) => "skills_install_failed",
        SkillsError::RemoveFailed(_) => "skills_remove_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

fn issue(issue: dim_agent_capabilities::skills::SkillIssue) -> SkillIssue {
    use dim_agent_capabilities::skills::SkillIssue as Source;
    match issue {
        Source::InvalidManifest { path, reason } | Source::Unreadable { path, reason } => {
            SkillIssue {
                path: path.to_string_lossy().into_owned(),
                message: reason,
            }
        }
        Source::RootUnreadable { root, reason } => SkillIssue {
            path: root.to_string_lossy().into_owned(),
            message: reason,
        },
        Source::ScanLimitReached { root } => SkillIssue {
            path: root.to_string_lossy().into_owned(),
            message: "Skill scan limit reached".into(),
        },
    }
}

fn invalid(message: &str) -> RpcError {
    RpcError::app(-32000, "skills_source_invalid", message)
}

fn acquire_error(error: PluginError) -> RpcError {
    match error.code {
        "invalid_source" | "invalid_id" | "invalid_manifest" => invalid(
            "Use an absolute directory or an https:// or ssh:// Git URL; check the source and ref",
        ),
        _ => RpcError::app(
            -32000,
            "skills_acquire_failed",
            "Skill source acquisition failed; check access and filesystem permissions",
        ),
    }
}

/// A source read into the operation's staging directory. The directory is
/// removed when this is dropped.
struct Acquired {
    source: SkillInstallSource,
    root: PathBuf,
    staging: Staging,
}

async fn acquire(params: SourceParams, home: &Path) -> Result<Acquired, RpcError> {
    let staging = Staging::in_root(&home.join(STAGING)).map_err(acquire_error)?;
    match params {
        SourceParams::Path { path } => {
            if !path.is_absolute() {
                return Err(invalid("Use an absolute source directory"));
            }
            let root = tokio::fs::canonicalize(&path)
                .await
                .map_err(|_| invalid("The source directory does not exist"))?;
            let meta = tokio::fs::metadata(&root)
                .await
                .map_err(|_| invalid("The source directory cannot be read"))?;
            if !meta.is_dir() {
                return Err(invalid("The source is not a directory"));
            }
            acquire::check_links(&root, &root).map_err(acquire_error)?;
            Ok(Acquired {
                source: SkillInstallSource::Path { path: root.clone() },
                root,
                staging,
            })
        }
        SourceParams::Git { git, reference } => {
            acquire::check_git_url(&git).map_err(acquire_error)?;
            if let Some(reference) = &reference {
                acquire::check_ref(reference).map_err(acquire_error)?;
            }
            let commit = acquire::clone(&git, reference.as_deref(), &staging)
                .await
                .map_err(acquire_error)?;
            let root = tokio::fs::canonicalize(acquire::work_dir(&staging))
                .await
                .map_err(|_| invalid("The acquired source cannot be read"))?;
            Ok(Acquired {
                source: SkillInstallSource::Git {
                    git,
                    reference,
                    commit,
                },
                root,
                staging,
            })
        }
    }
}

/// Reads an installation's recorded source again.
fn recorded(source: &SkillInstallSource) -> SourceParams {
    match source {
        SkillInstallSource::Path { path } => SourceParams::Path { path: path.clone() },
        SkillInstallSource::Git { git, reference, .. } => SourceParams::Git {
            git: git.clone(),
            reference: reference.clone(),
        },
    }
}

pub struct SkillsCatalogPlugin {
    platform: Key<HostPlatform>,
    settings: Key<SettingsService>,
    methods: Key<ManagementMethods>,
    parts: Key<dim_agent_runtime::PartRegistry>,
}
impl SkillsCatalogPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            platform: names.key()?,
            settings: names.key()?,
            methods: names.key()?,
            parts: names.key()?,
        })
    }
}
impl DynPlugin for SkillsCatalogPlugin {
    fn name(&self) -> &str {
        "SkillsCatalog"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.platform.erase(),
            self.settings.erase(),
            self.methods.erase(),
            self.parts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let platform = ctx.require_at(&self.platform)?;
            let settings = ctx.require_at(&self.settings)?;
            let methods = ctx.require_at(&self.methods)?;
            let parts = ctx.require_at(&self.parts)?;
            let home = platform.home.clone();
            settings.register::<SkillDocument>(ctx)?;
            let settings = Arc::new(SkillSettingsHost(Arc::new(SkillStorage { settings })));
            let catalog = dim_agent_capabilities::skills::SkillsCatalogPlugin::mount(
                ctx,
                (platform, settings),
            )
            .await?;
            ctx.provide_as::<SkillsCatalog>(catalog.clone())?;
            dim_agent_capabilities::skills::register_parts(ctx, &parts)?;
            methods.register(
                ctx,
                "skills",
                [
                    "skills.catalog",
                    "skills.set_enabled",
                    "skills.refresh",
                    "skills.preview",
                    "skills.install",
                    "skills.update",
                    "skills.remove",
                    "skills.installations",
                ]
                .into_iter()
                .map(|name| {
                    MethodDecl::new(
                        name,
                        !matches!(
                            name,
                            "skills.catalog" | "skills.preview" | "skills.installations"
                        ),
                        Interruption::Report,
                    )
                })
                .collect(),
                Arc::new(move |method, value, _, _| {
                    let catalog = catalog.clone();
                    let home = home.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "skills.catalog" => {
                                let p: SelectionScope = params(value)?;
                                let cwd = match p.cwd {
                                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                                    None => None,
                                };
                                let view =
                                    catalog.query(cwd.as_deref()).await.map_err(skills_error)?;
                                reply(SkillsCatalogResult {
                                    version: view.version,
                                    entries: view
                                        .entries
                                        .into_iter()
                                        .map(|entry| SkillEntry {
                                            id: entry.definition.id.clone(),
                                            name: entry.definition.name.clone(),
                                            description: entry.definition.description.clone(),
                                            file_path: entry
                                                .definition
                                                .file_path
                                                .to_string_lossy()
                                                .into_owned(),
                                            allowed_tools: entry.definition.allowed_tools.clone(),
                                            enabled: entry.enabled,
                                            shadowed_by: match entry.status {
                                                SkillStatus::Active => None,
                                                SkillStatus::Shadowed { by } => Some(by),
                                            },
                                        })
                                        .collect(),
                                    issues: view.issues.into_iter().map(issue).collect(),
                                })
                            }
                            "skills.set_enabled" => {
                                let p: SetSkillEnabled = params(value)?;
                                if p.id.trim().is_empty() {
                                    return Err(RpcError::invalid("id must not be empty"));
                                }
                                let cwd = match p.cwd {
                                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                                    None => None,
                                };
                                let (_, version) = catalog
                                    .set_enabled(&p.id, p.enabled, cwd.as_deref())
                                    .await
                                    .map_err(skills_error)?;
                                reply(SkillsVersion { version })
                            }
                            "skills.refresh" => {
                                let p: SelectionScope = params(value)?;
                                let project = match p.cwd {
                                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                                    None => None,
                                };
                                let (version, issues) = catalog
                                    .refresh(RefreshTarget {
                                        user: true,
                                        project,
                                    })
                                    .await
                                    .map_err(skills_error)?;
                                reply(SkillsRefreshResult {
                                    version,
                                    issues: issues.into_iter().map(issue).collect(),
                                })
                            }
                            "skills.preview" => {
                                let p: PreviewParams = params(value)?;
                                let acquired = acquire(p.source, &home).await?;
                                let id = p.id.unwrap_or_else(|| acquired.source.default_id());
                                let preview: SkillPreview = catalog
                                    .preview(&acquired.root, &id)
                                    .await
                                    .map_err(skills_error)?;
                                reply(SkillsPreviewResult {
                                    source: SourceReply::from(&acquired.source),
                                    skills: preview
                                        .skills
                                        .into_iter()
                                        .map(|skill| PreviewSkill {
                                            path: skill.path,
                                            name: skill.name,
                                            description: skill.description,
                                            revision: skill.revision,
                                        })
                                        .collect(),
                                    conflicts: preview
                                        .conflicts
                                        .into_iter()
                                        .map(|conflict| PreviewNamed {
                                            path: conflict.path,
                                            name: conflict.name,
                                            by: conflict.by,
                                        })
                                        .collect(),
                                    shadows: preview
                                        .shadows
                                        .into_iter()
                                        .map(|shadow| PreviewNamed {
                                            path: shadow.path,
                                            name: shadow.name,
                                            by: shadow.by,
                                        })
                                        .collect(),
                                    issues: preview.issues.into_iter().map(issue).collect(),
                                })
                            }
                            "skills.install" => {
                                let p: InstallParams = params(value)?;
                                let acquired = acquire(p.source, &home).await?;
                                let id = p.id.unwrap_or_else(|| acquired.source.default_id());
                                let selection: Vec<SelectedSkill> = p
                                    .skills
                                    .into_iter()
                                    .map(|skill| SelectedSkill {
                                        path: skill.path,
                                        revision: Some(skill.revision),
                                    })
                                    .collect();
                                let outcome = catalog
                                    .install(
                                        &id,
                                        acquired.source.clone(),
                                        &acquired.root,
                                        acquired.staging.path(),
                                        &selection,
                                        p.overwrite,
                                    )
                                    .await
                                    .map_err(skills_error)?;
                                reply(install_result(&id, outcome))
                            }
                            "skills.update" => {
                                let p: InstallationParams = params(value)?;
                                let record = catalog
                                    .installation(&p.id)
                                    .await
                                    .map_err(skills_error)?
                                    .ok_or_else(|| {
                                        skills_error(SkillsError::UnknownInstallation(p.id.clone()))
                                    })?;
                                let acquired = acquire(recorded(&record.source), &home).await?;
                                let selection: Vec<SelectedSkill> = record
                                    .skills
                                    .iter()
                                    .map(|path| SelectedSkill {
                                        path: path.clone(),
                                        revision: None,
                                    })
                                    .collect();
                                let outcome = catalog
                                    .install(
                                        &p.id,
                                        acquired.source.clone(),
                                        &acquired.root,
                                        acquired.staging.path(),
                                        &selection,
                                        true,
                                    )
                                    .await
                                    .map_err(skills_error)?;
                                reply(install_result(&p.id, outcome))
                            }
                            "skills.remove" => {
                                let p: InstallationParams = params(value)?;
                                let (_, version) =
                                    catalog.uninstall(&p.id).await.map_err(skills_error)?;
                                reply(SkillsVersion { version })
                            }
                            "skills.installations" => {
                                let recorded =
                                    catalog.installations().await.map_err(skills_error)?;
                                reply(InstallationsResult {
                                    installations: recorded
                                        .iter()
                                        .map(|(id, record)| installation(id, record))
                                        .collect(),
                                })
                            }
                            _ => unreachable!("registered Skills management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

fn install_result(id: &str, outcome: InstallOutcome) -> InstallResult {
    InstallResult {
        installation: installation(id, &outcome.installation),
        previous: outcome
            .previous
            .as_ref()
            .map(|record| installation(id, record)),
        version: outcome.version,
        issues: outcome.issues.into_iter().map(issue).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_from_before_this_field_loads_and_unknown_members_survive() {
        let mut document: SkillDocument =
            serde_json::from_str(r#"{"disabledSkillIds":["user:a"],"customField":{"kept":true}}"#)
                .unwrap();
        assert!(document.installations.is_empty());
        document.validate().unwrap();
        document.installations.insert(
            "acme".into(),
            SkillInstallation {
                source: SkillInstallSource::Path {
                    path: PathBuf::from("/src/skills"),
                },
                revision: "sha256:x".into(),
                skills: vec!["writing/review".into()],
                extra: BTreeMap::new(),
            },
        );
        let text = serde_json::to_string(&document).unwrap();
        assert!(text.contains(r#""customField":{"kept":true}"#), "{text}");
        assert!(text.contains(r#""installations":{"acme":"#), "{text}");
        let reloaded: SkillDocument = serde_json::from_str(&text).unwrap();
        reloaded.validate().unwrap();
        assert_eq!(reloaded.installations["acme"].skills, ["writing/review"]);
    }

    #[test]
    fn an_invalid_installation_is_refused_at_the_document_boundary() {
        for text in [
            r#"{"disabledSkillIds":[],"installations":{"Bad Id":{"source":{"path":"/src"},"revision":"x","skills":["a"]}}}"#,
            r#"{"disabledSkillIds":[],"installations":{"a":{"source":{"path":"relative"},"revision":"x","skills":["a"]}}}"#,
            r#"{"disabledSkillIds":[],"installations":{"a":{"source":{"path":"/src"},"revision":"x","skills":["../a"]}}}"#,
            r#"{"disabledSkillIds":[],"installations":{"a":{"source":{"path":"/src"},"revision":"x","skills":[]}}}"#,
        ] {
            let document: SkillDocument = serde_json::from_str(text).unwrap();
            assert!(document.validate().is_err(), "{text}");
        }
    }
}
