//! Goal as a native plugin ([Goal](../../../docs/design/plugins/Goal.md),
//! [Pursuit](../../../docs/design/plugins/Pursuit.md)): one row that registers
//! its Settings source, the Session part that writes change inputs by branch
//! id, and the branch part that folds the branch's events and decides
//! continuation.

mod branch;
mod fold;
mod session;
mod text;

pub use text::GoalTexts;

use dim_agent_runtime::{AgentBuilder, Names, PartRegistry, PartScope};
use dim_app_server::settings::{SettingsService, SourceDeclaration};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::sync::Arc;

/// Goal's Settings source, and the plugin row's name.
const SOURCE: &str = "goal";

/// The name of Goal's Session part, as the assembly profile requires it.
pub const SESSION_PART: &str = "GoalSession";
/// The name of Goal's branch part.
pub const BRANCH_PART: &str = "GoalBranch";

/// Installs Goal with its shipped policy texts (A.5.1).
pub fn install(builder: &mut AgentBuilder) {
    install_with(builder, GoalTexts::default());
}

/// Installs Goal with `texts` in place of the shipped policy sentences: a
/// candidate replaces them at the factory, so the code keeps no copy of
/// either sentence (model-prompts §8.1).
pub fn install_with(builder: &mut AgentBuilder, texts: GoalTexts) {
    builder.plugin(move |names| Ok(Box::new(GoalPlugin::new(names, texts.clone())?)));
}

pub(crate) struct GoalPlugin {
    settings: Key<SettingsService>,
    parts: Key<PartRegistry>,
    texts: Arc<GoalTexts>,
}

impl GoalPlugin {
    fn new(names: &Names<'_>, texts: GoalTexts) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            parts: names.key()?,
            texts: Arc::new(texts),
        })
    }
}

impl DynPlugin for GoalPlugin {
    fn name(&self) -> &str {
        SOURCE
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.require_at(&self.settings)?.register_declared(
                ctx,
                SourceDeclaration {
                    source: SOURCE.into(),
                    user_file: "goal.json".into(),
                    project_file: None,
                    schema: json!({
                        "type":"object",
                        "additionalProperties":false,
                        "properties":{
                            "maxContinuations":{
                                "type":"integer","minimum":1,"maximum":1000,
                                "description":"Maximum automatic continuation turns after one turn end. Omit for no limit."
                            }
                        }
                    }),
                    user_only: vec![],
                },
            )?;
            let parts = ctx.require_at(&self.parts)?;
            // One table for this plugin's parts: a change call reaches the
            // loaded branch's Owner, which composes the record (R5), without
            // a service (ADR 002 D3).
            let changes = branch::Changes::default();
            let session_changes = changes.clone();
            parts.register(ctx, PartScope::Session, SESSION_PART, move |mount| {
                Ok(Some(Box::new(session::GoalSession::new(
                    mount.names,
                    session_changes.clone(),
                )?) as Box<dyn DynPlugin>))
            })?;
            let branch_changes = changes.clone();
            let branch = self.texts.clone();
            parts.register(ctx, PartScope::Branch, BRANCH_PART, move |mount| {
                Ok(Some(Box::new(branch::GoalBranch::new(
                    mount.names,
                    branch.clone(),
                    branch_changes.clone(),
                )?) as Box<dyn DynPlugin>))
            })?;
            Ok(Effect::Done)
        })
    }
}
