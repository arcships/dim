//! Product-owned Skill enablement storage. Discovery and effective Skill views
//! belong to Skills; this port has no filesystem or app-server dependency.
use rutis::host::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

/// The Settings source holding Skill enablement. A host whose storage changes
/// outside the catalog announces each write as `settings.changed` on
/// `settings_changes(SOURCE)`; the catalog reads `skills()` again after each.
pub const SOURCE: &str = "skills";

/// Opaque source revision, not a notification counter.
pub type SettingsVersion = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSettingsSnapshot {
    pub version: SettingsVersion,
    pub disabled: BTreeSet<String>,
    /// The skills roots registered by plugins, by plugin id.
    pub plugins: BTreeMap<String, PathBuf>,
    /// The installations the user made from a local directory or a Git
    /// repository, by installation id.
    pub installations: BTreeMap<String, SkillInstallation>,
}

/// Where an installation was acquired from. A Git source keeps the URL as the
/// user gave it, like `plugins.json`; replies project it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstallation {
    pub source: SkillInstallSource,
    /// The revision of the installed selection, in install order.
    pub revision: String,
    /// Source-relative Skill directories, in install order.
    pub skills: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillInstallSource {
    Path {
        path: PathBuf,
    },
    Git {
        git: String,
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        commit: String,
    },
}

impl SkillInstallSource {
    /// The installation id a source gets when the caller names none: the last
    /// path component, so a credential-bearing and a clean URL of the same
    /// repository derive the same id.
    pub fn default_id(&self) -> String {
        let (value, git) = match self {
            Self::Path { path } => (path.to_string_lossy().into_owned(), false),
            Self::Git { git, .. } => (git.clone(), true),
        };
        let mut name = value
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_owned();
        if git && name.to_ascii_lowercase().ends_with(".git") {
            name.truncate(name.len() - 4);
        }
        let mut id: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else if c == '_' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        id.truncate(64);
        let id = id.trim_matches('-');
        if id.is_empty() {
            "skills".into()
        } else {
            id.to_owned()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SkillSettingsError {
    #[error("Skill settings changed; read the current version before updating")]
    Conflict,
    #[error("invalid Skill settings: {0}")]
    Invalid(String),
    #[error("Skill settings operation cancelled")]
    Cancelled,
    #[error("Skill settings service is closed")]
    Closed,
    #[error("Skill settings: {0}")]
    Failed(String),
}

pub trait SkillSettings: Send + Sync + 'static {
    fn skills(
        &self,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<SkillSettingsSnapshot, SkillSettingsError>>;

    /// Change one ID while preserving other IDs and extension fields. A stale
    /// expected version conflicts even if the requested state already matches.
    fn update_skills(
        self: Arc<Self>,
        expected: SettingsVersion,
        id: String,
        enabled: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>>;

    /// Records `root` as the skills root of `plugin`, or forgets the plugin
    /// when `root` is `None`, preserving every other field. It applies to
    /// the current version, whatever it is.
    fn set_plugin(
        self: Arc<Self>,
        plugin: String,
        root: Option<PathBuf>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>>;

    /// Records the installation `id`, or forgets it when `installation` is
    /// `None`, preserving every other field. It applies to the current
    /// version, whatever it is.
    fn set_installation(
        self: Arc<Self>,
        id: String,
        installation: Option<SkillInstallation>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<SettingsVersion, SkillSettingsError>>;
}

/// Storage port supplied to the catalog by its embedding host. A standalone
/// catalog plugin injects this service; a host mounting the catalog may pass it
/// privately without publishing a separate service or adapter plugin.
pub struct SkillSettingsHost(pub Arc<dyn SkillSettings>);
