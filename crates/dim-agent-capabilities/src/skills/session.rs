use super::{CatalogChanged, SkillDefinition, SkillSummary, SkillView, SkillsCatalog, SkillsError};
use crate::{
    host::{HostError, HostFuture, LoadPrompt, ToolExecutionContext},
    input_queue::{
        CheckSubmission, SubmitCheckReply, SubmitCheckRequest, SubmitChecks, SubmitEntry,
    },
    prompt_registry::{PromptRegistry, targets},
    prompt_sources::sha256_version,
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry},
};
use dim_agent_runtime::{
    BranchLifecycle, ConfigureControl, ConfigureUpdate, Names, PromptSourceTarget, SESSION_GROUP,
    SessionConfig, SessionLifecycle, SessionLoader,
};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, InstanceId, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// The prompt source of the Skill list, delivered as state.
pub const LIST: &str = "dim/skills/list";
/// The prompt source of how to use Skills, part of the system prompt.
pub const INSTRUCTIONS_SOURCE: &str = "dim/skills/instructions";

type View = Result<Arc<SkillView>, SkillsError>;

/// Identifies a view for change detection, including an unavailable one.
fn view_version(view: &View) -> String {
    match view {
        Ok(view) => view.version.clone(),
        Err(error) => format!("unavailable:{}", sha256_version(&error.to_string())),
    }
}

/// The Session's current Skill view, or why it is unavailable.
pub struct SessionSkills {
    current: Mutex<View>,
    /// The how-to-use text: [`super::INSTRUCTIONS`] or the factory
    /// configuration's replacement.
    instructions: Arc<str>,
    /// Notified when the current view changes.
    changed: watch::Sender<()>,
}
impl SessionSkills {
    fn new(view: View, instructions: Arc<str>) -> Self {
        Self {
            current: Mutex::new(view),
            instructions,
            changed: watch::Sender::new(()),
        }
    }
    pub fn current(&self) -> View {
        self.current.lock().unwrap().clone()
    }
    /// Enabled Skills of the current view, for input suggestions.
    pub fn list(&self) -> Result<Vec<SkillSummary>, SkillsError> {
        Ok(self
            .current()?
            .skills
            .values()
            .map(|skill| SkillSummary {
                id: skill.id.clone(),
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect())
    }
    fn replace(&self, view: View) {
        let mut current = self.current.lock().unwrap();
        if view_version(&current) != view_version(&view) {
            *current = view;
            drop(current);
            self.changed.send_replace(());
        }
    }
}

/// Two prompt sources: how to use Skills, in the system prompt, and the
/// Skill list, state after the rules and the environment. The list escapes
/// its own SKILL.md values.
impl LoadPrompt for SessionSkills {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled("prompt source load cancelled".into()));
            }
            if target.source == INSTRUCTIONS_SOURCE {
                return Ok(Some(PromptEntry {
                    source: target.source,
                    version: sha256_version(&self.instructions),
                    body: self.instructions.to_string(),
                    scope: target.scope,
                    delivery: PromptDelivery::System,
                    order: 30,
                }));
            }
            let list = match self.current() {
                Ok(view) => view.list.clone(),
                Err(error) => Some(crate::context_block::text(&format!(
                    "Skills unavailable: {error}"
                ))),
            };
            Ok(list.map(|body| PromptEntry {
                source: target.source,
                version: sha256_version(&body),
                body,
                scope: target.scope,
                delivery: PromptDelivery::State,
                order: 300,
            }))
        })
    }
}

/// Session part: the current view, its list as a prompt source, and the
/// fixed `skill` tool.
pub struct SkillsPlugin {
    /// The Session instance its content services are keyed in.
    session: InstanceId,
    catalog: Key<SkillsCatalog>,
    config: Key<SessionConfig>,
    loader: Key<SessionLoader>,
    lifecycle: Key<SessionLifecycle>,
    prompts: Key<PromptRegistry>,
    tools: Key<ToolRegistry>,
    checks: Key<SubmitChecks>,
    skills: Key<SessionSkills>,
    /// The assembly's [`super::SkillsConfig`], when the assembly installed
    /// the capability ([`super::install_with_config`]): the install call
    /// registers the name and provides the value. The app server registers
    /// the parts from its catalog plugin for every assembly with a home and
    /// not every such assembly installs the capability, so a part with no
    /// name to declare keeps the built-in [`super::INSTRUCTIONS`].
    factory: Option<Key<super::SkillsConfig>>,
}
impl SkillsPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            session: names.instance(SESSION_GROUP)?,
            catalog: names.key()?,
            config: names.key()?,
            loader: names.key()?,
            lifecycle: names.key()?,
            prompts: names.key()?,
            tools: names.key()?,
            checks: names.key()?,
            skills: names.key()?,
            factory: names
                .registered::<super::SkillsConfig>()
                .then(|| names.key::<super::SkillsConfig>())
                .transpose()?,
        })
    }
}
impl DynPlugin for SkillsPlugin {
    fn name(&self) -> &str {
        "Skills"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut keys = vec![
            self.catalog.erase(),
            self.config.erase(),
            self.loader.erase(),
            self.lifecycle.erase(),
            self.prompts.erase(),
            self.tools.erase(),
            self.checks.erase(),
        ];
        keys.extend(self.factory.iter().map(Key::erase));
        keys
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let catalog = ctx.require_at(&self.catalog)?;
            let config = ctx.require_at(&self.config)?;
            let loader = ctx.require_at(&self.loader)?;
            let session = ctx.require_at(&self.lifecycle)?;
            let prompts = ctx.require_at(&self.prompts)?;
            let tools = ctx.require_at(&self.tools)?;
            let cwd = PathBuf::from(&config.cwd);
            let lifecycle = ctx.cancellation_token();
            let (wake, mut changes) = watch::channel(());
            ctx.listen::<CatalogChanged>()
                .name(super::catalog_changes())
                .on(move |_, _| {
                    wake.send_replace(());
                    async { Ok(None) }
                })?;
            let contents = Arc::new(tokio::sync::Mutex::new(super::content::Contents::new(
                self.session,
            )));
            let tasks = TaskTracker::new();
            let cleanup = contents.clone();
            let joined = tasks.clone();
            ctx.effect(move || async move {
                joined.close();
                joined.wait().await;
                Ok(cleanup.lock().await.close().await?)
            })?;
            let initial = tasks
                .spawn({
                    let (contents, catalog, cwd, loader, stopped) = (
                        contents.clone(),
                        catalog.clone(),
                        cwd.clone(),
                        loader.clone(),
                        lifecycle.clone(),
                    );
                    async move {
                        contents
                            .lock()
                            .await
                            .update(catalog.query(Some(&cwd)).await, &loader, &stopped)
                            .await
                    }
                })
                .await?;
            // The factory configuration's text replaces the built-in
            // instructions ([`super::SkillsConfig`]).
            let instructions: Arc<str> = match &self.factory {
                Some(key) => ctx
                    .require_at(key)?
                    .instructions
                    .clone()
                    .unwrap_or_else(|| super::INSTRUCTIONS.into()),
                None => super::INSTRUCTIONS.into(),
            };
            let skills = Arc::new(SessionSkills::new(initial, instructions));
            let owner = skills.clone();
            let stopped = lifecycle.clone();
            tasks.spawn(async move {
                tokio::select! {
                    _ = stopped.cancelled() => return,
                    published = session.activated() => if !published { return },
                }
                loop {
                    tokio::select! {
                        biased;
                        _ = stopped.cancelled() => return,
                        _ = session.closing() => return,
                        changed = changes.changed() => if changed.is_err() { return },
                    }
                    let cancel = stopped.child_token();
                    let update = async {
                        let view = catalog.query(Some(&cwd)).await;
                        contents.lock().await.update(view, &loader, &cancel).await
                    };
                    tokio::pin!(update);
                    let view = tokio::select! {
                        biased;
                        _ = session.closing() => {
                            cancel.cancel();
                            let _ = update.await;
                            return;
                        }
                        view = &mut update => view,
                    };
                    owner.replace(view);
                }
            });
            tools.register(
                ctx,
                ToolContribution {
                    definition: skill_definition(),
                    handler: Arc::new(SkillTool {
                        skills: skills.clone(),
                        stopped: lifecycle.clone(),
                    }),
                },
            )?;
            let checks = ctx.require_at(&self.checks)?;
            checks.register(
                ctx,
                "skills",
                Arc::new(SkillSubmitCheck {
                    skills: skills.clone(),
                    stopped: lifecycle,
                }),
            )?;
            for source in [INSTRUCTIONS_SOURCE, LIST] {
                prompts.register(ctx, source, skills.clone())?;
            }
            ctx.provide_at(self.skills.clone(), skills)?;
            Ok(Effect::Done)
        })
    }
}
/// TS `skillToolDefinition`.
fn skill_definition() -> ToolDefinition {
    ToolDefinition {
        risk: Some("session".into()),
        ..crate::tool_registry::definition(
            "skill",
            "Load and apply a named skill's full SKILL.md instructions. When the user's request matches an available skill, calling this tool is a BLOCKING REQUIREMENT. A request matches a skill when the user names the skill or the request clearly aligns with the skill name or description. Invoke this tool BEFORE generating any response about the task.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Skill name to load from the listed skills catalog"
                    }
                },
                "required": ["skill_name"]
            }),
        )
    }
}

/// The text a loaded Skill carries: the `skill` tool's result and the block a
/// referenced Skill adds to the operator's input share it.
fn skill_text(skill: &SkillDefinition) -> String {
    let mut parts = vec![
        format!("Skill: {}", skill.name),
        format!("Description: {}", skill.description),
        format!("File: {}", skill.file_path.display()),
    ];
    if !skill.allowed_tools.is_empty() {
        parts.push(format!("Allowed tools: {}", skill.allowed_tools.join(", ")));
    }
    parts.extend(["".into(), "Instructions:".into(), skill.body.clone()]);
    parts.join("\n")
}

/// The `skills` submit check (Hooks A.6.2): an operator's `$skill: <name>`
/// references load those Skills with the same input, before the model sees
/// it.
struct SkillSubmitCheck {
    skills: Arc<SessionSkills>,
    /// The part's lifetime: a stopped or reloading part adds nothing.
    stopped: CancellationToken,
}
impl CheckSubmission for SkillSubmitCheck {
    fn check(
        &self,
        request: SubmitCheckRequest,
        cancel: CancellationToken,
    ) -> HostFuture<'_, SubmitCheckReply> {
        Box::pin(async move {
            let interrupted = || HostError::Cancelled("the entry's call was interrupted".into());
            if cancel.is_cancelled() {
                return Err(interrupted());
            }
            let nothing = SubmitCheckReply::Proceed { blocks: Vec::new() };
            // A branch's first input is built from its prompt alone.
            if request.entry == SubmitEntry::Branch {
                return Ok(nothing);
            }
            let Some(user) = request.user.as_ref() else {
                return Ok(nothing);
            };
            let names = referenced(&user.content);
            if names.is_empty() {
                return Ok(nothing);
            }
            // A stopped part, or an unavailable view, has no opinion.
            if self.stopped.is_cancelled() {
                return Ok(nothing);
            }
            let Ok(view) = self.skills.current() else {
                return Ok(nothing);
            };
            // A steer of a queued input carries the block the input already
            // loaded: its reference must not load the Skill a second time.
            let already: std::collections::BTreeSet<String> = user
                .content
                .iter()
                .enumerate()
                .filter_map(|(index, block)| existing_skill(index, block))
                .collect();
            let mut seen = std::collections::BTreeSet::new();
            let mut blocks = Vec::new();
            for name in names {
                if cancel.is_cancelled() {
                    return Err(interrupted());
                }
                if !seen.insert(name.clone())
                    || already.contains(&crate::context_block::attr(&name))
                {
                    continue;
                }
                let Some(skill) = view.skills.get(&name) else {
                    continue;
                };
                blocks.push(crate::context_block::element(
                    "dim-skill",
                    &[("name", skill.name.as_str())],
                    &skill_text(skill),
                ));
            }
            Ok(SubmitCheckReply::Proceed { blocks })
        })
    }
}

/// Whether a text block after the first was added by a submit check: a loaded
/// Skill block, or a hook's `UserPromptSubmit` context. The first block is
/// always the operator's own.
fn submit_check_block(index: usize, block: &ContentBlock) -> bool {
    if index == 0 {
        return false;
    }
    let ContentBlock::Text { text } = block else {
        return false;
    };
    text.starts_with("<dim-skill name=\"") || text.starts_with("<dim-hooks kind=\"prompt-submit\">")
}

/// The Skill name an existing block already loaded, as its attribute holds
/// it: a text block after the first that starts with the marker.
fn existing_skill(index: usize, block: &ContentBlock) -> Option<String> {
    if index == 0 {
        return None;
    }
    let ContentBlock::Text { text } = block else {
        return None;
    };
    let rest = text.strip_prefix("<dim-skill name=\"")?;
    let name = rest.split('"').next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// The Skill names the operator's own text references, in order. A block a
/// submit check added is not operator text, so a steer follows no reference
/// inside it.
fn referenced(content: &[ContentBlock]) -> Vec<String> {
    let mut names = Vec::new();
    for (index, block) in content.iter().enumerate() {
        if submit_check_block(index, block) {
            continue;
        }
        let ContentBlock::Text { text } = block else {
            continue;
        };
        references(text, &mut names);
    }
    names
}

/// Pushes the Skill names `text` references: `$skill:`, optional spaces, then
/// the run of non-whitespace characters that follows, matched exactly.
fn references(text: &str, names: &mut Vec<String>) {
    let mut from = 0;
    while let Some(at) = text[from..].find("$skill:") {
        let after = from + at + "$skill:".len();
        let rest = text[after..].trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end > 0 {
            names.push(rest[..end].to_owned());
        }
        from = after + (text[after..].len() - rest.len()) + end;
    }
}

struct SkillTool {
    skills: Arc<SessionSkills>,
    stopped: CancellationToken,
}
impl ToolHandler for SkillTool {
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled("skill load cancelled".into()));
            }
            let Some(name) = call.input.get("skill_name").and_then(|name| name.as_str()) else {
                return Ok(ToolResult::error(
                    "Invalid input: skill_name must be a string",
                ));
            };
            if self.stopped.is_cancelled() {
                return Err(HostError::Cancelled("skills reloading".into()));
            }
            let view = match self.skills.current() {
                Ok(view) => view,
                Err(error) => return Ok(failure(format!("Skills unavailable: {error}"))),
            };
            let skill = match view.skills.get(name) {
                Some(skill) => skill.clone(),
                None if view.disabled.contains(name) => {
                    return Ok(failure(format!("Skill is disabled: {name}")));
                }
                None => return Ok(failure(format!("Unknown skill: {name}"))),
            };
            let content = skill_text(&skill);
            let structured = serde_json::json!({
                "success": true,
                "name": skill.name,
                "description": skill.description,
                "allowedTools": skill.allowed_tools,
                "filePath": skill.file_path.to_string_lossy(),
            });
            Ok(ToolResult {
                structured_content: structured.as_object().cloned(),
                ..ToolResult::text(content)
            })
        })
    }
}

fn failure(message: String) -> ToolResult {
    let structured = serde_json::json!({ "success": false, "error": message });
    ToolResult {
        structured_content: structured.as_object().cloned(),
        ..ToolResult::error(message)
    }
}

/// Branch part: while the branch is loaded, has Prompt adopt the Skill list
/// when the Session's view changes. Prompt delivers the list at
/// `branch/start` and `branch/resume`.
pub struct SkillsBranch {
    skills: Arc<SessionSkills>,
}
impl SkillsBranch {
    /// The Session's view, for entry points that reach Skills through a branch.
    pub fn session(&self) -> Arc<SessionSkills> {
        self.skills.clone()
    }
}

pub struct SkillsBranchPlugin {
    skills: Key<SessionSkills>,
    phase: Key<BranchLifecycle>,
    configure: Key<dyn ConfigureControl>,
    branch: Key<SkillsBranch>,
}
impl SkillsBranchPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            skills: names.key()?,
            phase: names.key()?,
            configure: names.key()?,
            branch: names.key()?,
        })
    }
}
impl DynPlugin for SkillsBranchPlugin {
    fn name(&self) -> &str {
        "SkillsBranch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.skills.erase(),
            self.phase.erase(),
            self.configure.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let skills = ctx.require_at(&self.skills)?;
            let phase = ctx.require_at(&self.phase)?;
            let configure = ctx.require_at(&self.configure)?;
            let lifecycle = ctx.cancellation_token();
            let mut changed = skills.changed.subscribe();
            changed.mark_unchanged();
            let tasks = TaskTracker::new();
            tasks.spawn(async move {
                tokio::select! {
                    _ = lifecycle.cancelled() => return,
                    active = phase.activated() => if !active { return },
                }
                loop {
                    tokio::select! {
                        _ = lifecycle.cancelled() => return,
                        _ = phase.closing() => return,
                        result = changed.changed() => if result.is_err() { return },
                    }
                    let update = ConfigureUpdate {
                        sources: targets([LIST]),
                        model: None,
                    };
                    tokio::select! {
                        _ = lifecycle.cancelled() => return,
                        result = configure.clone().configure(update) => if let Err(error) = result {
                            tracing::warn!(%error, "Skill list was not adopted");
                        },
                    }
                }
            });
            ctx.effect(move || async move {
                tasks.close();
                tasks.wait().await;
                Ok(())
            })?;
            ctx.provide_at(self.branch.clone(), Arc::new(SkillsBranch { skills }))?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_reads_the_operator_text_only() {
        let content = vec![
            ContentBlock::Text {
                text: "$skill: checkin hello".into(),
            },
            ContentBlock::Text {
                text: "<dim-hooks kind=\"prompt-submit\">\n$skill: second\n</dim-hooks>".into(),
            },
            ContentBlock::Text {
                text: "<dim-skill name=\"checkin\">\nInstructions:\n$skill: second\n</dim-skill>"
                    .into(),
            },
        ];
        assert_eq!(referenced(&content), vec!["checkin"]);
    }
}
