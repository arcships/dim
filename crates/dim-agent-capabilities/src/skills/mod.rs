//! Skills: process-level discovery and enablement (`SkillsCatalog`), the
//! per-Session view, its list as a prompt source and the `skill` tool, and
//! the per-branch adoption of list changes (`SkillsBranch`).
mod catalog;
pub(crate) mod content;
mod discovery;
mod session;

pub use catalog::{
    CatalogChanged, MemorySkillSettings, SkillsCatalog, SkillsCatalogPlugin, catalog_changes,
};
pub use session::{SessionSkills, SkillsBranch, SkillsBranchPlugin, SkillsPlugin};

use dim_agent_runtime::{AgentBuilder, PartRegistry, PartScope};
use rutis::{Ctx, Error};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

/// The parts the Skills plugin registers.
const PARTS: [&str; 2] = ["Skills", "SkillsBranch"];

/// Requires the Session and branch parts in the profile and adds the
/// per-content extension. The Skills plugin (`SkillsCatalogPlugin`, or a
/// host's plugin that mounts its catalog) registers the parts.
pub fn install(builder: &mut AgentBuilder) {
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
    /// TS `getSkillId`: `user:<rel>` or `workspace:<skills dir>:<rel>`.
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

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub definition: Arc<SkillDefinition>,
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillsError {
    #[error("Skills catalog is closed")]
    Closed,
    #[error("unknown skill: {0}")]
    UnknownSkill(String),
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
- Loading: Use `skill({ \"skill_name\": \"<name>\" })` to load the full SKILL.md instructions. After the tool returns, follow the loaded instructions.
- Multiple matches: When multiple skills apply, load the smallest set that covers the request and apply them in a practical order.
- Missing or blocked: If a named skill is unavailable or cannot be loaded, say so briefly and continue with the best fallback.
- Context hygiene: Load full skill instructions only for skills you will use for the current request.
- Safety and fallback: If a loaded skill cannot be applied cleanly, state the issue briefly and continue with the next-best approach.
</skills_instructions>";
