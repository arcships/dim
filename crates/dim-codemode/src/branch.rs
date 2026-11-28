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
use dim_agent_runtime::{BranchLifecycle, Names};
use dim_agent_types::ToolResult;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

pub struct CodeModeBranchPlugin {
    runs: Key<CodeModeRuns>,
    executor: Key<ToolExecutor>,
    lifecycle: Key<BranchLifecycle>,
    config: Arc<CodeModeConfig>,
}

impl CodeModeBranchPlugin {
    pub fn with_config(names: &Names<'_>, config: Arc<CodeModeConfig>) -> Result<Self, Error> {
        Ok(Self {
            runs: names.key()?,
            executor: names.key()?,
            lifecycle: names.key()?,
            config,
        })
    }
}

impl DynPlugin for CodeModeBranchPlugin {
    fn name(&self) -> &str {
        "CodeModeBranch"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.runs.erase(), self.executor.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let runs = ctx.require_at(&self.runs)?;
            let executor: Arc<dyn NestedToolHost> =
                Arc::new(ToolExecutorHost::new(ctx.require_at(&self.executor)?));
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let branch = Arc::new(CodeModeBranch {
                executor,
                config: self.config.clone(),
                runs: runs.clone(),
            });
            let key = CodeModeRunKey::new(
                lifecycle.session_id().clone(),
                lifecycle.branch_id().clone(),
            );
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
