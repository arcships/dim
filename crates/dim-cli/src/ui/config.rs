//! Interface settings in `<home>/config.json`, one top-level key each; a
//! write keeps the other keys.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The value saved under `key`, if any.
pub fn read(home: &Path, key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(path(home)).ok()?;
    let mut value: Value = serde_json::from_str(&text).ok()?;
    value.get_mut(key).map(Value::take)
}

/// Saves `value` under `key`; the error text on failure.
pub fn write(home: &Path, key: &str, value: Value) -> Option<String> {
    let path = path(home);
    let mut config: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    config[key] = value;
    let text = match serde_json::to_string_pretty(&config) {
        Ok(text) => text + "\n",
        Err(error) => return Some(error.to_string()),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, text).err().map(|e| e.to_string())
}

/// `config.json` key of the default terminal UI mode.
const TERMINAL_MODE_KEY: &str = "tuiMode";

/// True when the saved default terminal UI mode is fullscreen; scrollback
/// is the default when nothing was saved.
pub fn saved_fullscreen(home: &Path) -> bool {
    read(home, TERMINAL_MODE_KEY).is_some_and(|mode| mode == "fullscreen")
}

/// Saves the default terminal UI mode; the error text on failure.
pub fn save_fullscreen(home: &Path, fullscreen: bool) -> Option<String> {
    let mode = if fullscreen {
        "fullscreen"
    } else {
        "scrollback"
    };
    write(home, TERMINAL_MODE_KEY, mode.into())
}

fn path(home: &Path) -> PathBuf {
    home.join("config.json")
}
