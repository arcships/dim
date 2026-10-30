//! Interleavings the public service cannot produce on demand (session-files
//! §6): a hook runs between the file write and txn 2, and sweeps run where
//! the test decides instead of in the background. The fork step is S5c's,
//! run here inside a transaction as S5c's fork will run it. A second hook
//! stops a Session delete between its output removal and its row
//! transaction, as a crash would (S5c §4).
use super::*;
use crate::storage::{SessionStore, SqliteTimelineStore};
use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_runtime::{SessionConfig, TimelineStore};
use rutis::host::CtxHost;
use std::path::PathBuf;

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

pub(super) fn after_output_removal(session_id: &str) -> dim_agent_runtime::Result<()> {
    let mut stops = STOP_AFTER_OUTPUT.lock().unwrap();
    match stops.iter().position(|stopped| stopped == session_id) {
        Some(index) => {
            stops.swap_remove(index);
            Err(error("stopped after the output removal"))
        }
        None => Ok(()),
    }
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
