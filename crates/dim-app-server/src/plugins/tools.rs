//! The agent's `plugin_*` tools (PluginManager §8.3): the same operations as
//! `plugin.*`, as ordinary tool calls that Permission decides.
use super::operations::{Failure, Operations, Source};
use dim_agent_capabilities::host::{HostFuture, ToolExecutionContext};
use dim_agent_capabilities::tool_registry::{
    ToolContribution, ToolHandler, ToolRegistry, definition,
};
use dim_agent_runtime::Names;
use dim_agent_types::{ToolCall, ToolResult, ToolSource};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The Session part PluginManager registers: it registers the tools.
pub(crate) struct PluginTools {
    tools: Key<ToolRegistry>,
    operations: Arc<Operations>,
}
impl PluginTools {
    pub fn new(names: &Names<'_>, operations: Arc<Operations>) -> rutis::Result<Self> {
        Ok(Self {
            tools: names.key()?,
            operations,
        })
    }
}

#[derive(Clone, Copy)]
enum Tool {
    List,
    Install,
    Reload,
    SetEnabled,
    Update,
    Remove,
}

const ID: &str = "The plugin id, as plugin_list shows it.";

impl Tool {
    const ALL: [Tool; 6] = [
        Tool::List,
        Tool::Install,
        Tool::Reload,
        Tool::SetEnabled,
        Tool::Update,
        Tool::Remove,
    ];
    fn definition(self) -> dim_agent_types::ToolDefinition {
        let id = json!({"type": "string", "description": ID});
        let (name, description, schema, risk) = match self {
            Tool::List => (
                "plugin_list",
                "List the host's plugins: built-in, default and installed, with whether each is enabled, locked, and its status.",
                json!({"type": "object", "properties": {}}),
                "read_filesystem",
            ),
            Tool::Install => (
                "plugin_install",
                "Install a plugin directory from an absolute local path or a Git URL. It runs at once. A local path is used in place, so editing it and calling plugin_reload brings in the change.",
                json!({"type": "object", "properties": {
                    "path": {"type": "string", "description": "Absolute path of a local plugin directory."},
                    "git": {"type": "string", "description": "Git URL of a plugin repository."},
                    "ref": {"type": "string", "description": "Branch or tag to clone (Git only)."},
                    "id": {"type": "string", "description": "Plugin id; defaults to the directory or repository name."}
                }}),
                "execute_process",
            ),
            Tool::Reload => (
                "plugin_reload",
                "Reload a plugin after editing it: its entry module is imported again and its content read again, without restarting the host.",
                json!({"type": "object", "properties": {"id": id}, "required": ["id"]}),
                "execute_process",
            ),
            Tool::SetEnabled => (
                "plugin_set_enabled",
                "Enable or disable a plugin. A disabled plugin stays installed.",
                json!({"type": "object", "properties": {"id": id, "enabled": {"type": "boolean"}}, "required": ["id", "enabled"]}),
                "execute_process",
            ),
            Tool::Update => (
                "plugin_update",
                "Update a Git plugin to the newest commit of its branch, then reload it.",
                json!({"type": "object", "properties": {"id": id}, "required": ["id"]}),
                "execute_process",
            ),
            Tool::Remove => (
                "plugin_remove",
                "Remove an installed plugin. Built-in and default plugins can only be disabled.",
                json!({"type": "object", "properties": {"id": id}, "required": ["id"]}),
                "execute_process",
            ),
        };
        let mut definition = definition(name, description, schema);
        definition.source = ToolSource::Extension;
        definition.risk = Some(risk.into());
        definition
    }
}

struct Handler {
    tool: Tool,
    operations: Arc<Operations>,
}
impl Handler {
    async fn run(&self, call: ToolCall) -> Result<String, Failure> {
        let input = Value::Object(call.input);
        let text = |key: &str| input[key].as_str().map(str::to_owned);
        let id = || {
            text("id").ok_or_else(|| Failure {
                code: "invalid_params",
                message: "id is required".into(),
            })
        };
        let operations = &self.operations;
        let result = match self.tool {
            Tool::List => {
                return Ok(super::manager::list(&operations.loader)
                    .iter()
                    .map(|plugin| {
                        let mut line = format!(
                            "{} ({}): {}{}",
                            plugin.id,
                            plugin.kind,
                            plugin.status,
                            if plugin.locked { ", locked" } else { "" }
                        );
                        if let Some(error) = &plugin.error {
                            line.push_str(&format!(" — {error}"));
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
            Tool::Install => {
                let source = match (text("path"), text("git")) {
                    (Some(path), None) => Source::Path { path: path.into() },
                    (None, Some(git)) => Source::Git {
                        git,
                        reference: text("ref"),
                    },
                    _ => {
                        return Err(Failure {
                            code: "invalid_params",
                            message: "give exactly one of path or git".into(),
                        });
                    }
                };
                operations.install(source, text("id")).await?
            }
            Tool::Reload => operations.reload(&id()?).await?,
            Tool::SetEnabled => {
                let enabled = input["enabled"].as_bool().ok_or_else(|| Failure {
                    code: "invalid_params",
                    message: "enabled is required".into(),
                })?;
                operations.set_enabled(&id()?, enabled).await?
            }
            Tool::Update => operations.update(&id()?).await?,
            Tool::Remove => operations.remove(&id()?).await?,
        };
        let id = result["id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| text("id"))
            .unwrap_or_default();
        let status = super::manager::list(&operations.loader)
            .into_iter()
            .find(|plugin| plugin.id == id)
            .map_or_else(
                || "removed".to_owned(),
                |plugin| match plugin.error {
                    Some(error) => format!("{} ({error})", plugin.status),
                    None => plugin.status.to_owned(),
                },
            );
        let mut text = format!("Plugin {id}: {status}.");
        if result["restartRequired"] == true {
            text.push_str(" Some of its names take effect after the App Server restarts.");
        }
        if let Some(error) = result["cleanupError"].as_str() {
            text.push_str(&format!(" Its files could not be deleted: {error}."));
        }
        Ok(text)
    }
}
impl ToolHandler for Handler {
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            Ok(match self.run(call).await {
                Ok(text) => ToolResult::text(text),
                Err(failure) => ToolResult::error(format!("{}: {}", failure.code, failure.message)),
            })
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
            let tools = ctx.require_at(&self.tools)?;
            for tool in Tool::ALL {
                tools.register(
                    ctx,
                    ToolContribution {
                        definition: tool.definition(),
                        handler: Arc::new(Handler {
                            tool,
                            operations: self.operations.clone(),
                        }),
                    },
                )?;
            }
            Ok(Effect::Done)
        })
    }
}
