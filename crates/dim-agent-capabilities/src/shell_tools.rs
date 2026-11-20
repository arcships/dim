//! Session-level tool host for the persistent shell: the `execute` tool.
//!
//! Defines the tool, resolves the calling branch's ShellSession, and
//! dispatches execution.

use crate::{
    host::{ToolDriver, ToolExecutionContext, ToolHost},
    host_platform::{HostInfo, HostPlatform, ISOLATION_SUMMARY},
    shell_session::{CallContext, ShellSession},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{AgentBuilder, Names};
use dim_agent_types::{ToolCall, ToolResult};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::sync::Arc;

/// The `execute` tool definition.
fn execute_definition(platform: &HostInfo) -> dim_agent_types::ToolDefinition {
    let mut env_line = format!(
        "Environment: {}/{}",
        platform.os.node_platform(),
        platform.arch
    );
    if let Some(isolation) = ISOLATION_SUMMARY {
        env_line = format!("{env_line}\n{isolation}");
    }
    crate::tool_registry::definition(
        "execute",
        &format!(
            "{env_line}\n\nRun commands in a persistent shell. Working directory, environment variables, shell variables and background jobs persist across calls.\n\n- runInBackground returns a taskId and an output file path at once; the session wakes when the command completes.\n- If a command is still running at the timeout, the call returns its output so far with a taskId and an output file path. The command keeps running and occupies the shell; the session wakes when it completes.\n- While the shell is occupied, a call does not run its command: it waits up to timeoutMs for the running command and returns its new output.\n- In PowerShell, background commands run in a separate process: environment variables and the working directory are inherited, PowerShell variables and functions are not."
        ),
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute." },
                "runInBackground": { "type": "boolean", "description": "Run in background without waiting. Returns a taskId and an output file path; the session wakes when the command completes." },
                "timeoutMs": { "type": "number", "description": "Optional milliseconds to wait before returning; a command still running keeps running. Default 300000." }
            },
            "required": ["command"]
        }),
    )
}

/// The tool driver.
struct ShellToolsDriver {
    definition: dim_agent_types::ToolDefinition,
    tree: Arc<TreeCommands>,
}

impl ToolDriver for ShellToolsDriver {
    fn definitions(&self) -> Vec<dim_agent_types::ToolDefinition> {
        vec![self.definition.clone()]
    }

    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: tokio_util::sync::CancellationToken,
    ) -> crate::host::HostFuture<'_, ToolResult> {
        let tree = self.tree.clone();
        Box::pin(async move {
            let branch = tree
                .get(&context.branch_id)
                .map_err(|e| crate::host::HostError::failed(e.to_string()))?;
            let session = branch
                .service::<ShellSession>()
                .map_err(|e| crate::host::HostError::failed(e.to_string()))?;
            let input = call.input;
            let command = input
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let run_in_background = input
                .get("runInBackground")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let timeout_ms = input
                .get("timeoutMs")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(300_000);
            let call_context = CallContext {
                turn_id: context.turn_id,
                request_id: context.request_id,
                call_id: call.id,
            };
            session
                .run(command, call_context, timeout_ms, run_in_background, cancel)
                .await
                .map_err(|e| crate::host::HostError::failed(e.to_string()))
        })
    }
}

/// Installs the shell tools into a Session.
pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(
        dim_agent_runtime::Feature::new("ShellTools").session("ShellTools", |names| {
            Ok(Box::new(ShellToolsPlugin::new(names)?))
        }),
    );
}

/// The plugin.
pub struct ShellToolsPlugin {
    host: Key<ToolHost>,
    platform: Key<HostPlatform>,
    tree: Key<TreeCommands>,
}

impl ShellToolsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            host: names.key()?,
            platform: names.key()?,
            tree: names.key()?,
        })
    }
}

impl DynPlugin for ShellToolsPlugin {
    fn name(&self) -> &str {
        "ShellTools"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.platform.erase(), self.tree.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let platform = ctx.require_at(&self.platform)?;
            let tree = ctx.require_at(&self.tree)?;
            let mut definition = execute_definition(&platform.info().await);
            definition.risk = Some("execute_process".into());
            let driver = Arc::new(ShellToolsDriver { definition, tree });
            ctx.provide_at(self.host.clone(), Arc::new(ToolHost(driver)))?;
            Ok(Effect::Done)
        })
    }
}
