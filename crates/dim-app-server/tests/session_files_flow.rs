//! SessionStore Session files through the `sessionFiles` service and the
//! native store (session-files design §16).
use dim_agent_capabilities::{
    file_storage::FileStoragePlugin,
    host::{SessionFileErrorCode, SessionFilesHost, StoredSessionFile},
};
use dim_agent_runtime::{Capability, KeysOf, SessionConfig, capability_plugin};
use dim_app_server::storage::{SessionStore, SessionStorePlugin, SqliteStoragePlugin};
use rusqlite::Connection;
use rutis::{BoxFuture, CordisError, Ctx, Effect, FiberState, TypedPlugin};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio_util::sync::CancellationToken;

/// A disposable database directory; never the product home.
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/session-files-flow")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory.canonicalize().unwrap())
    }
    fn database(&self) -> PathBuf {
        self.0.join("dimcode.sqlite")
    }
    fn files(&self) -> PathBuf {
        self.0.join("dimcode.sqlite.session-files")
    }
    fn rows(&self, sql: &str) -> Vec<String> {
        let connection = Connection::open(self.database()).unwrap();
        let mut query = connection.prepare(sql).unwrap();
        let mut rows: Vec<String> = query
            .query_map([], |row| {
                Ok(format!("{:?}", row.get::<_, rusqlite::types::Value>(0)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        rows
    }
    /// The entries of the Session-files directory, without FileStorage's own.
    fn entries(&self) -> Vec<String> {
        let mut entries: Vec<String> = match std::fs::read_dir(self.files()) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| !name.starts_with(".dim-file-storage-"))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(error) => panic!("{error}"),
        };
        entries.sort();
        entries
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A process-scope consumer, as Attachments' enclosing-scope dependency is.
struct Consumer(Arc<OnceLock<(Arc<SessionFilesHost>, Arc<SessionStore>)>>);
impl TypedPlugin for Consumer {
    type Deps = (Arc<SessionFilesHost>, Arc<SessionStore>);
    fn name(&self) -> &str {
        "test.session-files-consumer"
    }
    fn apply<'a>(
        &'a self,
        _: &'a Ctx,
        services: Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let _ = self.0.set(services);
            Ok(Effect::Done)
        })
    }
}
impl Capability for Consumer {
    fn keys(&self) -> KeysOf<Self> {
        ((), ())
    }
}

/// Waits until a fiber the mount depends on is Active.
async fn wait_active(node: &rutis::FiberView) {
    let mut status = node.watch();
    while status.borrow().state != FiberState::Active {
        assert_ne!(status.borrow().state, FiberState::Failed);
        status.changed().await.unwrap();
    }
}

struct Store {
    root: Ctx,
    consumer: rutis::FiberView,
    files: Arc<SessionFilesHost>,
    sessions: Arc<SessionStore>,
}
impl Store {
    /// Mounts the native store with the database directory as FileStorage
    /// root 0, or without FileStorage.
    async fn mount(home: &Home, file_storage: bool) -> Self {
        let root = Ctx::root().unwrap();
        // FileStorage activates on its own fiber; the consumer's Active state
        // does not cover it, and a put that lands before it does reports
        // NodeUnavailable. Keep the fiber and wait for it like the consumer.
        let files_node = file_storage.then(|| {
            root.plugin(capability_plugin(FileStoragePlugin::new(vec![
                home.0.clone(),
            ])))
        });
        root.plugin(capability_plugin(SqliteStoragePlugin::new(home.database())));
        root.plugin(capability_plugin(SessionStorePlugin::new(0)));
        let captured = Arc::new(OnceLock::new());
        let consumer = root.plugin(capability_plugin(Consumer(captured.clone())));
        wait_active(&consumer).await;
        if let Some(node) = &files_node {
            wait_active(node).await;
        }
        let (files, sessions) = captured.get().unwrap().clone();
        Self {
            root,
            consumer,
            files,
            sessions,
        }
    }
    async fn session(&self, id: &str) {
        self.sessions
            .timeline_store()
            .create_session(id, &SessionConfig::default())
            .await
            .unwrap();
    }
    async fn put(
        &self,
        session: &str,
        name: &str,
        content: &[u8],
    ) -> Result<StoredSessionFile, SessionFileErrorCode> {
        self.files
            .0
            .put(
                session.into(),
                name.into(),
                content.to_vec(),
                CancellationToken::new(),
            )
            .await
            .map_err(|error| error.code)
    }
}

#[tokio::test]
async fn a_put_stores_an_immutable_file_its_session_owns() {
    let home = Home::new();
    let store = Store::mount(&home, true).await;
    store.session("s1").await;

    let stored = store.put("s1", "paste.txt", b"pasted text").await.unwrap();
    let id = uuid::Uuid::parse_str(&stored.file_id).unwrap();
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(stored.file_id, id.hyphenated().to_string());
    assert_eq!(
        stored.path,
        home.files().join(&stored.file_id).join("paste.txt")
    );
    assert!(stored.path.is_absolute());
    assert_eq!(stored.bytes, 11);
    assert_eq!(std::fs::read(&stored.path).unwrap(), b"pasted text");
    assert!(
        std::fs::metadata(&stored.path)
            .unwrap()
            .permissions()
            .readonly()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&stored.path), 0o400);
        assert_eq!(mode(&home.files()), 0o700);
        assert_eq!(mode(stored.path.parent().unwrap()), 0o700);
    }
    assert_eq!(
        home.rows("SELECT file_id || '|' || ifnull(node, '-') || '|' || ifnull(writer, '-') FROM session_files"),
        [format!("Text(\"{}|-|-\")", stored.file_id)]
    );
    assert_eq!(
        home.rows("SELECT session_id || '|' || file_id FROM session_file_owners"),
        [format!("Text(\"s1|{}\")", stored.file_id)]
    );

    // Every put is a new file, even with the same name and bytes.
    let again = store.put("s1", "paste.txt", b"pasted text").await.unwrap();
    assert_ne!(again.file_id, stored.file_id);
    assert_eq!(home.entries().len(), 2);

    // Rejected puts write nothing.
    let rows = |home: &Home| {
        (
            home.rows("SELECT file_id FROM session_files"),
            home.rows("SELECT file_id FROM session_file_owners"),
            home.entries(),
        )
    };
    let before = rows(&home);
    assert_eq!(
        store.put("missing", "paste.txt", b"x").await,
        Err(SessionFileErrorCode::NotFound)
    );
    for name in [
        "",
        ".hidden",
        "..",
        "a/b",
        "a\\b",
        "naïve.txt",
        "auth.json",
        "AUTH.JSON",
        "Auth.Json.Lock",
        &"a".repeat(65),
    ] {
        assert_eq!(
            store.put("s1", name, b"x").await,
            Err(SessionFileErrorCode::Invalid),
            "{name:?}"
        );
    }
    assert_eq!(
        store
            .put("s1", "big.txt", &vec![0; 16 * 1024 * 1024 + 1])
            .await,
        Err(SessionFileErrorCode::TooLarge)
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        store
            .files
            .0
            .put("s1".into(), "paste.txt".into(), b"x".to_vec(), cancel)
            .await
            .map_err(|error| error.code),
        Err(SessionFileErrorCode::Cancelled)
    );
    assert_eq!(rows(&home), before);

    // A Session creation rollback releases its owner rows with its records.
    store
        .sessions
        .timeline_store()
        .rollback_session("s1")
        .await
        .unwrap();
    assert!(
        home.rows("SELECT file_id FROM session_file_owners")
            .is_empty()
    );
    assert_eq!(
        store.put("s1", "paste.txt", b"x").await,
        Err(SessionFileErrorCode::NotFound)
    );
    store.root.shutdown().await.unwrap();
}

/// Polls until `done`, for the background sweep.
async fn eventually(done: impl Fn() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the sweep did not finish");
}

/// §16 item 4, and the release trigger: the first put of a generation sweeps
/// what crashed generations left, a release sweeps what it unowned, and a
/// second database's Session files in the same directory are never touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweeps_remove_only_what_no_row_of_this_database_owns() {
    let home = Home::new();
    let store = Store::mount(&home, true).await;
    store.session("s1").await;
    let kept = store.put("s1", "kept.txt", b"kept").await.unwrap();
    store.root.shutdown().await.unwrap();

    // What a crashed generation and stray writers leave: a pending row of a
    // dead generation with its directory, a directory and a file without a
    // row, and another database's Session files beside this one's.
    let dead = uuid::Uuid::new_v4().to_string();
    Connection::open(home.database())
        .unwrap()
        .execute(
            "INSERT INTO session_files VALUES (?1, NULL, 'dead-generation')",
            [&dead],
        )
        .unwrap();
    let orphan = uuid::Uuid::new_v4().to_string();
    for directory in [&dead, &orphan] {
        std::fs::create_dir(home.files().join(directory)).unwrap();
        std::fs::write(home.files().join(directory).join("paste.txt"), b"left").unwrap();
    }
    std::fs::write(home.files().join("stray"), b"stray").unwrap();
    let other = home
        .0
        .join("other.sqlite.session-files")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("paste.txt"), b"other database").unwrap();

    let store = Store::mount(&home, true).await;
    let fresh = store.put("s1", "fresh.txt", b"fresh").await.unwrap();
    let mut owned = vec![kept.file_id.clone(), fresh.file_id.clone()];
    owned.sort();
    eventually(|| home.entries() == owned).await;
    assert_eq!(
        home.rows("SELECT file_id FROM session_files"),
        owned
            .iter()
            .map(|id| format!("Text(\"{id}\")"))
            .collect::<Vec<_>>()
    );
    assert_eq!(std::fs::read(&kept.path).unwrap(), b"kept");
    assert_eq!(
        std::fs::read(other.join("paste.txt")).unwrap(),
        b"other database"
    );

    // Releasing the last owner, here by a creation rollback, sweeps its files.
    store
        .sessions
        .timeline_store()
        .rollback_session("s1")
        .await
        .unwrap();
    // The sweep removes a file's entry first and its row a moment later, so
    // waiting for the entries alone runs ahead of the sweep's last write
    // under load and races it on macOS CI (#139). Wait for both.
    eventually(|| {
        home.entries().is_empty()
            && home.rows("SELECT file_id FROM session_files").is_empty()
    })
    .await;
    assert!(home.rows("SELECT file_id FROM session_files").is_empty());
    assert!(other.join("paste.txt").exists());
    store.root.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_put_without_active_file_storage_reports_node_unavailable() {
    let home = Home::new();
    let store = Store::mount(&home, false).await;
    store.session("s1").await;
    assert_eq!(
        store.put("s1", "paste.txt", b"pasted").await,
        Err(SessionFileErrorCode::NodeUnavailable)
    );
    // Nothing is written and nothing above FileStorage reloads.
    assert!(home.rows("SELECT file_id FROM session_files").is_empty());
    assert!(!home.files().exists());
    assert_eq!(store.consumer.watch().borrow().state, FiberState::Active);
    store.root.shutdown().await.unwrap();
}

/// §11.1: the model reads a Session file the way it reads any file. The read
/// asks nothing and leaves the Session's permission state as it was, under
/// the product default preset and under read-only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_reads_its_own_file_without_an_approval_request() {
    use dim_agent_capabilities::{
        defaults,
        host_platform::{HostPlatform, ShellStatus},
        permission::PermissionState,
        workspace_tools::{FileWriteCoordinatorPlugin, WorkspaceToolsConfig},
    };
    use dim_agent_runtime::{AgentBuilder, AgentConfig};
    use dim_agent_types::{EventKind, InputMode, PermissionPolicy, PermissionPreset};

    let home = Home::new();
    let work = home.0.join("work");
    std::fs::create_dir(&work).unwrap();
    let store = Store::mount(&home, true).await;
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "SESSION_FILES_TEST",
        reason: "Reading a Session file runs no command".into(),
    })
    .unwrap();
    platform.home = home.0.clone();
    store
        .root
        .provide_as(rutis::TypeKey::of::<HostPlatform>(), Arc::new(platform))
        .unwrap();
    (&store
        .root
        .plugin(capability_plugin(FileWriteCoordinatorPlugin::default())))
        .await
        .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(store.sessions.timeline_store());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            output_root: home.0.join("command-output"),
        },
    );
    let agent = builder.build_in(&store.root);

    for preset in [PermissionPreset::WorkspaceWrite, PermissionPreset::ReadOnly] {
        let session = agent
            .create_session(SessionConfig {
                cwd: work.to_string_lossy().into_owned(),
                tools: vec!["read".into()],
                permission_policy: PermissionPolicy {
                    preset,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap();
        let stored = store
            .put(session.id(), "paste.txt", b"pasted for the model")
            .await
            .unwrap();
        // Outside the working directory, next to the native database.
        assert!(!stored.path.starts_with(&work));
        let before = session.get::<PermissionState>("main").unwrap().unwrap();
        session
            .submit(
                format!("tool:read {}", serde_json::json!({"path": stored.path})),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let history = session.history("main", None).await.unwrap();
        assert!(
            !history
                .iter()
                .any(|event| matches!(event.kind, EventKind::ApprovalRequest(_))),
            "{preset:?}"
        );
        let results: Vec<_> = history
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ToolResult(data) => Some(data.results.clone()),
                _ => None,
            })
            .flatten()
            .map(|call| call.result)
            .collect();
        assert_eq!(results.len(), 1, "{preset:?}");
        assert_ne!(results[0].is_error, Some(true), "{:?}", results[0]);
        assert!(
            serde_json::to_string(&results[0].content)
                .unwrap()
                .contains("pasted for the model"),
            "{:?}",
            results[0]
        );
        assert_eq!(
            session.get::<PermissionState>("main").unwrap().unwrap(),
            before
        );
        session.close().await.unwrap();
    }
    agent.close().await.unwrap();
    store.root.shutdown().await.unwrap();
}
