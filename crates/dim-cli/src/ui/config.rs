//! Interface settings in `<home>/config.json`, one top-level key each; a
//! write keeps the other keys, under the lock that makes it atomic. A file
//! that exists but does not hold a JSON object is refused: `write` and
//! `remove` return its parse error and leave it as it was, while `read`
//! treats it as absent and the defaults apply.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::atomic;

/// The value saved under `key`, if any.
pub fn read(home: &Path, key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(path(home)).ok()?;
    let mut value: Value = serde_json::from_str(&text).ok()?;
    value.get_mut(key).map(Value::take)
}

/// Saves `value` under `key`; the error text on failure.
pub fn write(home: &Path, key: &str, value: Value) -> Option<String> {
    let file = path(home);
    atomic::update(&file, None, |current| {
        let mut config = match stored(current, &file)? {
            Some(config) => config,
            None => serde_json::json!({}),
        };
        config[key] = value;
        Ok(Some(text(config)?))
    })
    .err()
    .map(|error| error.to_string())
}

/// Removes `key`; the error text on failure.
pub fn remove(home: &Path, key: &str) -> Option<String> {
    let file = path(home);
    atomic::update(&file, None, |current| {
        let Some(mut config) = stored(current, &file)? else {
            return Ok(None);
        };
        if let Some(map) = config.as_object_mut() {
            map.remove(key);
        }
        Ok(Some(text(config)?))
    })
    .err()
    .map(|error| error.to_string())
}

/// The stored object, when the file holds one; a file that exists but does
/// not hold a JSON object is an error naming the file and what is wrong.
fn stored(current: Option<&[u8]>, file: &Path) -> std::io::Result<Option<Value>> {
    let Some(bytes) = current else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not valid JSON: {error}", file.display()),
        )
    })?;
    if !value.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} does not hold a JSON object", file.display()),
        ));
    }
    Ok(Some(value))
}

fn text(config: Value) -> std::io::Result<Vec<u8>> {
    let mut text = serde_json::to_string_pretty(&config)?;
    text.push('\n');
    Ok(text.into_bytes())
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

const PERMISSION_KEY: &str = "permissionPreset";

/// The permission preset the user last chose in `/approval` or `/approvals`,
/// the default for the sessions the terminal UI creates or resumes.
pub fn saved_permission(home: &Path) -> Option<String> {
    read(home, PERMISSION_KEY).and_then(|value| value.as_str().map(str::to_owned))
}

/// Remembers the chosen preset; the error text on failure.
pub fn save_permission(home: &Path, preset: &str) -> Option<String> {
    write(home, PERMISSION_KEY, preset.into())
}

fn path(home: &Path) -> PathBuf {
    home.join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUNDS: usize = 8;
    const WRITERS: usize = 4;

    /// Every writer keeps its own key across a write race, and the file
    /// parses, so a reader finds every key after the race.
    #[test]
    fn concurrent_writers_keep_every_key() {
        let dir = tempfile::tempdir().unwrap();
        for round in 0..ROUNDS {
            let home = dir.path().join(format!("round-{round}"));
            std::thread::scope(|scope| {
                for writer in 0..WRITERS {
                    let home = home.clone();
                    scope.spawn(move || {
                        for value in 0..8 {
                            let key = format!("key{writer}");
                            assert_eq!(write(&home, &key, value.into()), None);
                        }
                    });
                }
            });
            let text = std::fs::read_to_string(path(&home)).unwrap();
            let config: Value = serde_json::from_str(&text).unwrap();
            for writer in 0..WRITERS {
                let key = format!("key{writer}");
                assert_eq!(config[&key], 7, "round {round} lost {key}: {text}");
            }
        }
    }

    /// A write whose temp file the directory rejects leaves the old file
    /// and returns the error text.
    #[cfg(unix)]
    #[test]
    fn a_write_that_cannot_replace_the_file_keeps_the_old_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert_eq!(write(home, "kept", true.into()), None);
        let before = std::fs::read(path(home)).unwrap();
        let writable = std::fs::metadata(home).unwrap().permissions();
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = write(home, "lost", 1.into());
        std::fs::set_permissions(home, writable).unwrap();
        assert!(error.is_some(), "the write must report its failure");
        assert_eq!(std::fs::read(path(home)).unwrap(), before);
        assert_eq!(read(home, "kept"), Some(true.into()));
        assert_eq!(read(home, "lost"), None);
    }

    /// A file that exists but does not hold a JSON object is refused: both
    /// `write` and `remove` return its parse error, and the bytes stay as
    /// they were instead of being replaced by a file the change cannot keep.
    #[test]
    fn a_malformed_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        for malformed in [&b"{\"kept\": true, \"broken\": }"[..], b"[1, 2]"] {
            std::fs::write(path(home), malformed).unwrap();
            for error in [write(home, "lost", 1.into()), remove(home, "kept")] {
                let error = error.expect("a malformed file must be reported");
                assert!(error.contains("config.json"), "{error}");
                assert_eq!(std::fs::read(path(home)).unwrap(), malformed);
            }
            assert_eq!(read(home, "kept"), None);
        }
    }

    /// A `config.json` that is a symbolic link stays a link, its target
    /// takes the write, and the lock stays in the home.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_stays_a_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let linked = dir.path().join("dotfiles");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        let target = linked.join("config.json");
        std::fs::write(&target, "{\"kept\": true}\n").unwrap();
        symlink(&target, path(&home)).unwrap();
        assert_eq!(write(&home, "added", 1.into()), None);
        assert!(std::fs::symlink_metadata(path(&home)).unwrap().is_symlink());
        let config: Value = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(config["kept"], true);
        assert_eq!(config["added"], 1);
        assert!(!linked.join("config.json.lock").exists());
    }

    /// A write keeps an existing file's mode.
    #[cfg(unix)]
    #[test]
    fn a_write_keeps_the_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(path(home), "{}\n").unwrap();
        std::fs::set_permissions(path(home), std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(write(home, "kept", true.into()), None);
        assert_eq!(
            std::fs::metadata(path(home)).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    /// A file that cannot be read is not replaced by one holding a single
    /// key; the error comes back and the path stays as it was.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_left_alone() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        symlink(path(home), path(home)).unwrap();
        let error = write(home, "lost", 1.into());
        assert!(error.is_some(), "the read failure must be reported");
        assert!(
            std::fs::symlink_metadata(path(home)).unwrap().is_symlink(),
            "the unreadable path must not be replaced"
        );
    }
}
