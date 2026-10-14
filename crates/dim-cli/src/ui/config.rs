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

fn path(home: &Path) -> PathBuf {
    home.join("config.json")
}
