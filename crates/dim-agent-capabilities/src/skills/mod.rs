//! Skills: process-level discovery and enablement (`SkillsCatalog`), the
//! per-Session view, its list as a prompt source and the `skill` tool, and
//! the per-branch adoption of list changes (`SkillsBranch`).
mod catalog;
pub(crate) mod content;
mod discovery;
mod install;
mod session;

pub use catalog::{
    CatalogChanged, MemorySkillSettings, SkillsCatalog, SkillsCatalogPlugin, catalog_changes,
};
pub use install::selection_path;
pub use session::{SessionSkills, SkillsBranch, SkillsBranchPlugin, SkillsPlugin};

use crate::skill_settings::SkillInstallation;
use dim_agent_runtime::{AgentBuilder, PartRegistry, PartScope};
use rutis::{Ctx, Error};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

/// The parts the Skills plugin registers.
const PARTS: [&str; 2] = ["Skills", "SkillsBranch"];

/// Skills' factory configuration: the text that replaces [`INSTRUCTIONS`]
/// ([model-prompts §9.2](../../../docs/design/model-prompts.md#92-every-capability-that-writes-model-facing-text)).
#[derive(Debug, Clone, Default)]
pub struct SkillsConfig {
    /// The how-to-use text in place of [`INSTRUCTIONS`].
    pub instructions: Option<Arc<str>>,
}

/// Requires the Session and branch parts in the profile and adds the
/// per-content extension. The Skills plugin (`SkillsCatalogPlugin`, or a
/// host's plugin that mounts its catalog) registers the parts.
pub fn install(builder: &mut AgentBuilder) {
    install_with_config(builder, SkillsConfig::default());
}

/// As [`install`], with `config` as the factory configuration: the parts it
/// registers deliver the configured instructions instead of the built-in
/// text.
pub fn install_with_config(builder: &mut AgentBuilder, config: SkillsConfig) {
    crate::register_names(builder);
    dim_agent_runtime::names::register::<SkillsConfig>(
        builder.catalog_mut(),
        Some(dim_agent_runtime::SESSION_GROUP),
    );
    builder.session_value(Arc::new(config));
    builder.session_extension_part::<content::ContentConfig>(
        content::GROUP,
        "SkillContent",
        |_, config| Ok(Box::new(content::ContentPlugin(config.clone()))),
    );
    for part in PARTS {
        builder.require_part(part);
    }
}

/// Registers the Session and branch parts of Skills on behalf of the plugin
/// that owns `ctx`, which holds the catalog they read.
pub fn register_parts(ctx: &Ctx, parts: &std::sync::Arc<PartRegistry>) -> Result<(), Error> {
    parts.register(ctx, PartScope::Session, PARTS[0], |mount| {
        Ok(Some(Box::new(SkillsPlugin::new(mount.names)?)))
    })?;
    parts.register(ctx, PartScope::Branch, PARTS[1], |mount| {
        Ok(Some(Box::new(SkillsBranchPlugin::new(mount.names)?)))
    })
}

/// A parsed `SKILL.md`; the body is read at scan time with path variables
/// already replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDefinition {
    /// TS `getSkillId`: `user:<rel>` or `workspace:<skills dir>:<rel>`; a
    /// plugin's Skill is `plugin:<plugin id>:<rel>`.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Descriptive only; never grants permissions.
    pub allowed_tools: Vec<String>,
    pub file_path: PathBuf,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillStatus {
    Active,
    Shadowed { by: String },
}

/// Where a Skill was found, as the catalog reports it: `user`, `project` or
/// `plugin:<plugin id>`. Clients group entries by it instead of reading the
/// id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    User,
    Project,
    Plugin(String),
}
impl std::fmt::Display for SkillSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Project => f.write_str("project"),
            Self::Plugin(plugin) => write!(f, "plugin:{plugin}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub definition: Arc<SkillDefinition>,
    pub source: SkillSource,
    pub enabled: bool,
    pub status: SkillStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillIssue {
    InvalidManifest { path: PathBuf, reason: String },
    Unreadable { path: PathBuf, reason: String },
    RootUnreadable { root: PathBuf, reason: String },
    ScanLimitReached { root: PathBuf },
}

/// Monotonic within the process; changes when a snapshot is replaced or the
/// disabled set changes.
pub type CatalogVersion = u64;

#[derive(Debug, Clone)]
pub struct CatalogView {
    pub version: CatalogVersion,
    pub entries: Vec<CatalogEntry>,
    pub issues: Vec<SkillIssue>,
}

/// An installation id: `[a-z0-9_-]{1,64}`, which is also its directory name
/// under the user skills root.
pub fn valid_installation_id(id: &str) -> bool {
    install::valid_id(id)
}

/// One Skill a preview found; `path` is the selection identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSkill {
    pub path: String,
    pub name: String,
    pub description: String,
    pub revision: String,
}

/// A Skill in the user root that already provides one of the selected names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewConflict {
    pub path: String,
    pub name: String,
    pub by: String,
}

/// A Skill outside the user root that the installation would hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewShadow {
    pub path: String,
    pub name: String,
    pub by: String,
}

#[derive(Debug, Clone)]
pub struct SkillPreview {
    pub skills: Vec<PreviewSkill>,
    pub conflicts: Vec<PreviewConflict>,
    pub shadows: Vec<PreviewShadow>,
    pub issues: Vec<SkillIssue>,
}

/// A Skill the caller selected, with the revision it reviewed: `Some` must
/// match the tree, `None` accepts what the source holds (an update).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSkill {
    pub path: String,
    pub revision: Option<String>,
}

/// What an install may replace at `<user root>/<id>`. Only a directory the
/// record owns is ever replaced; the user root also holds hand-written Skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replace {
    /// Create the installation. A recorded id is [`SkillsError::InstallationExists`].
    Create,
    /// Replace the record the id holds now, or create it (the user confirmed
    /// the overwrite).
    Recorded,
    /// Replace exactly this record. A record that is gone or different is
    /// [`SkillsError::UnknownInstallation`], so an update cannot undo a removal
    /// or another install made while its source was acquired.
    Expected(SkillInstallation),
}

#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub installation: SkillInstallation,
    pub previous: Option<SkillInstallation>,
    pub version: CatalogVersion,
    pub issues: Vec<SkillIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillsError {
    #[error("Skills catalog is closed")]
    Closed,
    #[error("unknown skill: {0}")]
    UnknownSkill(String),
    #[error("Skill installation {0:?} already exists; confirm overwrite or choose another id")]
    InstallationExists(String),
    #[error("unknown Skill installation: {0}")]
    UnknownInstallation(String),
    /// `<user root>/<id>` holds something no record owns, which is never
    /// replaced: the user root is where hand-written Skills live.
    #[error("{0}")]
    UnmanagedDirectory(String),
    #[error("Skill {path:?} changed since the preview: reviewed {expected}, found {found}")]
    StalePreview {
        path: String,
        expected: String,
        found: String,
    },
    #[error("invalid Skill selection: {0}")]
    SelectionInvalid(String),
    #[error("Skill {0:?} is already provided by another Skill")]
    InstallConflict(String),
    #[error("Skill installation failed: {0}")]
    InstallIo(String),
    #[error("Skill removal failed: {0}")]
    RemoveFailed(String),
    #[error("Skill settings changed; retry")]
    SettingsConflict,
    #[error("Skill settings read failed: {0}")]
    SettingsRead(String),
    #[error("Skill settings write failed: {0}")]
    SettingsWrite(String),
    /// The write succeeded but reading the settings back failed; the catalog
    /// retries on its next query.
    #[error("Skill settings were saved but the catalog could not read them back: {0}")]
    SettingsSync(String),
    #[error("Skill root is unreadable: {reason}")]
    RootUnreadable { root: PathBuf, reason: String },
}

/// Which roots an explicit refresh rescans.
#[derive(Debug, Clone, Default)]
pub struct RefreshTarget {
    pub user: bool,
    /// The Session cwd whose project root is rescanned.
    pub project: Option<PathBuf>,
}

/// What a Session's model may see and load. `version` digests every Skill's
/// identity and body, so a body-only change is a new view.
#[derive(Debug)]
pub struct SkillView {
    pub version: String,
    pub skills: BTreeMap<String, Arc<SkillDefinition>>,
    pub disabled: BTreeSet<String>,
    /// Rendered list state; `None` when no Skill is enabled.
    pub list: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
}

const METADATA_BUDGET_CHARS: usize = 4000;

impl SkillView {
    pub(crate) fn from_contributions(
        skills: BTreeMap<String, Arc<SkillDefinition>>,
        disabled: BTreeSet<String>,
    ) -> Self {
        let list = (!skills.is_empty()).then(|| render_skill_metadata(&skills));
        let identity = serde_json::json!({
            "skills": skills.values().map(|skill| serde_json::json!([
                skill.id,
                skill.name,
                skill.description,
                skill.allowed_tools,
                skill.file_path.to_string_lossy(),
                skill.body,
            ])).collect::<Vec<_>>(),
            "disabled": disabled,
        });
        Self {
            version: crate::prompt_sources::sha256_version(&identity.to_string()),
            skills,
            disabled,
            list,
        }
    }
}

/// The list of TS `renderSkillMetadata`: every name is listed; descriptions
/// are added cheapest first within the character budget. Names and
/// descriptions come from SKILL.md files and are escaped.
fn render_skill_metadata(skills: &BTreeMap<String, Arc<SkillDefinition>>) -> String {
    let cost = |skill: &SkillDefinition| format!(": {}", skill.description).encode_utf16().count();
    let mut by_cost: Vec<_> = skills.values().collect();
    by_cost.sort_by_key(|skill| cost(skill));
    let mut used = 0;
    let mut described = BTreeSet::new();
    for skill in by_cost {
        if used + cost(skill) <= METADATA_BUDGET_CHARS {
            used += cost(skill);
            described.insert(skill.name.as_str());
        }
    }
    let list = skills
        .values()
        .map(|skill| {
            let name = crate::context_block::text(&skill.name);
            if described.contains(skill.name.as_str()) {
                format!(
                    "- {name}: {}",
                    crate::context_block::text(&skill.description)
                )
            } else {
                format!("- {name}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n{list}\n")
}

/// How to use Skills (TS `renderSkillMetadata`'s instructions): fixed text,
/// part of the system prompt while Skills is installed. The list itself is
/// state (`dim/skills/list`).
pub(crate) const INSTRUCTIONS: &str = "<skills_instructions>
## Skills
A skill is a local instruction bundle stored in a SKILL.md file. Use the `skill` tool to load the full instructions before applying a skill.

### How to use skills
- Discovery: The latest <dim-state source=\"dim/skills/list\"> lists the skills available in this session, each with a name and, when space allows, a description.
- Trigger rules: When the user's request matches an available skill, calling the `skill` tool is a BLOCKING REQUIREMENT. A request matches a skill when the user names the skill or the request clearly aligns with the skill name or description. Invoke the relevant `skill` tool BEFORE generating any response about the task. A `$skill: name` marker names the exact skill to load.
- Loading: Use `skill({ \"skill_name\": \"<name>\" })` to load the full SKILL.md instructions. After the tool returns, follow the loaded instructions. A `$skill: name` marker followed in the same message by a `<dim-skill name=\"name\">` block means that skill is already loaded: follow the block and do not call `skill` for it. A marker without such a block is loaded with the tool.
- Multiple matches: When multiple skills apply, load the smallest set that covers the request and apply them in a practical order.
- Missing or blocked: If a named skill is unavailable or cannot be loaded, say so briefly and continue with the best fallback.
- Context hygiene: Load full skill instructions only for skills you will use for the current request.
- Safety and fallback: If a loaded skill cannot be applied cleanly, state the issue briefly and continue with the next-best approach.
</skills_instructions>";
