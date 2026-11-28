//! #2204: the Session-id output-directory isolation on the Windows
//! filesystem it protects. POSIX hosts compare the ids' destinations
//! textually against volume rules they never execute; only here does the
//! drive-anchored resolution the comparison mirrors actually govern the
//! directories Sessions write. A legacy id resolving back inside the output
//! root — drive-absolute, or verbatim as a canonicalized Windows path spells
//! it — overlaps the candidate's own output directory and must refuse
//! creation and the fork with InvalidInput before either writes a row.

use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_runtime::{AgentError, SessionConfig, StoredEvent, TimelineKey, TimelineStore};
use dim_agent_types::{AgentEvent, AgentProfile, DeltaData, EventKind};
use dim_app_server::protocol::SessionOrigin;
use dim_app_server::storage::{SqliteStorage, SqliteTimelineStore};
use rusqlite::{Connection, params};
use rutis::host::CtxHost;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("dim-nocase-flow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory.join("dimcode.sqlite"))
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

/// The owner agent the store's writer foreign key requires: the writer
/// connection enforces `PRAGMA foreign_keys`, and the owned creation inserts
/// `session_owners.agent_id` against `agents`, so a test that never seeds it
/// fails on Windows — the platform this file runs on — with FOREIGN KEY
/// constraint failed instead of exercising the isolation.
fn seed_owner(connection: &Connection) {
    connection
        .execute(
            "INSERT INTO agents(agent_id, profile, is_default) VALUES ('owner', ?1, 1)",
            [serde_json::to_string(&AgentProfile {
                name: "owner".into(),
                description: String::new(),
                system_prompt: String::new(),
                tools: Some(vec![]),
                model: None,
            })
            .unwrap()],
        )
        .unwrap();
}

/// A stored record written around every validation, the way a pre-#2204 or
/// external writer could leave it.
fn seed_stored_session(connection: &Connection, session_id: &str) {
    let config = serde_json::to_string(&SessionConfig::default()).unwrap();
    connection
        .execute(
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) \
             VALUES (?1, ?2, 0, 'client')",
            params![session_id, config],
        )
        .unwrap();
}

fn stored_rows(connection: &Connection, session_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT (SELECT count(*) FROM agent_sessions WHERE session_id=?1) + \
             (SELECT count(*) FROM timeline_branches WHERE session_id=?1) + \
             (SELECT count(*) FROM timeline_events WHERE session_id=?1)",
            [session_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// The normal components of a canonical path, the names a path spelling of
/// the output root carries.
fn normal_names(root: &Path) -> Vec<String> {
    root.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The drive letter of a canonical Windows path, the letter a verbatim
/// `\\?\` prefix or a plain `C:` prefix carries.
fn drive_letter(root: &Path) -> char {
    let text = root.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let bytes = text.as_bytes();
    assert!(
        bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic(),
        "the test drive layout needs a lettered root, got {text}"
    );
    (bytes[0] as char).to_ascii_lowercase()
}

/// One stored legacy id, refused or admitted on both the creation and the
/// fork entry. `stored` derives from the test's own output root: the drive
/// layout and the root's real components only exist on the machine the test
/// runs on, so nothing in the id is hardcoded. Creation and the fork
/// admission each run in a fresh store, since the admitted id is written by
/// then.
async fn assert_shape(
    stored: impl Fn(&Path) -> String,
    candidate: &str,
    admitted_control: bool,
    why: &str,
) {
    for fork_only in [false, true] {
        let database = Database::new();
        let output_root = database
            .0
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join("command-output");
        let stored = stored(&output_root);
        let directory = database.0.parent().unwrap().canonicalize().unwrap();
        let root = rutis::Ctx::root().unwrap();
        (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
            .await
            .unwrap();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
        let store = SqliteTimelineStore::new(db.clone());
        // The fork source needs no rows of its own: the refusal must precede
        // the copy's first write, and an empty copy still exercises both
        // chunk transactions the check rides on.
        store
            .create_session("source", &SessionConfig::default())
            .await
            .unwrap();
        let connection = Connection::open(&database.0).unwrap();
        seed_owner(&connection);
        let config = sessions
            .session_config_text("source")
            .await
            .unwrap()
            .unwrap();
        seed_stored_session(&connection, &stored);

        if fork_only {
            let forked = sessions.fork_session("source", candidate, 1, &config).await;
            if admitted_control {
                assert!(
                    forked.is_ok(),
                    "{why}: forking into {candidate:?} while {stored:?} is stored must succeed, got {forked:?}"
                );
            } else {
                assert!(
                    matches!(forked, Err(AgentError::InvalidInput(_))),
                    "{why}: forking into {candidate:?} while {stored:?} is stored must be InvalidInput, got {forked:?}"
                );
            }
        } else {
            let created = sessions
                .create_owned_session(
                    candidate,
                    &SessionConfig::default(),
                    "owner",
                    2,
                    None,
                    SessionOrigin::Client,
                    None,
                )
                .await;
            if admitted_control {
                assert!(
                    created.is_ok(),
                    "{why}: creating {candidate:?} while {stored:?} is stored must succeed, got {created:?}"
                );
            } else {
                assert!(
                    matches!(created, Err(AgentError::InvalidInput(_))),
                    "{why}: creating {candidate:?} while {stored:?} is stored must be InvalidInput, got {created:?}"
                );
            }
        }
        if admitted_control {
            assert!(
                stored_rows(&connection, candidate) > 0,
                "{why}: the admitted candidate {candidate:?} is written"
            );
        } else {
            assert_eq!(
                stored_rows(&connection, candidate),
                0,
                "{why}: the refused candidate {candidate:?} writes nothing"
            );
        }

        drop(connection);
        drop(sessions);
        store.close().await.unwrap();
        db.close().await.unwrap();
        root.shutdown().await.unwrap();
    }
}

/// A drive-absolute legacy id resolving back inside the output root —
/// `C:\<root components>\abc` — overlaps the candidate's own output
/// directory on the volume the test store lives on. Creation and the fork
/// must refuse the candidate before either writes, while the stored row
/// keeps its bytes; a candidate on an untouched directory is admitted.
#[tokio::test]
async fn drive_absolute_overlap_refuses_create_and_fork() {
    assert_shape(
        |root: &Path| {
            format!(
                "{}:\\{}\\abc",
                drive_letter(root),
                normal_names(root).join("\\")
            )
        },
        "abc",
        false,
        "a drive-absolute id resolving inside the output root overlaps the candidate's directory",
    )
    .await;
    assert_shape(
        |root: &Path| {
            format!(
                "{}:\\{}\\sibling",
                drive_letter(root),
                normal_names(root).join("\\")
            )
        },
        "abc",
        true,
        "a drive-absolute id resolving beside the candidate's directory shares nothing with it",
    )
    .await;
    assert_shape(
        |root: &Path| format!("\\{}\\abc", normal_names(root).join("\\")),
        "abc",
        false,
        "a rooted id resolves inside the output root on the same volume, so it overlaps the candidate's directory",
    )
    .await;
    assert_shape(
        |_: &Path| "\\unshared-root\\abc".to_owned(),
        "abc",
        true,
        "a rooted id resolving beside the output root's own directory shares nothing with the candidate",
    )
    .await;
}

/// The verbatim spelling `\\?\C:\<root components>\abc` is the form
/// `canonicalize` returns on Windows: it names the same volume and directory
/// as the plain drive-absolute form, so the comparison's volume
/// normalization must treat it as one, and the id resolving inside the
/// output root refuses creation and the fork.
#[tokio::test]
async fn verbatim_overlap_refuses_create_and_fork() {
    assert_shape(
        |root: &Path| {
            format!(
                "\\\\?\\{}:\\{}\\abc",
                drive_letter(root),
                normal_names(root).join("\\")
            )
        },
        "abc",
        false,
        "a verbatim id names the same volume and directory as the plain drive-absolute form",
    )
    .await;
}

fn key(session: &str, branch: &str) -> TimelineKey {
    TimelineKey {
        session_id: session.into(),
        branch_id: branch.into(),
    }
}

fn record(sequence: u64, kind: EventKind) -> StoredEvent {
    StoredEvent {
        event: Arc::new(AgentEvent {
            id: format!("event-{sequence}"),
            sequence,
            create_at: 1000 + sequence,
            tags: Some(vec!["stored".into()]),
            parent_call_id: None,
            kind,
        }),
        trace: None,
        batch_end: sequence,
    }
}

fn text(sequence: u64, delta: &str) -> StoredEvent {
    record(
        sequence,
        EventKind::Text(DeltaData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            delta: delta.into(),
        }),
    )
}

async fn append(
    store: &SqliteTimelineStore,
    key: TimelineKey,
    records: Vec<StoredEvent>,
) -> dim_agent_runtime::Result<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store.append(
        key,
        records,
        Box::new(move |result| {
            let _ = sender.send(result);
        }),
    )?;
    tokio::time::timeout(Duration::from_secs(180), receiver)
        .await
        .unwrap()
        .unwrap()
}

/// A stored id already inside the output root keeps its bytes and its
/// behavior: restoration reads its rows as written, deletion removes it
/// through the product entry — a single plain component passes the deletion
/// boundary's shape check — and it forks as a source the way any Session
/// does. Only the admission of a conflicting new id is refused; the legacy
/// rows themselves never change.
#[tokio::test]
async fn legacy_single_component_ids_remain_operable() {
    let database = Database::new();
    let directory = database.0.parent().unwrap().canonicalize().unwrap();
    let root = rutis::Ctx::root().unwrap();
    (&root.plugin(FileStoragePlugin::new(vec![directory.clone()])))
        .await
        .unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let sessions = dim_app_server::storage::SessionStore::new(db.clone(), root.clone(), 0);
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    seed_owner(&connection);
    seed_stored_session(&connection, "Legacy-9");
    connection
        .execute(
            "INSERT INTO timeline_branches (session_id, branch_id, info, sequence) \
             VALUES ('Legacy-9', 'main', '{\"id\":\"main\"}', 0)",
            [],
        )
        .unwrap();
    append(
        &store,
        key("Legacy-9", "main"),
        vec![text(1, "first"), text(2, "second")],
    )
    .await
    .unwrap();

    let config = sessions
        .session_config_text("Legacy-9")
        .await
        .unwrap()
        .unwrap();
    assert!(
        sessions.session_config("Legacy-9").await.unwrap().is_some(),
        "restoration reads the single-component legacy id's configuration as written"
    );
    sessions
        .fork_session("Legacy-9", "valid-fork", 2, &config)
        .await
        .unwrap();
    assert!(
        stored_rows(&connection, "valid-fork") > 0,
        "the legacy source's rows are copied into the fork"
    );

    assert!(sessions.delete_session("Legacy-9").await.unwrap());
    assert!(
        connection
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id='Legacy-9')",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap(),
        "deletion removes the single-component legacy id as before"
    );

    drop(connection);
    drop(sessions);
    store.close().await.unwrap();
    db.close().await.unwrap();
    root.shutdown().await.unwrap();
}
