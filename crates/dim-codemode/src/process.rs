use crate::{
    branch::CodeModeBranchPlugin, config::CodeModeConfig, runs::CodeModeRuns,
    session::CodeModeSessionPlugin,
};
use dim_agent_runtime::{Names, PartRegistry, PartScope};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::sync::Arc;

/// The process row: provides the branch runner map and registers the
/// Session tool and the branch executor.
pub struct CodeModeProcessPlugin {
    registry: Key<PartRegistry>,
    runs: Key<CodeModeRuns>,
    config: Arc<CodeModeConfig>,
}

impl CodeModeProcessPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            registry: names.key()?,
            runs: names.key()?,
            config: Arc::new(CodeModeConfig::default()),
        })
    }
}

impl DynPlugin for CodeModeProcessPlugin {
    fn name(&self) -> &str {
        "CodeMode"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            ctx.provide_at(
                self.runs.clone(),
                Arc::new(CodeModeRuns::new(self.config.max_programs)),
            )?;

            registry.register(ctx, PartScope::Session, "CodeModeSession", move |mount| {
                Ok(Some(Box::new(CodeModeSessionPlugin::new(mount.names)?)))
            })?;

            let branch_config = self.config.clone();
            registry.register(ctx, PartScope::Branch, "CodeModeBranch", move |mount| {
                Ok(Some(Box::new(CodeModeBranchPlugin::with_config(
                    mount.names,
                    branch_config.clone(),
                )?)))
            })?;
            Ok(Effect::Done)
        })
    }
}
