//! Skills management wire boundary; discovery and enablement belong to SkillsCatalog.
use crate::{
    methods::{ManagementMethods, SelectionScope},
    protocol::{canonical_cwd, optional_non_null},
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, params, reply};
use dim_agent_capabilities::skills::{RefreshTarget, SkillStatus, SkillsCatalog, SkillsError};
use dim_agent_capabilities::{
    host_platform::HostPlatform,
    skill_settings::{
        SOURCE, SettingsVersion, SkillSettings, SkillSettingsError, SkillSettingsHost,
        SkillSettingsSnapshot,
    },
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillDocument {
    disabled_skill_ids: Vec<String>,
    /// Skills roots registered by plugins, by plugin id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    plugin_skills: BTreeMap<String, PathBuf>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
impl BusinessDocument for SkillDocument {
    const NAMESPACE: &'static str = SOURCE;
    const USER_FILE: &'static str = "skills.json";
    fn validate(&self) -> crate::settings::Result<()> {
        if self.extra.contains_key("disabledSkillIds") || self.extra.contains_key("pluginSkills") {
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
        Ok(())
    }
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
    /// `user`, `project` or `plugin:<plugin id>`: where the Skill was found.
    pub source: String,
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

fn error(error: SkillsError) -> RpcError {
    let kind = match &error {
        SkillsError::Closed => return RpcError::reloading(),
        SkillsError::UnknownSkill(_) => "skill_unknown",
        SkillsError::SettingsConflict => "skills_conflict",
        SkillsError::SettingsWrite(_) => "skills_write_failed",
        SkillsError::SettingsSync(_) => "skills_sync_failed",
        SkillsError::SettingsRead(_) | SkillsError::RootUnreadable { .. } => "skills_unavailable",
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
                ["skills.catalog", "skills.set_enabled", "skills.refresh"]
                    .into_iter()
                    .map(|name| {
                        MethodDecl::new(name, name != "skills.catalog", Interruption::Report)
                    })
                    .collect(),
                Arc::new(move |method, value, _, _| {
                    let catalog = catalog.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "skills.catalog" => {
                                let p: SelectionScope = params(value)?;
                                let cwd = match p.cwd {
                                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                                    None => None,
                                };
                                let view = catalog.query(cwd.as_deref()).await.map_err(error)?;
                                reply(SkillsCatalogResult {
                                    version: view.version,
                                    entries: view
                                        .entries
                                        .into_iter()
                                        .map(|entry| SkillEntry {
                                            id: entry.definition.id.clone(),
                                            source: entry.source.to_string(),
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
                                    .map_err(error)?;
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
                                    .map_err(error)?;
                                reply(SkillsRefreshResult {
                                    version,
                                    issues: issues.into_iter().map(issue).collect(),
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
