//! The `recall` tool: the branch's history told to the model as an account of
//! what happened. Design: `docs/design/plugins/Recall.md` §5 (tool contract);
//! the account is the narrative projection of §3, told in the second
//! person ([`crate::narrative`]). This part's page is
//! `docs/architecture/rust-agent/plugins/Recall.md`.
use crate::{
    host::{HostError, HostFuture, ToolExecutionContext},
    narrative::{self, ABOUT, Person, Query},
    timeline_tree::TreeCommands,
    tool_registry::{ToolConcurrency, ToolContribution, ToolHandler, ToolRegistry, definition},
};
use dim_agent_runtime::{AgentBuilder, Feature, Names};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The projection behind `recall`, for flow tests that answer recorded
/// histories. Feature `testing` only; production builds have none.
#[cfg(feature = "testing")]
pub use crate::narrative::testing;

pub const RECALL_TOOL: &str = "recall";

/// The Recall plugin: the Session tool.
pub fn feature() -> Feature {
    Feature::new("Recall").session("Recall", |names| Ok(Box::new(RecallPlugin::new(names)?)))
}

/// Installs Recall in the profile, beside `wait`.
pub fn install(builder: &mut AgentBuilder) {
    builder.require_feature(feature());
}

fn recall_definition() -> ToolDefinition {
    let mut definition = definition(
        RECALL_TOOL,
        "Recall what happened earlier in this conversation, including what a compaction summarized: what the user said, what you did and what came back. With no arguments it lists past turns; open a turn or anything in it by its id, or search with about. A shortened input that ends in [input <id>] is read in full by opening that id.",
        json!({
            "type": "object",
            "properties": {
                "about": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": ABOUT,
                    "description": "Words to look for; all must appear, in any order, ignoring case."
                },
                "tool": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Only calls to this tool."
                },
                "before": {"type": "string", "description": "An id; show what came before it."},
                "open": {
                    "type": "string",
                    "description": "An id; show that turn, or that thing in full."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "With open: continue from here, a line of a turn or a character of one thing."
                }
            },
            "additionalProperties": false
        }),
    );
    definition.source = ToolSource::Extension;
    definition.risk = Some("session".into());
    definition
}

pub struct RecallPlugin {
    tree: Key<TreeCommands>,
    tools: Key<ToolRegistry>,
}

impl RecallPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            tree: names.key()?,
            tools: names.key()?,
        })
    }
}

impl DynPlugin for RecallPlugin {
    fn name(&self) -> &str {
        "Recall"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let tools = ctx.require_at(&self.tools)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: recall_definition(),
                    handler: Arc::new(RecallTool(tree)),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

struct RecallTool(Arc<TreeCommands>);

impl RecallTool {
    async fn run(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        let query = match Query::parse(RECALL_TOOL, &call.input) {
            Ok(query) => query,
            Err(message) => return Ok(ToolResult::error(message)),
        };
        narrative::read(
            &self.0,
            &context.branch_id,
            Some(&context.turn_id),
            Person::Second,
            &query,
            &cancel,
        )
        .await
    }
}

impl ToolHandler for RecallTool {
    /// Recall reads this branch's Timeline and changes nothing.
    fn concurrency(&self, _: &ToolCall) -> ToolConcurrency {
        ToolConcurrency::Concurrent
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(self.run(call, context, cancel))
    }
}
