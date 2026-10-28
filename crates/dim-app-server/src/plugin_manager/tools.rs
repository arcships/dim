//! `PluginTools` (PluginManager §6): the Session part that gives the agent
//! PluginManager's operations as tools. Each call goes to PluginManager;
//! Permission decides it like any tool call.
use super::Manager;
use dim_agent_capabilities::{
    host::{HostFuture, ToolExecutionContext},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
};
use dim_agent_runtime::Names;
use dim_agent_types::{ToolCall, ToolResult, ToolSource};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct PluginTools {
    tools: Key<ToolRegistry>,
    manager: Manager,
}

impl PluginTools {
    pub fn new(names: &Names<'_>, manager: Manager) -> rutis::Result<Self> {
        Ok(Self {
            tools: names.key()?,
            manager,
        })
    }
}

/// Each tool: its method, description and input schema (A.1 params).
fn tools() -> [(&'static str, &'static str, Value); 7] {
    let name = json!({"type": "object", "required": ["name"], "additionalProperties": false,
        "properties": {"name": {"type": "string", "description": "The plugin name"}}});
    let id = json!({"type": "object", "required": ["id"], "additionalProperties": false,
        "properties": {"id": {"type": "string", "description": "The installed plugin id"}}});
    [
        (
            "plugin.list",
            "List plugins by name: the candidates of each name, the one in use, whether it is off or locked, and its load status.",
            json!({"type": "object", "additionalProperties": false, "properties": {}}),
        ),
        (
            "plugin.install",
            "Install a plugin from a local directory or a Git URL (https:// or ssh://) and turn it on unless enable is false. The plugin directory has a dim-plugin.json manifest.",
            json!({"type": "object", "required": ["source"], "additionalProperties": false, "properties": {
                "source": {"type": "object", "oneOf": [
                    {"type": "object", "required": ["path"], "additionalProperties": false,
                        "properties": {"path": {"type": "string", "description": "Absolute path of the plugin directory"}}},
                    {"type": "object", "required": ["git"], "additionalProperties": false,
                        "properties": {"git": {"type": "string"}, "ref": {"type": "string"}}}
                ]},
                "id": {"type": "string", "description": "Install under this id instead of the lowercased manifest name"},
                "enable": {"type": "boolean"}
            }}),
        ),
        (
            "plugin.enable",
            "Turn a plugin name on, optionally switching which plugin it uses: an installed id, \"builtin\" or \"default\".",
            json!({"type": "object", "required": ["name"], "additionalProperties": false, "properties": {
                "name": {"type": "string"}, "use": {"type": "string"}
            }}),
        ),
        ("plugin.disable", "Turn a plugin name off.", name.clone()),
        (
            "plugin.reload",
            "Reload the plugin a name uses after its files changed: its entry module is imported again and its tools are replaced.",
            name,
        ),
        (
            "plugin.update",
            "Fetch the latest commit of a Git plugin (or ref) and load it.",
            json!({"type": "object", "required": ["id"], "additionalProperties": false, "properties": {
                "id": {"type": "string"}, "ref": {"type": "string"}
            }}),
        ),
        ("plugin.remove", "Remove an installed plugin.", id),
    ]
}

struct Call {
    method: &'static str,
    manager: Manager,
}

impl ToolHandler for Call {
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            Ok(
                match self
                    .manager
                    .call(self.method, Value::Object(call.input), None)
                    .await
                {
                    Ok(reply) => ToolResult::text(reply.to_string()),
                    Err(error) => ToolResult::error(error.to_string()),
                },
            )
        })
    }
}

impl DynPlugin for PluginTools {
    fn name(&self) -> &str {
        "PluginTools"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.tools)?;
            for (method, description, schema) in tools() {
                let mut definition = definition(&method.replace('.', "_"), description, schema);
                definition.source = ToolSource::Extension;
                definition.risk = Some(
                    if method == "plugin.list" {
                        "read_filesystem"
                    } else {
                        "execute_process"
                    }
                    .into(),
                );
                registry.register(
                    ctx,
                    ToolContribution {
                        definition,
                        handler: Arc::new(Call {
                            method,
                            manager: self.manager.clone(),
                        }),
                    },
                )?;
            }
            Ok(Effect::Done)
        })
    }
}
