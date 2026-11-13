//! The committed `package-lock.json` files of the runtime's plugins.
//!
//! A plugin's tree is installed with `npm ci --ignore-scripts`, so the lock is
//! what ships: every entry must carry an `integrity` and no install script.
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Every plugin directory that has a lock, sorted.
fn locks(root: &Path) -> Result<Vec<PathBuf>> {
    let directory = root.join("runtime/plugins");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        let lock = path.join("package-lock.json");
        if path.is_dir() && lock.is_file() {
            found.push(lock);
        }
    }
    found.sort();
    Ok(found)
}

pub fn check(root: &Path) -> Result<()> {
    let locks = locks(root)?;
    if locks.is_empty() {
        return Err(format!("{}: no plugin lock", root.join("runtime/plugins").display()).into());
    }
    let mut errors = Vec::new();
    for path in &locks {
        let lock: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let Some(entries) = lock["packages"].as_object() else {
            errors.push(format!("{}: no packages member", path.display()));
            continue;
        };
        for (name, entry) in entries {
            if name.is_empty() {
                continue;
            }
            if !entry["integrity"].is_string() {
                errors.push(format!("{}: {name} has no integrity", path.display()));
            }
            if entry.get("hasInstallScript").is_some() {
                errors.push(format!("{}: {name} runs an install script", path.display()));
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n").into());
    }
    println!("plugin locks passed");
    Ok(())
}
