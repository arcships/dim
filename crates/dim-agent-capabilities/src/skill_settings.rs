//! Product-owned Skill enablement storage. Discovery and effective Skill views
//! belong to Skills; this port has no filesystem or app-server dependency.
use rutis::host::BoxFuture;
use std::{collections::BTreeSet, sync::Arc};
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
}

/// Storage port supplied to the catalog by its embedding host. A standalone
/// catalog plugin injects this service; a host mounting the catalog may pass it
/// privately without publishing a separate service or adapter plugin.
pub struct SkillSettingsHost(pub Arc<dyn SkillSettings>);
