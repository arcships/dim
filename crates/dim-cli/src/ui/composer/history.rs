//! Input history: the last submitted drafts of one working directory, kept in
//! `<home>/input-history.json` (`{"version":1,"buckets":{cwd:[...]}}`, at most
//! ten entries per directory, a repeated entry moves to the end). A file that
//! does not parse is replaced by one holding the appended entry: history is a
//! recall convenience, and refusing would stop recording for good.
//!
//! `append` queues the entry to one writer thread and returns, so the submit
//! path never waits for the file or its lock; a write that fails is dropped.
//! If the writer cannot start, entries are dropped for the rest of the process.
//! `flush` waits for the queue, and the interface calls it on the way out.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use crate::ui::atomic;

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

fn parse(raw: Option<&[u8]>) -> Store {
    let Some(raw) = raw else {
        return Store::default();
    };
    match serde_json::from_slice::<Store>(raw) {
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

fn read(home: &Path) -> Store {
    parse(std::fs::read(path(home)).ok().as_deref())
}

pub fn load(home: &Path, cwd: &str) -> Vec<String> {
    let key = cwd.trim();
    if key.is_empty() {
        return Vec::new();
    }
    read(home).buckets.remove(key).unwrap_or_default()
}

/// Hands `entry` for `cwd` to the history's writer thread and returns; the
/// entry is written under the history's lock, and a write that fails is
/// dropped.
pub fn append(home: &Path, cwd: &str, entry: &str) {
    let key = cwd.trim();
    if key.is_empty() || entry.is_empty() {
        return;
    }
    let _ = writer().send(Message::Append {
        home: home.to_owned(),
        key: key.to_owned(),
        entry: entry.to_owned(),
    });
}

/// Waits until every appended entry has been written or dropped.
pub fn flush() {
    let (flushed, done) = mpsc::channel();
    if writer().send(Message::Flush(flushed)).is_ok() {
        let _ = done.recv();
    }
}

enum Message {
    Append {
        home: PathBuf,
        key: String,
        entry: String,
    },
    Flush(Sender<()>),
}

/// The one thread that writes this process's history, started on first use.
fn writer() -> &'static Sender<Message> {
    static WRITER: OnceLock<Sender<Message>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (sender, queued) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("dim-input-history".into())
            .spawn(move || {
                for message in queued {
                    match message {
                        Message::Append { home, key, entry } => write(&home, &key, &entry),
                        Message::Flush(flushed) => {
                            let _ = flushed.send(());
                        }
                    }
                }
            });
        sender
    })
}

/// Appends `entry` for `key` under the history's lock, so concurrent writers
/// inside one process and across processes keep every entry.
fn write(home: &Path, key: &str, entry: &str) {
    let _ = atomic::update(&path(home), Some(0o600), |current| {
        let mut store = parse(current);
        store.version = 1;
        let list = store.buckets.entry(key.to_owned()).or_default();
        list.retain(|existing| existing != entry);
        list.push(entry.to_owned());
        let skip = list.len().saturating_sub(MAX_ENTRIES);
        list.drain(..skip);
        Ok(Some(serde_json::to_vec(&store)?))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_dedupes_caps_and_isolates_directories() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..12 {
            append(dir.path(), "/work/a", &format!("msg {n}"));
        }
        append(dir.path(), "/work/a", "msg 5");
        append(dir.path(), "/work/b", "other");
        flush();
        let a = load(dir.path(), "/work/a");
        assert_eq!(a.len(), 10);
        assert_eq!(a.last().map(String::as_str), Some("msg 5"));
        assert_eq!(a.iter().filter(|e| *e == "msg 5").count(), 1);
        assert_eq!(load(dir.path(), "/work/b"), vec!["other"]);
        assert!(load(dir.path(), "/missing").is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path(dir.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    /// A file that does not parse is replaced by a valid one holding the
    /// appended entry, so recording goes on instead of stopping for good.
    #[test]
    fn a_corrupt_file_is_replaced_by_one_holding_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "not json").unwrap();
        assert!(load(dir.path(), "/x").is_empty());
        append(dir.path(), "/x", "ok");
        flush();
        assert_eq!(load(dir.path(), "/x"), vec!["ok"]);
        let store: Store = serde_json::from_slice(&std::fs::read(path(dir.path())).unwrap())
            .expect("the appended file must parse");
        assert_eq!(store.version, 1);
        assert_eq!(store.buckets["/x"], vec!["ok"]);
    }

    /// A linked history file stays a link, its target takes the append, and
    /// no lock is left in the linked directory.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_history_stays_a_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let linked = dir.path().join("dotfiles");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        let target = linked.join(FILE);
        std::fs::write(&target, "{\"version\":1,\"buckets\":{\"/keep\":[\"a\"]}}").unwrap();
        symlink(&target, path(&home)).unwrap();
        append(&home, "/work", "entry");
        flush();
        assert!(std::fs::symlink_metadata(path(&home)).unwrap().is_symlink());
        let store: Store = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(store.buckets["/keep"], vec!["a"]);
        assert_eq!(store.buckets["/work"], vec!["entry"]);
        assert!(!linked.join("input-history.json.lock").exists());
    }

    /// Concurrent appends to one home keep every entry.
    #[test]
    fn concurrent_appends_keep_every_entry() {
        if atomic::test_in_child() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for round in 0..8 {
            let home = dir.path().join(format!("round-{round}"));
            std::thread::scope(|scope| {
                for writer in 0..4 {
                    let home = home.clone();
                    scope.spawn(move || {
                        for n in 0..2 {
                            append(&home, "/work", &format!("w{writer} {n}"));
                        }
                    });
                }
            });
            flush();
            let mut entries = load(&home, "/work");
            let expected: Vec<String> = (0..4)
                .flat_map(|writer| (0..2).map(move |n| format!("w{writer} {n}")))
                .collect();
            assert_eq!(entries.len(), expected.len(), "round {round}: {entries:?}");
            entries.sort();
            let mut expected_sorted = expected;
            expected_sorted.sort();
            assert_eq!(entries, expected_sorted, "round {round}");
        }
    }

    /// Set on the children a cross-process test re-executes, with the home
    /// and the writer number they write.
    const CHILD_HOME: &str = "DIM_TEST_ATOMIC_CHILD_HOME";
    const CHILD_WRITER: &str = "DIM_TEST_ATOMIC_CHILD_WRITER";

    /// Two processes writing one home keep every key and every entry, so the
    /// exclusion is a file lock and not one of this process's own. This is
    /// tested here because both home files are written through the same
    /// helper, and the layers let only the composer's modules reach both.
    #[test]
    fn the_lock_holds_across_processes() {
        let Some(home) = std::env::var_os(CHILD_HOME) else {
            return across_processes();
        };
        let home = PathBuf::from(home);
        let writer: usize = std::env::var(CHILD_WRITER).unwrap().parse().unwrap();
        for value in 0..40 {
            let key = format!("key{writer}");
            assert_eq!(crate::ui::config::write(&home, &key, value.into()), None);
            if value < 4 {
                append(&home, "/work", &format!("entry {writer} {value}"));
            }
        }
        flush();
    }

    /// Starts the children of `the_lock_holds_across_processes` and checks
    /// what they wrote: every key, and every history entry.
    fn across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let test = std::env::current_exe().unwrap();
        let mut children = atomic::TestChildren(Vec::new());
        for writer in 0..2 {
            children.0.push(
                std::process::Command::new(&test)
                    .arg("--exact")
                    .arg("ui::composer::history::tests::the_lock_holds_across_processes")
                    .env(CHILD_HOME, home)
                    .env(CHILD_WRITER, writer.to_string())
                    .spawn()
                    .unwrap_or_else(|error| panic!("writer {writer}: {error}")),
            );
        }
        children.wait();
        for writer in 0..2 {
            let key = format!("key{writer}");
            assert_eq!(
                crate::ui::config::read(home, &key),
                Some(39.into()),
                "{key} lost"
            );
        }
        let mut entries = load(home, "/work");
        entries.sort();
        let mut expected: Vec<String> = (0..2)
            .flat_map(|writer| (0..4).map(move |value| format!("entry {writer} {value}")))
            .collect();
        expected.sort();
        assert_eq!(entries, expected);
    }
}
