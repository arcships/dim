//! The `prompts` Settings source: user overrides of the built-in model-facing
//! policy texts ([prompt overrides](../../../docs/design/appserver/prompt-overrides.md)).
//!
//! The App Server owns the document because it owns Settings; each capability
//! owns its text and reads only its field through the host port it declares.
//! The factory configuration of every owner outranks this setting, so a
//! candidate assembly keeps the text it was built with.
use crate::settings::{Settings, SettingsScope, SourceDeclaration};
use dim_agent_capabilities::{
    host::{HostError, HostFuture},
    prompt_sources::{BasePromptSettings, BasePromptSettingsHost},
    skills::{SkillsInstructionsSettings, SkillsInstructionsSettingsHost},
    summary_strategy::{SummaryPromptSettings, SummaryPromptSettingsHost},
};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const SOURCE: &str = "prompts";

/// The user document. Its fields are independent: a missing one keeps that
/// text's built-in value.
#[derive(Debug, Default, Deserialize)]
struct PromptDocument {
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    skills: Option<String>,
}

struct Storage {
    settings: Arc<Settings>,
}

impl Storage {
    async fn read(&self, cancel: &CancellationToken) -> Result<PromptDocument, HostError> {
        let snapshot = self
            .settings
            .read(SOURCE, SettingsScope::User, cancel)
            .await
            .map_err(|error| HostError::failed(format!("prompts settings: {error}")))?;
        serde_json::from_value(snapshot.document)
            .map_err(|error| HostError::failed(format!("prompts settings: {error}")))
    }
}

impl BasePromptSettings for Storage {
    fn base(&self, cancel: CancellationToken) -> HostFuture<'_, Option<Arc<str>>> {
        Box::pin(async move { Ok(self.read(&cancel).await?.base.map(Arc::from)) })
    }
}

impl SkillsInstructionsSettings for Storage {
    fn instructions(&self, cancel: CancellationToken) -> HostFuture<'_, Option<Arc<str>>> {
        Box::pin(async move { Ok(self.read(&cancel).await?.skills.map(Arc::from)) })
    }
}

impl SummaryPromptSettings for Storage {
    fn system_prompt(&self, cancel: CancellationToken) -> HostFuture<'_, Option<Arc<str>>> {
        Box::pin(async move { Ok(self.read(&cancel).await?.summary.map(Arc::from)) })
    }
}

pub struct PromptSettingsPlugin {
    settings: Key<Settings>,
}

impl PromptSettingsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
        })
    }
}

impl DynPlugin for PromptSettingsPlugin {
    fn name(&self) -> &str {
        "PromptSettings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            settings.register(
                ctx,
                SourceDeclaration {
                    source: SOURCE.into(),
                    user_file: "prompts.json".into(),
                    project_file: None,
                    schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "base": {
                                "type": "string",
                                "minLength": 1,
                                "pattern": "\\S",
                                "description": "Replaces the built-in dim/base identity and rules."
                            },
                            "summary": {
                                "type": "string",
                                "minLength": 1,
                                "pattern": "\\S",
                                "description": "Replaces the built-in compaction summary system prompt."
                            },
                            "skills": {
                                "type": "string",
                                "minLength": 1,
                                "pattern": "\\S",
                                "description": "Replaces the built-in Skills how-to-use instructions."
                            }
                        }
                    }),
                    user_only: vec!["/base".into(), "/summary".into(), "/skills".into()],
                },
            )?;
            let storage = Arc::new(Storage { settings });
            let base: Arc<dyn BasePromptSettings> = storage.clone();
            let instructions: Arc<dyn SkillsInstructionsSettings> = storage.clone();
            let summary: Arc<dyn SummaryPromptSettings> = storage;
            let base = Arc::new(BasePromptSettingsHost(base));
            ctx.provide_as::<BasePromptSettingsHost>(base.clone())?;
            ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
                rutis_bridge::session::host_key("basePromptSettingsHost"),
                base,
            )?;
            let instructions = Arc::new(SkillsInstructionsSettingsHost(instructions));
            ctx.provide_as::<SkillsInstructionsSettingsHost>(instructions.clone())?;
            ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
                rutis_bridge::session::host_key("skillsInstructionsSettingsHost"),
                instructions,
            )?;
            let summary = Arc::new(SummaryPromptSettingsHost(summary));
            ctx.provide_as::<SummaryPromptSettingsHost>(summary.clone())?;
            ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
                rutis_bridge::session::host_key("summaryPromptSettingsHost"),
                summary,
            )?;
            Ok(Effect::Done)
        })
    }
}
