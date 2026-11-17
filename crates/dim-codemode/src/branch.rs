use crate::{
    config::CodeModeConfig,
    host::{NestedToolHost, ToolExecutorHost},
    interpreter::run_program,
    runs::{CodeModeRunKey, CodeModeRunner, CodeModeRuns},
};
use dim_agent_capabilities::{
    host::{HostError, ToolExecutionContext},
    tool_executor::ToolExecutor,
};
use dim_agent_runtime::{Names, TimelineIdentity};
use dim_agent_types::ToolResult;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

pub struct CodeModeBranchPlugin {
    runs: Key<CodeModeRuns>,
    executor: Key<ToolExecutor>,
    identity: Key<TimelineIdentity>,
    config: Arc<CodeModeConfig>,
}

impl CodeModeBranchPlugin {
    pub fn with_config(names: &Names<'_>, config: Arc<CodeModeConfig>) -> Result<Self, Error> {
        Ok(Self {
            runs: names.key()?,
            executor: names.key()?,
            identity: names.key()?,
            config,
        })
    }
}

impl DynPlugin for CodeModeBranchPlugin {
    fn name(&self) -> &str {
        "CodeModeBranch"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.runs.erase(),
            self.executor.erase(),
            self.identity.erase(),
        ]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let runs = ctx.require_at(&self.runs)?;
            let executor: Arc<dyn NestedToolHost> =
                Arc::new(ToolExecutorHost::new(ctx.require_at(&self.executor)?));
            let identity = ctx.require_at(&self.identity)?;
            let branch = Arc::new(CodeModeBranch {
                executor,
                config: self.config.clone(),
                runs: runs.clone(),
            });
            let key = CodeModeRunKey::new(identity.session_id.clone(), identity.branch_id.clone());
            runs.insert(key.clone(), branch);
            let cleanup = runs.clone();
            ctx.effect(move || async move {
                cleanup.remove(&key);
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

struct CodeModeBranch {
    executor: Arc<dyn NestedToolHost>,
    config: Arc<CodeModeConfig>,
    runs: Arc<CodeModeRuns>,
}

impl CodeModeRunner for CodeModeBranch {
    fn run(
        &self,
        outer_call_id: String,
        code: String,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, HostError>> + Send + '_>> {
        Box::pin(async move {
            let _program = self.runs.acquire_program(&cancel).await?;
            run_program(
                self.executor.clone(),
                self.config.clone(),
                outer_call_id,
                code,
                context,
                cancel,
            )
            .await
        })
    }
}
