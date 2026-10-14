//! Input history: the last submitted drafts of one working directory, kept in
//! `<home>/input-history.json` (`{"version":1,"buckets":{cwd:[...]}}`, at most
//! ten entries per directory, a repeated entry moves to the end).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "input-history.json";
const MAX_ENTRIES: usize = 10;

#[derive(Serialize, Deserialize, Default)]
struct Store {
    version: u32,
    buckets: BTreeMap<String, Vec<String>>,
}

fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

fn read(home: &Path) -> Store {
    let Ok(raw) = std::fs::read_to_string(path(home)) else {
        return Store::default();
    };
    match serde_json::from_str::<Store>(&raw) {
        Ok(store) if store.version == 1 => Store {
            version: 1,
            buckets: store
                .buckets
                .into_iter()
                .map(|(cwd, list)| {
                    let list: Vec<String> = list.into_iter().filter(|e| !e.is_empty()).collect();
                    let skip = list.len().saturating_sub(MAX_ENTRIES);
                    (cwd, list.into_iter().skip(skip).collect())
                })
                .filter(|(_, list): &(String, Vec<String>)| !list.is_empty())
                .collect(),
        },
        _ => Store::default(),
    }
}

pub fn load(home: &Path, cwd: &str) -> Vec<String> {
    let key = cwd.trim();
    if key.is_empty() {
        return Vec::new();
    }
    read(home).buckets.remove(key).unwrap_or_default()
}

/// Appends `entry` for `cwd`, atomically (write a temp file, rename).
pub fn append(home: &Path, cwd: &str, entry: &str) -> std::io::Result<()> {
    let key = cwd.trim();
    if key.is_empty() || entry.is_empty() {
        return Ok(());
    }
    let mut store = read(home);
    store.version = 1;
    let list = store.buckets.entry(key.to_owned()).or_default();
    list.retain(|existing| existing != entry);
    list.push(entry.to_owned());
    let skip = list.len().saturating_sub(MAX_ENTRIES);
    list.drain(..skip);
    std::fs::create_dir_all(home)?;
    let target = path(home);
    let temp = target.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec(&store)?)?;
    std::fs::rename(&temp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_dedupes_caps_and_isolates_directories() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..12 {
            append(dir.path(), "/work/a", &format!("msg {n}")).unwrap();
        }
        append(dir.path(), "/work/a", "msg 5").unwrap();
        append(dir.path(), "/work/b", "other").unwrap();
        let a = load(dir.path(), "/work/a");
        assert_eq!(a.len(), 10);
        assert_eq!(a.last().map(String::as_str), Some("msg 5"));
        assert_eq!(a.iter().filter(|e| *e == "msg 5").count(), 1);
        assert_eq!(load(dir.path(), "/work/b"), vec!["other"]);
        assert!(load(dir.path(), "/missing").is_empty());
    }

    #[test]
    fn a_corrupt_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "not json").unwrap();
        assert!(load(dir.path(), "/x").is_empty());
        append(dir.path(), "/x", "ok").unwrap();
        assert_eq!(load(dir.path(), "/x"), vec!["ok"]);
    }
}
