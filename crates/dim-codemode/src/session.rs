use crate::{CODEMODE_TOOL, runs::CodeModeRuns};
use dim_agent_capabilities::{
    host::{HostError, HostFuture, ToolExecutionContext},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
};
use dim_agent_runtime::{Names, SessionIdentity};
use dim_agent_types::{ToolResult, ToolSource};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const DESCRIPTION: &str = "Run a raw JavaScript async function body in a fresh context. Top-level await and return are supported.

tools.list() -> [{name, description}]
tools.describe(name) -> full declaration | undefined
tools.call(name, args) -> the whole tool result
  args is a JSON object.
  {content, structuredContent?, isError?, and other optional fields}: content is a string or content blocks, structuredContent is the tool's structured data; neither replaces the other.
  Return content unless you need the structured fields.
  A failed tool throws ToolCallError with code, message and optional data.
  Only the request's tools are callable; codemode is excluded.

No Node, filesystem, network, timers or console. The only host surface is `tools`.
Return a JSON-serializable value; undefined becomes null. Do not JSON.stringify.
The returned value becomes the outer result content, serialized as JSON.
Child results never enter the model context.";

pub struct CodeModeSessionPlugin {
    runs: Key<CodeModeRuns>,
    tools: Key<ToolRegistry>,
    identity: Key<SessionIdentity>,
}

impl CodeModeSessionPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            runs: names.key()?,
            tools: names.key()?,
            identity: names.key()?,
        })
    }
}

impl DynPlugin for CodeModeSessionPlugin {
    fn name(&self) -> &str {
        "CodeModeSession"
    }

    fn injects(&self) -> Vec<TypeKey> {
        vec![self.runs.erase(), self.tools.erase(), self.identity.erase()]
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let runs = ctx.require_at(&self.runs)?;
            let tools = ctx.require_at(&self.tools)?;
            let identity = ctx.require_at(&self.identity)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: codemode_definition(),
                    handler: Arc::new(CodeModeTool {
                        runs,
                        session_id: identity.session_id.clone(),
                    }),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

fn codemode_definition() -> dim_agent_types::ToolDefinition {
    let mut definition = definition(
        CODEMODE_TOOL,
        DESCRIPTION,
        json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Raw JavaScript async function body. Return a JSON-serializable value."
                }
            },
            "required": ["code"],
            "additionalProperties": false
        }),
    );
    definition.source = ToolSource::Extension;
    definition.risk = Some("session".into());
    definition
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodeModeInput {
    code: String,
}

struct CodeModeTool {
    runs: Arc<CodeModeRuns>,
    session_id: String,
}

impl ToolHandler for CodeModeTool {
    fn execute(
        &self,
        call: dim_agent_types::ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let input: CodeModeInput =
                serde_json::from_value(serde_json::Value::Object(call.input)).map_err(|error| {
                    HostError::failed(format!("Invalid codemode arguments: {error}"))
                })?;
            let runner = self
                .runs
                .get(&self.session_id, &context.branch_id)
                .ok_or_else(|| {
                    HostError::failed("Code Mode is not available on the calling branch")
                })?;
            runner.run(call.id, input.code, context, cancel).await
        })
    }
}
