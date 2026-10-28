//! `PluginTools` (PluginManager §6): the Session part that gives the agent
//! the `plugin` tool. A command goes to PluginManager's methods, or to
//! Settings for `config`; Permission decides each call like any tool call.
use super::{Manager, PluginError};
use crate::settings::{SettingsScope, Writer, holds_write_only, without_write_only};
use dim_agent_capabilities::{
    host::{HostFuture, ToolExecutionContext},
    tool_registry::{ToolContribution, ToolHandler, ToolRegistry, definition},
};
use dim_agent_runtime::{Names, SessionConfig};
use dim_agent_types::{ToolCall, ToolResult, ToolSource};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct PluginTools {
    tools: Key<ToolRegistry>,
    config: Key<SessionConfig>,
    manager: Manager,
}

impl PluginTools {
    pub fn new(names: &Names<'_>, manager: Manager) -> rutis::Result<Self> {
        Ok(Self {
            tools: names.key()?,
            config: names.key()?,
            manager,
        })
    }
}

/// Each command: its name, usage line and details.
const COMMANDS: [(&str, &str, &str); 9] = [
    (
        "list",
        "list",
        "Lists plugins by name: the candidates of each name (\"builtin\", \"default\" or an installed id), the one in use, whether it is off or locked, and its load status (running, pending, failed, stopped, off).",
    ),
    (
        "install",
        "install <path|git-url> [--ref R] [--id ID] [--no-enable]",
        "Installs the plugin directory at <path> (relative to the Session's working directory; used in place; quote a path with spaces, a backslash is taken as is) or clones <git-url> (https:// or ssh://, optionally at --ref), and turns it on unless --no-enable. The directory has a dim-plugin.json manifest. The id defaults to the manifest name lowercased; give --id when it is taken. The result has the id, name and load status.",
    ),
    (
        "enable",
        "enable <name> [--use ID|builtin|default]",
        "Turns a plugin name on. --use switches which plugin the name uses: an installed id, the built-in plugin or the default plugin of that name.",
    ),
    ("disable", "disable <name>", "Turns a plugin name off."),
    (
        "reload",
        "reload <name>",
        "Reloads the plugin a name uses after its files changed: the manifest is read again, the entry module is imported again (not the modules it imports) and its tools are replaced from the next model request.",
    ),
    (
        "update",
        "update <id>",
        "Fetches the latest commit of an installed Git plugin's ref and loads it.",
    ),
    (
        "remove",
        "remove <id>",
        "Removes an installed plugin. A name that used it falls back to its built-in or default plugin, if any.",
    ),
    (
        "config",
        "config <name> [set <key> <value>]",
        "Without set: shows the business configuration of the plugin a name uses, each Settings source it registered while running, with its user document, revision, JSON Schema and user-only fields. A member the schema marks writeOnly (a secret) is left out of the document and its JSON Pointer listed in writeOnlySet when it holds a value. With set: replaces the top-level member <key> of that document (write <source>.<key> when the plugin has several sources) with <value>, parsed as JSON or else taken as a string; quote a value with spaces. Settings checks the schema and the revision read just before; user-only fields, and members that are or contain writeOnly ones, are changed only by the user. The plugin reads the new value when it next needs it, without a restart.",
    ),
    (
        "help",
        "help [command]",
        "Shows the commands, or one command's usage and details.",
    ),
];

fn usage(command: &str) -> Option<String> {
    COMMANDS
        .iter()
        .find(|(name, ..)| *name == command)
        .map(|(_, line, detail)| format!("Usage: {line}\n{detail}"))
}

fn commands() -> String {
    let lines: Vec<_> = COMMANDS.iter().map(|(_, line, _)| *line).collect();
    format!(
        "Commands: {}. Run `help <command>` for a command's details.",
        lines.join(" | ")
    )
}

enum Command {
    Method(&'static str, Value),
    Config {
        name: String,
        set: Option<(String, Value)>,
    },
    Help(Option<String>),
}

/// Why a command was not run: one command's usage, or the command list.
#[derive(Clone, Copy)]
enum Malformed {
    Usage(&'static str),
    Unknown,
}

/// Splits a command into words: whitespace separates them and a pair of
/// single or double quotes groups one. A backslash is an ordinary character,
/// so a Windows path needs no quoting. An unclosed quote is `None`.
fn words(command: &str) -> Option<Vec<String>> {
    let (mut words, mut word, mut quote) = (Vec::new(), None::<String>, None);
    for ch in command.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => word.get_or_insert_default().push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                word.get_or_insert_default();
            }
            None if ch.is_whitespace() => words.extend(word.take()),
            None => word.get_or_insert_default().push(ch),
        }
    }
    if quote.is_some() {
        return None;
    }
    words.extend(word);
    Some(words)
}

impl Command {
    fn parse(command: &str, cwd: &str) -> Result<Self, Malformed> {
        let words = words(command).ok_or(Malformed::Unknown)?;
        let Some((first, rest)) = words.split_first() else {
            return Err(Malformed::Unknown);
        };
        let name = COMMANDS
            .iter()
            .map(|(name, ..)| *name)
            .find(|name| name == first)
            .ok_or(Malformed::Unknown)?;
        let malformed = Malformed::Usage(name);
        let plain = |value: &String| !value.starts_with("--");
        Ok(match (name, rest) {
            ("list", []) => Command::Method("plugin.list", json!({})),
            ("install", _) => {
                let (mut source, mut reference, mut id, mut enable) = (None, None, None, true);
                let mut args = rest.iter();
                while let Some(arg) = args.next() {
                    match arg.as_str() {
                        "--ref" => reference = Some(args.next().ok_or(malformed)?),
                        "--id" => id = Some(args.next().ok_or(malformed)?),
                        "--no-enable" => enable = false,
                        _ if !plain(arg) || source.is_some() => return Err(malformed),
                        _ => source = Some(arg),
                    }
                }
                let source = source.ok_or(malformed)?;
                let source = if source.contains("://") {
                    match reference {
                        Some(reference) => json!({"git": source, "ref": reference}),
                        None => json!({"git": source}),
                    }
                } else if reference.is_some() {
                    return Err(malformed);
                } else {
                    json!({"path": PathBuf::from(cwd).join(source)})
                };
                let mut params = json!({"source": source, "enable": enable});
                if let Some(id) = id {
                    params["id"] = json!(id);
                }
                Command::Method("plugin.install", params)
            }
            ("enable", [name]) if plain(name) => {
                Command::Method("plugin.enable", json!({"name": name}))
            }
            ("enable", [name, flag, used]) if plain(name) && flag == "--use" => {
                Command::Method("plugin.enable", json!({"name": name, "use": used}))
            }
            ("disable", [name]) if plain(name) => {
                Command::Method("plugin.disable", json!({"name": name}))
            }
            ("reload", [name]) if plain(name) => {
                Command::Method("plugin.reload", json!({"name": name}))
            }
            ("update", [id]) if plain(id) => Command::Method("plugin.update", json!({"id": id})),
            ("remove", [id]) if plain(id) => Command::Method("plugin.remove", json!({"id": id})),
            ("config", [name]) => Command::Config {
                name: name.clone(),
                set: None,
            },
            ("config", [name, set, key, value]) if set == "set" => Command::Config {
                name: name.clone(),
                set: Some((
                    key.clone(),
                    serde_json::from_str(value).unwrap_or_else(|_| json!(value)),
                )),
            },
            ("help", []) => Command::Help(None),
            ("help", [command]) => Command::Help(Some(command.clone())),
            _ => return Err(malformed),
        })
    }

    fn read_only(&self) -> bool {
        matches!(self, Command::Method("plugin.list", _) | Command::Help(_))
    }
}

#[derive(serde::Deserialize)]
struct Input {
    command: String,
}

struct Plugin {
    manager: Manager,
    cwd: String,
}

impl Plugin {
    fn command(&self, call: &ToolCall) -> Result<Command, Malformed> {
        let Input { command } = serde_json::from_value(Value::Object(call.input.clone()))
            .map_err(|_| Malformed::Unknown)?;
        Command::parse(&command, &self.cwd)
    }

    async fn config(
        &self,
        name: &str,
        set: Option<(String, Value)>,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        let (settings, sources) = self
            .manager
            .sources(name)
            .map_err(|error: PluginError| error.to_string())?;
        let Some(settings) = settings.filter(|_| !sources.is_empty()) else {
            return match set {
                None => Ok(json!({"name": name, "sources": []})),
                Some(_) => Err(format!(
                    "plugin {name:?} has no Settings source now; a plugin registers its sources while it runs"
                )),
            };
        };
        let Some((key, value)) = set else {
            let mut shown = Vec::new();
            for source in &sources {
                shown.push(
                    match settings
                        .read_declared(&source.source, SettingsScope::User, cancel)
                        .await
                    {
                        Ok(snapshot) => {
                            let (document, set) =
                                without_write_only(&source.schema, snapshot.document);
                            json!({
                                "source": source.source,
                                "revision": snapshot.revision,
                                "document": document,
                                "writeOnlySet": set,
                                "schema": source.schema,
                                "userOnly": source.user_only,
                            })
                        }
                        Err(error) => json!({"source": source.source, "error": error.to_string()}),
                    },
                );
            }
            return Ok(json!({"name": name, "sources": shown}));
        };
        let (source, member) = match sources.as_slice() {
            [only] => (only, key.as_str()),
            _ => key
                .split_once('.')
                .and_then(|(source, member)| {
                    sources
                        .iter()
                        .find(|declared| declared.source == source)
                        .map(|declared| (declared, member))
                })
                .ok_or_else(|| {
                    let names: Vec<_> = sources.iter().map(|s| s.source.as_str()).collect();
                    format!(
                        "plugin {name:?} has several sources ({}); write the key as <source>.<key>",
                        names.join(", ")
                    )
                })?,
        };
        if source.schema["properties"]
            .get(member)
            .is_some_and(holds_write_only)
        {
            return Err(format!(
                "settings member {member} holds a write-only value; only the user sets it"
            ));
        }
        let current = settings
            .read_declared(&source.source, SettingsScope::User, cancel)
            .await
            .map_err(|error| error.to_string())?;
        let mut document = current.document;
        document[member] = value;
        let updated = settings
            .update_declared(
                &source.source,
                SettingsScope::User,
                &current.revision,
                document,
                Writer::Owner,
                cancel,
            )
            .await
            .map_err(|error| error.to_string())?;
        let (document, set) = without_write_only(&source.schema, updated.document);
        Ok(json!({
            "source": source.source,
            "revision": updated.revision,
            "document": document,
            "writeOnlySet": set,
        }))
    }
}

impl ToolHandler for Plugin {
    fn risk(&self, call: &ToolCall) -> Option<&'static str> {
        match self.command(call) {
            Ok(command) if !command.read_only() => None,
            _ => Some("read_filesystem"),
        }
    }

    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let command = match self.command(&call) {
                Ok(command) => command,
                Err(Malformed::Usage(name)) => {
                    return Ok(ToolResult::error(usage(name).expect("a known command")));
                }
                Err(Malformed::Unknown) => return Ok(ToolResult::error(commands())),
            };
            let reply = match command {
                Command::Method(method, params) => self
                    .manager
                    .call(method, params, None)
                    .await
                    .map_err(|error| error.to_string()),
                Command::Config { name, set } => self.config(&name, set, &cancel).await,
                Command::Help(None) => return Ok(ToolResult::text(commands())),
                Command::Help(Some(name)) => {
                    return Ok(match usage(&name) {
                        Some(usage) => ToolResult::text(usage),
                        None => ToolResult::error(commands()),
                    });
                }
            };
            Ok(match reply {
                Ok(reply) => ToolResult::text(reply.to_string()),
                Err(error) => ToolResult::error(error),
            })
        })
    }
}

impl DynPlugin for PluginTools {
    fn name(&self) -> &str {
        "PluginTools"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase(), self.config.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.tools)?;
            let config = ctx.require_at(&self.config)?;
            let mut definition = definition(
                "plugin",
                &format!("Manage Dim plugins. {}", commands()),
                json!({"type": "object", "required": ["command"], "additionalProperties": false,
                    "properties": {"command": {"type": "string", "description": "A plugin command, such as `list` or `help install`"}}}),
            );
            definition.source = ToolSource::Extension;
            definition.risk = Some("persistent_configuration".into());
            registry.register(
                ctx,
                ToolContribution {
                    definition,
                    handler: Arc::new(Plugin {
                        manager: self.manager.clone(),
                        cwd: config.cwd.clone(),
                    }),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
