//! `dimcode plugin`: PluginManager's `plugin.*` methods (PluginManager §6).

use crate::model::session::PluginAction;
use crate::{Result, client::Client, sanitize::safe};
use dim_client::Message;
use serde_json::{Value, json};

/// Parses `plugin ARGS...` into a method and its params. Relative paths are
/// resolved against the working directory here, where the user typed them.
pub fn parse(
    args: &mut dyn Iterator<Item = String>,
) -> std::result::Result<(String, Value), String> {
    const USAGE: &str = "Use plugin list | install <path|git-url> [--ref R] [--id ID] [--no-enable] | enable <name> [--use ID|builtin|default] | disable <name> | reload <name> | update <id> | remove <id>";
    let command = args.next().ok_or(USAGE)?;
    let rest: Vec<String> = args.collect();
    let one = |field: &str| match rest.as_slice() {
        [value] if !value.starts_with("--") => Ok(json!({ field: value })),
        _ => Err(USAGE.to_owned()),
    };
    let params = match command.as_str() {
        "list" if rest.is_empty() => json!({}),
        "install" => {
            let mut source = None;
            let (mut reference, mut id, mut enable) = (None, None, true);
            let mut rest = rest.into_iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--ref" => reference = Some(rest.next().ok_or(USAGE)?),
                    "--id" => id = Some(rest.next().ok_or(USAGE)?),
                    "--no-enable" => enable = false,
                    value if value.starts_with("--") || source.is_some() => {
                        return Err(USAGE.into());
                    }
                    value => source = Some(value.to_owned()),
                }
            }
            let source = source.ok_or(USAGE)?;
            let source = if source.contains("://") {
                json!({"git": source, "ref": reference})
            } else if reference.is_some() {
                return Err("--ref applies to a Git URL".into());
            } else {
                let path = std::env::current_dir()
                    .map_err(|error| error.to_string())?
                    .join(&source);
                json!({"path": path})
            };
            let mut params = json!({"source": source, "enable": enable});
            if let Some(id) = id {
                params["id"] = json!(id);
            }
            params
        }
        "enable" => match rest.as_slice() {
            [name] if !name.starts_with("--") => json!({"name": name}),
            [name, flag, use_] if flag == "--use" => json!({"name": name, "use": use_}),
            _ => return Err(USAGE.into()),
        },
        "disable" | "reload" => one("name")?,
        "update" | "remove" => one("id")?,
        _ => return Err(USAGE.into()),
    };
    Ok((format!("plugin.{command}"), strip_nulls(params)))
}

/// The method and params of a `/plugins` panel change.
pub fn call(action: &PluginAction) -> (&'static str, Value) {
    match action {
        PluginAction::Enable(name) => ("plugin.enable", json!({"name": name})),
        PluginAction::Disable(name) => ("plugin.disable", json!({"name": name})),
        PluginAction::Reload(name) => ("plugin.reload", json!({"name": name})),
        PluginAction::Install(path) => ("plugin.install", json!({"source": {"path": path}})),
    }
}

fn strip_nulls(mut value: Value) -> Value {
    if let Value::Object(map) = &mut value {
        map.retain(|_, field| !field.is_null());
        for field in map.values_mut() {
            *field = strip_nulls(field.take());
        }
    }
    value
}

/// Calls the method, showing install and update phases, and prints the
/// result.
pub async fn run(client: &mut Client, method: &str, params: Value) -> Result<i32> {
    let id = client.send(method, params).await?;
    let result = loop {
        match client.next().await? {
            Message::Response {
                id: received,
                result,
            } if received == id => break result?,
            Message::Notice {
                method: notice,
                params,
            } if notice == "management.progress" && params["id"].as_u64() == Some(id) => {
                if let Some(phase) = params["data"]["phase"].as_str() {
                    eprintln!("{}...", safe(phase));
                }
            }
            _ => {}
        }
    };
    if method == "plugin.list" {
        for item in result.as_array().into_iter().flatten() {
            println!("{}", list_line(item));
        }
    } else {
        println!("{}", summary(method, &result));
    }
    Ok(0)
}

fn text(value: &Value) -> String {
    safe(value.as_str().unwrap_or("-")).to_string()
}

/// One name: what it uses, its status and the other candidates.
fn list_line(item: &Value) -> String {
    let mut line = format!("{}  use={}", text(&item["name"]), text(&item["use"]));
    if let Some(status) = item["status"].as_str() {
        line.push_str(&format!("  {}", safe(status)));
    }
    if item["locked"] == true {
        line.push_str("  locked");
    }
    let candidates: Vec<String> = item["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|candidate| text(&candidate["id"]))
        .collect();
    line.push_str(&format!("  candidates={}", candidates.join(",")));
    if let Some(error) = item["error"].as_str() {
        line.push_str(&format!("\n    error: {}", safe(error)));
    }
    if let Some(waiting) = item["waitingFor"].as_array().filter(|w| !w.is_empty()) {
        let names: Vec<String> = waiting.iter().map(text).collect();
        line.push_str(&format!("\n    waiting for: {}", names.join(", ")));
    }
    line
}

fn summary(method: &str, result: &Value) -> String {
    let mut parts = vec![method.trim_start_matches("plugin.").to_owned()];
    for (field, value) in result.as_object().into_iter().flatten() {
        let value = match value {
            Value::String(text) => safe(text).to_string(),
            other => safe(&other.to_string()).to_string(),
        };
        parts.push(format!("{field}={value}"));
    }
    if parts.len() == 1 {
        parts.push("done".into());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::{Value, json};

    fn parsed(args: &[&str]) -> Result<(String, Value), String> {
        parse(&mut args.iter().map(|arg| arg.to_string()))
    }

    /// Each form in the usage string maps to its `plugin.*` method and params.
    /// Each `/plugins` panel change calls its `plugin.*` method; install
    /// takes the plugin as it is (enable defaults to on).
    #[test]
    fn panel_changes_call_their_plugin_methods() {
        use super::call;
        use crate::model::session::PluginAction;
        let cases = [
            (
                PluginAction::Enable("web".into()),
                "plugin.enable",
                json!({"name": "web"}),
            ),
            (
                PluginAction::Disable("web".into()),
                "plugin.disable",
                json!({"name": "web"}),
            ),
            (
                PluginAction::Reload("web".into()),
                "plugin.reload",
                json!({"name": "web"}),
            ),
            (
                PluginAction::Install("/p/my-tool".into()),
                "plugin.install",
                json!({"source": {"path": "/p/my-tool"}}),
            ),
        ];
        for (action, method, params) in cases {
            assert_eq!(call(&action), (method, params), "{action:?}");
        }
    }

    #[test]
    fn usage_forms_map_to_plugin_methods() {
        let cwd = std::env::current_dir().unwrap();
        let cases = [
            (vec!["list"], "plugin.list", json!({})),
            (
                vec!["install", "x", "--no-enable", "--id", "y"],
                "plugin.install",
                json!({"source": {"path": cwd.join("x")}, "enable": false, "id": "y"}),
            ),
            (
                vec!["install", "https://h/r.git", "--ref", "v1"],
                "plugin.install",
                json!({"source": {"git": "https://h/r.git", "ref": "v1"}, "enable": true}),
            ),
            (
                vec!["install", "file:///r.git"],
                "plugin.install",
                json!({"source": {"git": "file:///r.git"}, "enable": true}),
            ),
            (vec!["enable", "n"], "plugin.enable", json!({"name": "n"})),
            (
                vec!["enable", "n", "--use", "builtin"],
                "plugin.enable",
                json!({"name": "n", "use": "builtin"}),
            ),
            (vec!["disable", "n"], "plugin.disable", json!({"name": "n"})),
            (vec!["reload", "n"], "plugin.reload", json!({"name": "n"})),
            (vec!["update", "i"], "plugin.update", json!({"id": "i"})),
            (vec!["remove", "i"], "plugin.remove", json!({"id": "i"})),
        ];
        for (args, method, params) in cases {
            assert_eq!(parsed(&args), Ok((method.to_owned(), params)), "{args:?}");
        }
        for args in [
            vec!["list", "x"],
            vec!["install"],
            vec!["install", "x", "--ref", "v1"],
            vec!["enable", "n", "--use"],
            vec!["remove"],
            vec!["frobnicate"],
        ] {
            assert!(parsed(&args).is_err(), "{args:?}");
        }
    }
}
