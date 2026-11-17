//! `dimcode plugin`: PluginManager's `plugin.*` methods (PluginManager §6).

use crate::model::session::PluginAction;
use crate::{
    Result,
    client::Client,
    sanitize::{safe, safe_line},
};
use dim_client::Message;
use serde_json::{Value, json};

/// Parses `plugin ARGS...` into a method and its params. Relative paths are
/// resolved against the working directory here, where the user typed them.
pub fn parse(
    args: &mut dyn Iterator<Item = String>,
) -> std::result::Result<(String, Value), String> {
    const USAGE: &str = "Use plugin list | install <path|git-url> [--ref R] [--id ID] [--no-enable] | install --optional <name> [--id ID] [--no-enable] | enable <name> [--use ID|builtin|default] | disable <name> | reload <name> | update <id> | remove <id>";
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
            let mut optional = None;
            let (mut reference, mut id, mut enable) = (None, None, true);
            let mut rest = rest.into_iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--ref" => reference = Some(rest.next().ok_or(USAGE)?),
                    "--id" => id = Some(rest.next().ok_or(USAGE)?),
                    "--optional" => optional = Some(rest.next().ok_or(USAGE)?),
                    "--no-enable" => enable = false,
                    value if value.starts_with("--") || source.is_some() => {
                        return Err(USAGE.into());
                    }
                    value => source = Some(value.to_owned()),
                }
            }
            let source = match (optional, source) {
                (Some(name), None) if reference.is_none() => json!({"optional": name}),
                (Some(_), None) => return Err("--ref applies to a Git URL".into()),
                (Some(_), Some(_)) => return Err(USAGE.into()),
                (None, None) => return Err(USAGE.into()),
                (None, Some(source)) if source.contains("://") => {
                    json!({"git": source, "ref": reference})
                }
                (None, Some(_)) if reference.is_some() => {
                    return Err("--ref applies to a Git URL".into());
                }
                (None, Some(source)) => {
                    let path = std::env::current_dir()
                        .map_err(|error| error.to_string())?
                        .join(&source);
                    json!({"path": path})
                }
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
        PluginAction::InstallOptional(name) => {
            ("plugin.install", json!({"source": {"optional": name}}))
        }
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
    if method == "plugin.install" {
        report_unrunnable_hooks(client, &result).await;
    }
    Ok(0)
}

/// After a successful `plugin.install`, reads `hooks.catalog` and prints
/// the installed plugin's hook handlers whose state is not `ok` — the
/// events and handler kinds Dim does not run — with one line pointing to
/// `Hooks.md` Appendix G. A daemon that does not advertise `hooks.catalog`
/// is not asked, and a read that fails reports that the handlers could not
/// be checked without failing the install.
async fn report_unrunnable_hooks(client: &mut Client, result: &Value) {
    let Some(id) = result["id"].as_str() else {
        return;
    };
    if !client.supports(&["hooks.catalog"]) {
        return;
    }
    let catalog = match read_catalog(client).await {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!(
                "The installed plugin's hook handlers could not be checked: {}",
                safe_line(&error.to_string())
            );
            return;
        }
    };
    let source = format!("plugin:{id}");
    let mut lines = Vec::new();
    for entry in catalog["hooks"].as_array().into_iter().flatten() {
        if entry["source"].as_str() == Some(source.as_str())
            && entry["state"].as_str() != Some("ok")
        {
            lines.push(format!(
                "  {} {}",
                safe_line(entry["event"].as_str().unwrap_or("-")),
                safe_line(entry["state"].as_str().unwrap_or("-"))
            ));
        }
    }
    if !lines.is_empty() {
        println!("Hook handlers that will not run:");
        for line in lines {
            println!("{line}");
        }
        println!("See Hooks.md Appendix G for the events Dim runs and the differences.");
    }
}

/// The reply to `hooks.catalog`, read past any notices.
async fn read_catalog(client: &mut Client) -> Result<Value> {
    let request = client.send("hooks.catalog", json!({})).await?;
    loop {
        match client.next().await? {
            Message::Response { id, result } if id == request => {
                return result.map_err(Into::into);
            }
            _ => {}
        }
    }
}

fn text(value: &Value) -> String {
    safe(value.as_str().unwrap_or("-")).to_string()
}

/// One name: what it uses, its status and the other candidates.
fn list_line(item: &Value) -> String {
    let mut line = format!("{}  use={}", text(&item["name"]), text(&item["use"]));
    let candidates: Vec<String> = item["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|candidate| text(&candidate["id"]))
        .collect();
    // An optional plugin the home does not have yet comes without a status:
    // say what it is, and name the command that installs it below.
    let installable = item["optional"] == true && candidates.is_empty();
    if installable {
        line.push_str("  optional, not installed");
    } else if let Some(status) = item["status"].as_str() {
        line.push_str(&format!("  {}", safe(status)));
    }
    if item["locked"] == true {
        line.push_str("  locked");
    }
    line.push_str(&format!("  candidates={}", candidates.join(",")));
    if let Some(error) = item["error"].as_str() {
        line.push_str(&format!("\n    error: {}", safe(error)));
    }
    if let Some(waiting) = item["waitingFor"].as_array().filter(|w| !w.is_empty()) {
        let names: Vec<String> = waiting.iter().map(text).collect();
        line.push_str(&format!("\n    waiting for: {}", names.join(", ")));
    }
    if installable {
        line.push_str(&format!(
            "\n    install: dimcode plugin install --optional {}",
            text(&item["name"])
        ));
    }
    if let Some(version) = item["update"]["version"].as_str() {
        line.push_str(&format!("\n    update: {}", safe(version)));
    }
    if let Some(error) = item["update"]["error"].as_str() {
        line.push_str(&format!("\n    update failed: {}", safe(error)));
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
    use super::{list_line, parse};
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
            (
                PluginAction::InstallOptional("light-ocr".into()),
                "plugin.install",
                json!({"source": {"optional": "light-ocr"}}),
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
            (
                vec!["install", "--optional", "light-ocr"],
                "plugin.install",
                json!({"source": {"optional": "light-ocr"}, "enable": true}),
            ),
            (
                vec![
                    "install",
                    "--optional",
                    "light-ocr",
                    "--id",
                    "ocr",
                    "--no-enable",
                ],
                "plugin.install",
                json!({"source": {"optional": "light-ocr"}, "enable": false, "id": "ocr"}),
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
            vec!["install", "--optional", "light-ocr", "--ref", "v1"],
            vec!["install", "x", "--optional", "light-ocr"],
            vec!["install", "--optional"],
            vec!["enable", "n", "--use"],
            vec!["remove"],
            vec!["frobnicate"],
        ] {
            assert!(parsed(&args).is_err(), "{args:?}");
        }
    }

    /// The row names the plugin and says it is not installed.
    #[test]
    fn an_uninstalled_optional_item_shows_its_install_command() {
        let item = json!({
            "name": "light-ocr",
            "use": null,
            "off": true,
            "locked": false,
            "candidates": [],
            "optional": true,
        });
        let line = list_line(&item);
        assert!(line.contains("light-ocr"), "{line}");
        assert!(line.contains("not installed"), "{line}");
        assert!(
            line.contains("dimcode plugin install --optional light-ocr"),
            "{line}"
        );
        let plain = json!({
            "name": "web",
            "use": "web",
            "status": "running",
            "locked": true,
            "candidates": [{"id": "web"}],
            "error": "last start failed",
            "waitingFor": ["node"],
        });
        let line = list_line(&plain);
        assert!(
            line.starts_with("web  use=web  running  locked  candidates=web\n"),
            "{line}"
        );
        assert!(line.contains("\n    error: last start failed"), "{line}");
        assert!(line.contains("\n    waiting for: node"), "{line}");
        assert!(!line.contains("optional"), "{line}");
        assert!(!line.contains("update"), "{line}");
    }

    /// No failed line appears when the update has no error.
    #[test]
    fn an_installed_item_with_an_update_shows_its_version() {
        let item = json!({
            "name": "light-ocr",
            "use": "light-ocr",
            "status": "running",
            "locked": false,
            "candidates": [{"id": "light-ocr"}],
            "optional": true,
            "update": {"version": "1.1.0"},
        });
        let line = list_line(&item);
        assert!(line.contains("light-ocr"), "{line}");
        assert!(line.contains("1.1.0"), "the version is named: {line}");
        assert!(
            !line.contains("update failed"),
            "no failure is invented: {line}"
        );
    }

    /// An update that failed keeps its version line and adds the error.
    #[test]
    fn a_failed_update_shows_both_lines() {
        let item = json!({
            "name": "light-ocr",
            "use": "light-ocr",
            "status": "running",
            "locked": false,
            "candidates": [{"id": "light-ocr"}],
            "update": {"version": "1.1.0", "error": "network unreachable"},
        });
        let line = list_line(&item);
        assert!(line.contains("1.1.0"), "the version is named: {line}");
        assert!(
            line.contains("network unreachable"),
            "the failure is named: {line}"
        );
    }
}
