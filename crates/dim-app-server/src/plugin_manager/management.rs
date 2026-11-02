//! Credential-safe projections at PluginManager's client RPC boundary.
use super::PluginError;
use dim_agent_capabilities::methods::RpcError;
use serde_json::{Map, Value};

pub(super) fn error(error: PluginError) -> RpcError {
    let message = match error.code {
        "invalid_params" => "Invalid plugin request; check the required fields",
        "invalid_source" => {
            "Use a local directory or an https:// or ssh:// Git URL; check the source and ref"
        }
        "invalid_id" => {
            "Installation IDs must match [a-z0-9_-]{1,64} and cannot be builtin or default"
        }
        "plugin_exists" => "This installation ID already exists",
        "plugin_not_found" => "Plugin not found or disabled; refresh the catalog",
        "plugin_locked" => "This plugin is locked and cannot be disabled or replaced",
        "not_updatable" => "Local plugins cannot be updated from Git; reload them instead",
        "not_removable" => "Built-in and default plugins cannot be removed",
        "not_reloadable" => "Built-in plugins require a rebuild; replace them instead",
        "name_changed" => "Plugin name changed; install the source as a new plugin",
        "invalid_manifest" => "Invalid dim-plugin.json; check the manifest and its paths",
        "path_outside_plugins" => {
            "Plugin directory is outside the managed root or contains a symbolic link"
        }
        "acquire_failed" => {
            "Plugin acquisition failed; check Git access and filesystem permissions"
        }
        "record_unreadable" => "Plugin configuration could not be read",
        _ => "Plugin operation failed",
    };
    RpcError::service(error.code, message)
}

pub(super) fn reply(method: &str, mut value: Value) -> Value {
    if method == "plugin.list" {
        if let Value::Array(items) = &mut value {
            for item in items {
                if let Value::Object(fields) = item {
                    outcome(fields);
                    if let Some(Value::Array(candidates)) = fields.get_mut("candidates") {
                        for candidate in candidates {
                            if let Some(Value::Object(source)) = candidate.get_mut("source") {
                                git_source(source);
                            }
                        }
                    }
                }
            }
        }
    } else if let Value::Object(fields) = &mut value {
        outcome(fields);
    }
    value
}

fn outcome(fields: &mut Map<String, Value>) {
    if let Some(error) = fields.get_mut("error") {
        *error = Value::String("Plugin content or runtime failed".into());
    }
    if let Some(Value::Array(refused)) = fields.get_mut("refused") {
        for refusal in refused {
            if let Some(error) = refusal.get_mut("error") {
                *error = Value::String("Plugin content was refused".into());
            }
        }
    }
}

fn git_source(source: &mut Map<String, Value>) {
    if let Some(Value::String(git)) = source.get_mut("git") {
        *git = super::acquire::git_url_safe(git);
    }
}
