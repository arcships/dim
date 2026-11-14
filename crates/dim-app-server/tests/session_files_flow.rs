//! SessionStore Session files through the `sessionFiles` service and the
//! native store (session-files design §16).
use dim_agent_capabilities::{
    file_storage::FileStoragePlugin,
    host::{SessionFileErrorCode, SessionFilesHost, StoredSessionFile},
};
use dim_agent_runtime::SessionConfig;
use dim_agent_runtime::testing::chain_events;
use dim_app_server::storage::{SessionStore, SessionStorePlugin, SqliteStoragePlugin};
use rusqlite::Connection;
use rutis::host::{CtxHost, FiberState};
use rutis::{Ctx, Plugin};
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
impl Plugin for Consumer {
    type Deps = (Arc<SessionFilesHost>, Arc<SessionStore>);
    fn name(&self) -> &str {
        "test.session-files-consumer"
    }
    async fn apply(&self, _: &Ctx, services: Self::Deps) -> rutis::Result {
        let _ = self.0.set(services);
        Ok(())
    }
}

struct Store {
    root: Ctx,
    consumer: rutis::host::FiberView,
    files: Arc<SessionFilesHost>,
    sessions: Arc<SessionStore>,
}
impl Store {
    /// Mounts the native store with the database directory as FileStorage
    /// root 0, or without FileStorage.
    async fn mount(home: &Home, file_storage: bool) -> Self {
        let root = Ctx::root().unwrap();
        if file_storage {
            (&root.plugin(FileStoragePlugin::new(vec![home.0.clone()])))
                .await
                .unwrap();
        }
        root.plugin(SqliteStoragePlugin::new(home.database()));
        let catalog = dim_app_server::catalog();
        let build = rutis_loader::resolve::Build::default();
        let names = dim_agent_runtime::Names::copy(&catalog, &build);
        root.plugin(SessionStorePlugin::new(&names, 0).unwrap());
        let captured = Arc::new(OnceLock::new());
        let consumer = root.plugin(Consumer(captured.clone()));
        let mut status = consumer.watch();
        while status.borrow().state != FiberState::Active {
            assert_ne!(status.borrow().state, FiberState::Failed);
            status.changed().await.unwrap();
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
    async fn owns(&self, session: &str, path: &std::path::Path) -> bool {
        self.files
            .0
            .owns(session.into(), path.to_path_buf())
            .await
            .unwrap()
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

    let again = store.put("s1", "paste.txt", b"pasted text").await.unwrap();
    assert_ne!(again.file_id, stored.file_id);
    assert_eq!(home.entries().len(), 2);

    // Ownership reads the attachment table: only the Session that owns the
    // file id owns its path, and only a path inside that file's directory.
    store.session("s2").await;
    assert!(store.owns("s1", &stored.path).await);
    assert!(store.owns("s1", &again.path).await);
    assert!(!store.owns("s2", &stored.path).await);
    assert!(!store.owns("s1", &home.0.join("outside.txt")).await);
    let stranger = home
        .files()
        .join(uuid::Uuid::new_v4().to_string())
        .join("paste.txt");
    assert!(!store.owns("s1", &stranger).await);

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
    assert!(!store.owns("s1", &stored.path).await);
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
    let rows: Vec<_> = owned.iter().map(|id| format!("Text(\"{id}\")")).collect();
    eventually(|| {
        home.entries() == owned && home.rows("SELECT file_id FROM session_files") == rows
    })
    .await;
    assert_eq!(std::fs::read(&kept.path).unwrap(), b"kept");
    assert_eq!(
        std::fs::read(other.join("paste.txt")).unwrap(),
        b"other database"
    );

    store
        .sessions
        .timeline_store()
        .rollback_session("s1")
        .await
        .unwrap();
    eventually(|| {
        home.entries().is_empty() && home.rows("SELECT file_id FROM session_files").is_empty()
    })
    .await;
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
        workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::{AgentBuilder, AgentConfig, testing::MinimalHost};
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
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(store.sessions.timeline_store());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: home.0.join("command-output"),
        },
    );
    let agent = MinimalHost::start_with(&builder, |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        Ok(())
    })
    .await
    .unwrap();

    for preset in [PermissionPreset::WorkspaceWrite, PermissionPreset::ReadOnly] {
        let session = agent
            .create_session(SessionConfig {
                cwd: work.to_string_lossy().into_owned(),
                tools: Some(vec!["read".into()]),
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
        let history = chain_events(&session, "main", ..).await.unwrap();
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

#[tokio::test]
async fn path_import_is_immutable_bounded_and_session_owned() {
    let home = Home::new();
    let store = Store::mount(&home, true).await;
    store.session("owner").await;
    store.session("other").await;
    let source = home.0.join("source.bin");
    let bytes = vec![0x5a; 16 * 1024 * 1024];
    std::fs::write(&source, &bytes).unwrap();
    use rutis_bridge::session::{HostDispatch, Value, host_key};
    let face = store
        .root
        .get_at::<dyn HostDispatch>(&host_key("sessionFiles"))
        .unwrap();
    let call = face
        .invoke(
            "import",
            Value::List(vec![
                serde_json::json!({"sessionId":"owner", "name":"file.bin", "source":source}).into(),
            ]),
        )
        .unwrap();
    let Value::Reference(reply) = call else {
        panic!("Import is asynchronous")
    };
    let stored: StoredSessionFile =
        serde_json::from_value(reply.wait_async().await.unwrap().json().unwrap()).unwrap();
    let call = face
        .invoke(
            "read",
            Value::List(vec![
                serde_json::json!({"sessionId":"owner", "fileId":stored.file_id, "offset":0})
                    .into(),
            ]),
        )
        .unwrap();
    let Value::Reference(reply) = call else {
        panic!("Read is asynchronous")
    };
    let chunk = reply.wait_async().await.unwrap().json().unwrap();
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(chunk["data"].as_str().unwrap())
            .unwrap(),
        bytes[..64 * 1024]
    );
    assert_eq!(stored.bytes, bytes.len() as u64);
    std::fs::write(&source, b"changed source").unwrap();
    let mut received = Vec::new();
    loop {
        let block = store
            .files
            .0
            .read(
                "owner".into(),
                stored.file_id.clone(),
                received.len() as u64,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            block.len() <= 64 * 1024,
            "Each file read must fit the data-face budget"
        );
        if block.is_empty() {
            break;
        }
        received.extend(block);
    }
    assert_eq!(
        received, bytes,
        "Import preserves its accepted bytes after the source changes"
    );
    assert!(
        store
            .files
            .0
            .read(
                "other".into(),
                stored.file_id.clone(),
                0,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    std::fs::write(&source, vec![0; bytes.len() + 1]).unwrap();
    let failure = store
        .files
        .0
        .import(
            "owner".into(),
            "large.bin".into(),
            source.to_str().unwrap().into(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, SessionFileErrorCode::TooLarge);
    assert_eq!(home.entries(), vec![stored.file_id]);
    store.consumer.shutdown().await.unwrap();
    store.root.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_materializes_user_and_tool_images_without_mutating_history() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use dim_agent_types::*;
    let home = Home::new();
    let store = Store::mount(&home, true).await;
    store.session("owner").await;
    let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR42mP4zwAE/xkgFAAb8gP9PpddpAAAAABJRU5ErkJggg==").unwrap();
    let stored = store
        .files
        .0
        .put(
            "owner".into(),
            "image.png".into(),
            bytes.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let source = ImageSource {
        source_type: ImageSourceType::File,
        media_type: "image/png".into(),
        file_id: Some(stored.file_id.clone()),
        data: None,
        url: None,
    };
    let mut request = ModelRequest::new(
        "provider",
        "model",
        vec![Message {
            role: Role::User,
            content: MessageContent::Parts(vec![
                MessagePart::Image {
                    source: source.clone(),
                },
                MessagePart::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: ToolContent::Blocks(vec![
                        ContentBlock::Text {
                            text: "caption".into(),
                        },
                        ContentBlock::Image { source },
                    ]),
                    structured_content: None,
                    is_error: None,
                },
            ]),
        }],
    );
    request.metadata = Some(RequestMetadata {
        session_id: Some("owner".into()),
        ..Default::default()
    });
    let request = Arc::new(request);
    let original = serde_json::to_value(&request).unwrap();
    let rendered = dim_agent_capabilities::session_files::materialize(
        &request,
        Some(&store.files),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let rendered = serde_json::to_value(rendered).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), original);
    let parts = rendered["messages"][0]["content"].as_array().unwrap();
    for image in [&parts[0]["source"], &parts[1]["content"][1]["source"]] {
        assert_eq!(image["type"], "base64");
        assert_eq!(
            STANDARD.decode(image["data"].as_str().unwrap()).unwrap(),
            bytes
        );
        assert!(image.get("fileId").is_none());
    }
    assert_eq!(parts[1]["tool_use_id"], "tool-1");
    assert_eq!(parts[1]["content"][0]["text"], "caption");
    store.consumer.shutdown().await.unwrap();
    store.root.shutdown().await.unwrap();
    drop(store);
    let restored_home = Home::new();
    std::fs::copy(home.database(), restored_home.database()).unwrap();
    let directory = restored_home.files().join(&stored.file_id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(&stored.path, directory.join("image.png")).unwrap();
    let restored = Store::mount(&restored_home, true).await;
    let request: Arc<ModelRequest> = serde_json::from_value(original).unwrap();
    let recovered = dim_agent_capabilities::session_files::materialize(
        &request,
        Some(&restored.files),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(recovered).unwrap(),
        rendered,
        "A closed native store and its owned files restore the same images in a new home"
    );
    restored.consumer.shutdown().await.unwrap();
    restored.root.shutdown().await.unwrap();
}
