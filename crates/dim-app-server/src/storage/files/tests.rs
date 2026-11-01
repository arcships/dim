//! Interleavings the public service cannot produce on demand (session-files
//! §6): a hook runs between the file write and txn 2, and sweeps run where
//! the test decides instead of in the background. The fork step is S5c's,
//! run here inside a transaction as S5c's fork will run it. A second hook
//! stops a Session delete between its output removal and its row
//! transaction, as a crash would (S5c §4).
use super::*;
use crate::storage::{SessionStore, SqliteTimelineStore};
use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_capabilities::host_platform::{HostPlatform, ShellStatus};
use dim_agent_runtime::{SessionConfig, TimelineStore};
use rutis::host::CtxHost;
use std::path::PathBuf;
use tokio::sync::Semaphore;

/// Hooks that run once, for the named Session, right before the
/// cancellation check that precedes txn 2.
type Hook = Box<dyn FnOnce() + Send>;
static BEFORE_COMMIT: Mutex<Vec<(String, Hook)>> = Mutex::new(Vec::new());

pub(super) fn before_commit(session_id: &str) {
    let hook = {
        let mut hooks = BEFORE_COMMIT.lock().unwrap();
        let index = hooks.iter().position(|(hooked, _)| hooked == session_id);
        index.map(|index| hooks.swap_remove(index).1)
    };
    if let Some(hook) = hook {
        hook();
    }
}
fn on_commit(session_id: &str, hook: impl FnOnce() + Send + 'static) {
    BEFORE_COMMIT
        .lock()
        .unwrap()
        .push((session_id.into(), Box::new(hook)));
}

/// Sessions whose next delete stops after its output removal.
static STOP_AFTER_OUTPUT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Sessions whose next delete waits after its output removal: the first
/// semaphore is signalled when the delete reaches the pause, the second
/// releases it.
type OutputPause = (String, Arc<Semaphore>, Arc<Semaphore>);
static PAUSE_AFTER_OUTPUT: Mutex<Vec<OutputPause>> = Mutex::new(Vec::new());

fn pause_after_output_removal(session_id: &str) -> (Arc<Semaphore>, Arc<Semaphore>) {
    let reached = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    PAUSE_AFTER_OUTPUT
        .lock()
        .unwrap()
        .push((session_id.into(), reached.clone(), release.clone()));
    (reached, release)
}

pub(super) async fn after_output_removal(session_id: &str) -> dim_agent_runtime::Result<()> {
    let stopped = {
        let mut stops = STOP_AFTER_OUTPUT.lock().unwrap();
        stops
            .iter()
            .position(|stopped| stopped == session_id)
            .map(|index| stops.swap_remove(index))
    };
    if stopped.is_some() {
        return Err(error("stopped after the output removal"));
    }
    let paused = {
        let mut pauses = PAUSE_AFTER_OUTPUT.lock().unwrap();
        pauses
            .iter()
            .position(|(paused, _, _)| paused == session_id)
            .map(|index| pauses.swap_remove(index))
    };
    if let Some((_, reached, release)) = paused {
        reached.add_permits(1);
        let _released = release
            .acquire()
            .await
            .map_err(|_| error("the test released no delete"))?;
    }
    Ok(())
}

struct Fixture {
    home: PathBuf,
    root: Ctx,
    db: Arc<SqliteStorage>,
    timeline: Arc<SqliteTimelineStore>,
    files: SessionFiles,
}
impl Fixture {
    async fn new() -> Self {
        let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/session-files-unit")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let root = Ctx::root().unwrap();
        (&root.plugin(FileStoragePlugin::new(vec![home.clone()])))
            .await
            .unwrap();
        let db = SqliteStorage::open(home.join("dimcode.sqlite")).unwrap();
        let timeline = SqliteTimelineStore::new(db.clone());
        let files = SessionFiles::new(db.clone(), root.clone(), 0, Arc::new(Notify::new()));
        Self {
            home,
            root,
            db,
            timeline,
            files,
        }
    }
    async fn session(&self, id: &str) {
        self.timeline
            .create_session(id, &SessionConfig::default())
            .await
            .unwrap();
    }
    async fn put(
        &self,
        session: &str,
        cancel: CancellationToken,
    ) -> Result<StoredSessionFile, SessionFileErrorCode> {
        self.files
            .put(
                session.into(),
                "paste.txt".into(),
                b"pasted".to_vec(),
                cancel,
            )
            .await
            .map_err(|failure| failure.code)
    }
    fn connection(&self) -> Connection {
        Connection::open(self.home.join("dimcode.sqlite")).unwrap()
    }
    /// `file_id|writer|owners` for every row.
    fn rows(&self) -> Vec<String> {
        self.connection()
            .prepare("SELECT file_id || '|' || ifnull(writer, '-') || '|' || (SELECT count(*) FROM session_file_owners o WHERE o.file_id = f.file_id) FROM session_files f ORDER BY file_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    fn entries(&self) -> Vec<String> {
        let directory = self.home.join("dimcode.sqlite.session-files");
        let mut entries: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| !name.starts_with(".dim-file-storage-"))
            .collect();
        entries.sort();
        entries
    }
    async fn close(self) {
        self.files.close().await;
        self.timeline.close().await.unwrap();
        self.db.close().await.unwrap();
        self.root.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.home).unwrap();
    }
}

/// §16 item 2: a fork owns its source's files without copying them, keeps
/// them after the source is released, and the last release removes them.
#[tokio::test]
async fn a_fork_shares_files_until_their_last_owner_is_released() {
    let fixture = Fixture::new().await;
    fixture.session("source").await;
    fixture.session("fork").await;
    let stored = fixture
        .put("source", CancellationToken::new())
        .await
        .unwrap();
    let identity = |path: &Path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(path).unwrap();
            (metadata.dev(), metadata.ino())
        }
        #[cfg(not(unix))]
        {
            std::fs::metadata(path).unwrap().len()
        }
    };
    let before = identity(&stored.path);
    fixture
        .db
        .run(|connection| {
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(error)?;
            share(&tx, "source", "fork").map_err(error)?;
            tx.commit().map_err(error)
        })
        .await
        .unwrap();
    assert_eq!(fixture.rows(), [format!("{}|-|2", stored.file_id)]);

    fixture.timeline.rollback_session("source").await.unwrap();
    assert!(fixture.files.0.sweep().await);
    assert_eq!(fixture.rows(), [format!("{}|-|1", stored.file_id)]);
    assert_eq!(identity(&stored.path), before);
    assert_eq!(std::fs::read(&stored.path).unwrap(), b"pasted");
    assert_eq!(fixture.entries(), std::slice::from_ref(&stored.file_id));

    fixture.timeline.rollback_session("fork").await.unwrap();
    assert_eq!(fixture.rows(), [format!("{}|-|0", stored.file_id)]);
    assert!(fixture.files.0.sweep().await);
    assert!(fixture.rows().is_empty());
    assert!(fixture.entries().is_empty());
    fixture.close().await;
}

/// §6 and §16 items 3 and 6: a put that loses its Session, is cancelled, or
/// has its row claimed by a later generation's sweep after the file is
/// written owns nothing, and the sweep removes the written directory.
#[tokio::test]
async fn puts_ended_before_txn_2_are_abandoned_and_swept() {
    let fixture = Fixture::new().await;
    let database = fixture.home.join("dimcode.sqlite");

    fixture.session("put-deleted").await;
    on_commit("put-deleted", {
        let database = database.clone();
        move || {
            let connection = Connection::open(database).unwrap();
            for table in ["session_catalog", "timeline_branches", "agent_sessions"] {
                connection
                    .execute(
                        &format!("DELETE FROM {table} WHERE session_id = 'put-deleted'"),
                        [],
                    )
                    .unwrap();
            }
        }
    });
    assert_eq!(
        fixture.put("put-deleted", CancellationToken::new()).await,
        Err(SessionFileErrorCode::NotFound)
    );

    fixture.session("cancelled").await;
    let cancel = CancellationToken::new();
    on_commit("cancelled", {
        let cancel = cancel.clone();
        move || cancel.cancel()
    });
    assert_eq!(
        fixture.put("cancelled", cancel).await,
        Err(SessionFileErrorCode::Cancelled)
    );

    fixture.session("claimed").await;
    on_commit("claimed", move || {
        Connection::open(database)
            .unwrap()
            .execute(
                "UPDATE session_files SET writer = NULL WHERE writer IS NOT NULL",
                [],
            )
            .unwrap();
    });
    assert_eq!(
        fixture.put("claimed", CancellationToken::new()).await,
        Err(SessionFileErrorCode::Cancelled)
    );

    let rows = fixture.rows();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.ends_with("|-|0")), "{rows:?}");
    assert_eq!(fixture.entries().len(), 3);
    for entry in fixture.entries() {
        let written = fixture
            .home
            .join("dimcode.sqlite.session-files")
            .join(entry)
            .join("paste.txt");
        assert_eq!(std::fs::read(written).unwrap(), b"pasted");
    }
    assert!(fixture.files.0.sweep().await);
    assert!(fixture.rows().is_empty());
    assert!(fixture.entries().is_empty());
    fixture.close().await;
}

/// S5c §4 and acceptance 9: a delete stopped after its output removal
/// leaves every row and no output; its retry removes the rows and releases
/// the Session's files, and the next generation's first put sweeps them.
#[tokio::test]
async fn an_interrupted_delete_converges_on_retry_and_the_next_sweep() {
    let fixture = Fixture::new().await;
    let store = SessionStore::new(fixture.db.clone(), fixture.root.clone(), 0);
    let output = |id: &str| fixture.home.join("command-output").join(id);
    fixture.session("deleted").await;
    fixture.session("kept").await;
    let stored = fixture
        .put("deleted", CancellationToken::new())
        .await
        .unwrap();
    std::fs::create_dir_all(output("deleted").join("nested")).unwrap();
    std::fs::write(output("deleted").join("nested/exec.log"), b"output").unwrap();
    std::fs::create_dir_all(output("kept")).unwrap();
    let sessions = || {
        fixture
            .connection()
            .prepare("SELECT session_id FROM agent_sessions UNION ALL SELECT session_id FROM timeline_branches ORDER BY 1")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };

    STOP_AFTER_OUTPUT.lock().unwrap().push("deleted".into());
    assert!(store.delete_session("deleted").await.is_err());
    assert_eq!(sessions(), ["deleted", "deleted", "kept", "kept"]);
    assert_eq!(fixture.rows(), [format!("{}|-|1", stored.file_id)]);
    assert!(!output("deleted").exists());
    fixture
        .timeline
        .branches("deleted")
        .await
        .expect("the Session still loads without its output");

    assert!(store.delete_session("deleted").await.unwrap());
    assert_eq!(sessions(), ["kept", "kept"]);
    assert_eq!(fixture.rows(), [format!("{}|-|0", stored.file_id)]);
    assert_eq!(fixture.entries(), std::slice::from_ref(&stored.file_id));
    assert!(output("kept").exists());
    assert!(!store.delete_session("deleted").await.unwrap());
    assert!(!store.delete_session("unknown").await.unwrap());
    assert_eq!(sessions(), ["kept", "kept"]);

    let next = SessionFiles::new(
        fixture.db.clone(),
        fixture.root.clone(),
        0,
        Arc::new(Notify::new()),
    );
    next.start();
    let kept = next
        .put(
            "kept".into(),
            "paste.txt".into(),
            b"kept".to_vec(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    for _ in 0..500 {
        if fixture.rows() == [format!("{}|-|1", kept.file_id)] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(fixture.rows(), [format!("{}|-|1", kept.file_id)]);
    assert_eq!(fixture.entries(), [kept.file_id]);
    next.close().await;
    drop(store);
    fixture.close().await;
}

/// ADR 013 D4: the fork copies every output row, including one for a file
/// that does not exist yet. `run_background` registers the output path before
/// its command writes it, so the fork must own the file the command creates
/// and keep it after the source delete.
#[tokio::test]
async fn a_fork_owns_output_registered_before_the_file_exists() {
    let fixture = Fixture::new().await;
    let store = SessionStore::new(fixture.db.clone(), fixture.root.clone(), 0);
    fixture.session("running-source").await;
    let log = fixture.home.join("command-output/running-source/exec.log");
    fixture
        .files
        .register("running-source".into(), log.clone())
        .await
        .unwrap();
    assert!(!log.exists());

    store
        .fork_session("running-source", "running-fork", 0)
        .await
        .unwrap();
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, b"output").unwrap();

    assert!(store.delete_session("running-source").await.unwrap());
    assert_eq!(std::fs::read(&log).unwrap(), b"output");
    let owned: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM session_output_owners WHERE session_id='running-fork'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owned, 1, "the fork owns the file the command created");

    drop(store);
    fixture.close().await;
}

/// ADR 013 D4: a fork and a delete serialize on the ownership lock. A fork
/// admitted while the delete is removing output cannot copy a row for it.
#[tokio::test]
async fn a_fork_waits_for_a_delete_that_removed_output() {
    let fixture = Fixture::new().await;
    let store = SessionStore::new(fixture.db.clone(), fixture.root.clone(), 0);
    fixture.session("paused-source").await;
    let log = fixture.home.join("command-output/paused-source/exec.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, b"output").unwrap();
    fixture
        .files
        .register("paused-source".into(), log.clone())
        .await
        .unwrap();

    let (reached, release) = pause_after_output_removal("paused-source");
    let deleting = {
        let store = store.clone();
        tokio::spawn(async move { store.delete_session("paused-source").await })
    };
    let _reached = reached.acquire().await.unwrap();
    let forking = {
        let store = store.clone();
        tokio::spawn(async move { store.fork_session("paused-source", "paused-fork", 0).await })
    };
    release.add_permits(1);
    assert!(deleting.await.unwrap().unwrap());
    assert!(forking.await.unwrap().is_err(), "the source is gone");
    let copied: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM session_output_owners WHERE session_id='paused-fork'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copied, 0, "the fork owns no removed output");
    assert!(!log.exists());

    drop(store);
    fixture.close().await;
}

/// ADR 013 D4 on Windows: the directory separator is also the LIKE escape
/// character, so it must be escaped before the wildcard is appended.
#[test]
fn a_windows_style_directory_pattern_matches_a_nested_row() {
    let pattern = like_prefix(Path::new(r"C:\data\command-output\S"), '\\');
    assert_eq!(pattern, r"C:\\data\\command-output\\S\\%");
    let connection = Connection::open_in_memory().unwrap();
    let matched: bool = connection
        .query_row(
            "SELECT ?1 LIKE ?2 ESCAPE '\\'",
            rusqlite::params![r"C:\data\command-output\S\exec.log", pattern],
            |row| row.get(0),
        )
        .unwrap();
    assert!(matched);
}

/// ADR 013 D4: two concurrent deletes of a source and its copy must not both
/// keep the shared directory and leave it without an owner.
#[tokio::test]
async fn concurrent_deletes_remove_a_shared_output_directory() {
    let fixture = Fixture::new().await;
    let store = SessionStore::new(fixture.db.clone(), fixture.root.clone(), 0);
    fixture.session("source").await;
    let log = fixture.home.join("command-output/source/exec.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, b"output").unwrap();
    fixture
        .files
        .register("source".into(), log.clone())
        .await
        .unwrap();
    store.fork_session("source", "fork", 0).await.unwrap();

    let (source, fork) = tokio::join!(store.delete_session("source"), store.delete_session("fork"));
    assert!(source.unwrap());
    assert!(fork.unwrap());
    assert!(
        !log.exists(),
        "the last owner's delete removes the directory"
    );
    let remaining: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM session_output_owners", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0);

    drop(store);
    fixture.close().await;
}

/// ADR 013 D4: a shell output directory goes through HostPlatform, is kept
/// while another Session names a path under it, and is removed by the last
/// owner's delete.
#[tokio::test]
async fn a_delete_removes_shell_output_of_its_last_owner() {
    let fixture = Fixture::new().await;
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "DELETE_FLOW",
        reason: "Deleting output runs no command".into(),
    })
    .unwrap();
    platform.home = fixture.home.clone();
    fixture
        .root
        .provide_as::<HostPlatform>(Arc::new(platform))
        .unwrap();
    let store = SessionStore::new(fixture.db.clone(), fixture.root.clone(), 0);
    fixture.session("source").await;
    let log = fixture.home.join(".dim/exec/source/main/exec-1.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, b"output").unwrap();
    fixture
        .files
        .register("source".into(), log.clone())
        .await
        .unwrap();
    store.fork_session("source", "fork", 0).await.unwrap();

    assert!(store.delete_session("source").await.unwrap());
    assert!(log.exists());
    assert!(store.delete_session("fork").await.unwrap());
    assert!(!log.exists());

    drop(store);
    fixture.close().await;
}

/// ADR 012 D1: a delete removes the Session's grants in the same transaction
/// as its other rows.
#[tokio::test]
async fn a_delete_removes_the_sessions_grants() {
    use dim_agent_types::grant::{GrantKind, GrantMode};

    let fixture = Fixture::new().await;
    let store = SessionStore::new(fixture.db.clone(), fixture.root.clone(), 0);
    fixture.session("deleted").await;
    store
        .add_grant(
            "deleted",
            PathBuf::from("/work/notes.md"),
            GrantKind::File,
            GrantMode::Write,
        )
        .await
        .unwrap();
    store
        .add_grant(
            "deleted",
            PathBuf::from("/work/notes"),
            GrantKind::Dir,
            GrantMode::Write,
        )
        .await
        .unwrap();
    assert_eq!(store.grants("deleted").await.unwrap().len(), 2);
    assert!(store.delete_session("deleted").await.unwrap());
    assert!(store.grants("deleted").await.unwrap().is_empty());
    drop(store);
    fixture.close().await;
}
