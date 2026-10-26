use dim_agent_capabilities::file_storage::{
    FileLease, FilePath, FileStorage, FileStorageError, FileStoragePlugin,
};
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_app_server::{
    settings::{
        BusinessDocument, ProjectAuthorized, Result, SettingsChanged, SettingsError,
        SettingsPlugin, SettingsScope, SettingsService, SourceDeclaration, Writer,
        project_authorizations, settings_changes,
    },
    storage::SqliteStoragePlugin,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, Value as RpcValue, host_key};
use rutis_loader::resolve::Build;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{fs, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Settings keyed from the App Server's catalog, mounted on a test root.
fn settings_part(home_root: usize, project_roots: Vec<usize>) -> SettingsPlugin {
    let catalog = dim_app_server::catalog();
    let build = Build::default();
    SettingsPlugin::new(&Names::copy(&catalog, &build), home_root, project_roots).unwrap()
}
/// [`settings_part`] for the native profile.
fn native_settings_part(home_root: usize, project_roots: Vec<usize>) -> SettingsPlugin {
    let catalog = dim_app_server::catalog();
    let build = Build::default();
    SettingsPlugin::native(&Names::copy(&catalog, &build), home_root, project_roots).unwrap()
}

type Captured = tokio::sync::watch::Sender<
    Option<(
        std::sync::Arc<SettingsService>,
        std::sync::Arc<rutis_loader::Loader>,
    )>,
>;
struct ProcessSettings {
    capture: Captured,
    settings: Key<SettingsService>,
}
impl ProcessSettings {
    fn new(names: &Names<'_>, capture: Captured) -> rutis::Result<Self> {
        Ok(Self {
            capture,
            settings: names.key()?,
        })
    }
}
impl DynPlugin for ProcessSettings {
    fn name(&self) -> &str {
        "test.process-settings"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase(), TypeKey::of::<rutis_loader::Loader>()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let loader = ctx.require::<rutis_loader::Loader>()?;
            self.capture.send_replace(Some((settings, loader)));
            Ok(Effect::Done)
        })
    }
}

/// Records every event of one rutis channel, in delivery order.
fn listen<E: rutis::Event<Value = ()> + Clone>(
    root: &Ctx,
    name: &str,
) -> tokio::sync::watch::Receiver<Vec<E>> {
    let log = std::sync::Arc::new(tokio::sync::watch::Sender::new(Vec::new()));
    let sender = log.clone();
    root.listen::<E>()
        .name(name)
        .on(move |event, _| {
            sender.send_modify(|log| log.push((*event).clone()));
            async { Ok(None) }
        })
        .unwrap();
    log.subscribe()
}
/// Waits until the channel has delivered an event matching `seen`.
async fn saw<E: Clone>(
    changes: &mut tokio::sync::watch::Receiver<Vec<E>>,
    seen: impl Fn(&E) -> bool,
) {
    tokio::time::timeout(
        Duration::from_secs(5),
        changes.wait_for(|log| log.iter().any(&seen)),
    )
    .await
    .unwrap()
    .unwrap();
}
/// Takes the lease once its holder releases it; retry policy belongs to the consumer.
async fn lock_when_free(
    files: &Arc<FileStorage>,
    path: &FilePath,
    cancel: &CancellationToken,
) -> FileLease {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match files.lock(path, cancel).await {
                Err(FileStorageError::Locked) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                lease => return lease.unwrap(),
            }
        }
    })
    .await
    .unwrap()
}
fn change(scope: SettingsScope, revision: &str) -> impl Fn(&SettingsChanged) -> bool + '_ {
    move |c| c.source == "example" && c.scope == scope && c.revision == revision
}

#[tokio::test]
async fn server_reload_preserves_project_root_identity_and_settings() {
    use dim_agent_capabilities::defaults;
    use dim_agent_runtime::{AgentBuilder, AgentConfig, Feature, SessionConfig};
    use dim_app_server::{ServerConfig, mcp::config::McpSettings, serve};

    tokio::time::timeout(Duration::from_secs(10), async {
        let home = home();
        let project_a = home.join("project-a");
        let project_b = home.join("project-b");
        for (project, marker) in [(&project_a, "a"), (&project_b, "b")] {
            fs::create_dir(project).unwrap();
            fs::write(
                project.join(".mcp.json"),
                serde_json::json!({"mcpServers":{},"project":marker}).to_string(),
            )
            .unwrap();
        }
        let (capture, mut services) = tokio::sync::watch::channel(None);
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        // The process loader is a handle the loader provides, not a name.
        builder.require_feature(
            Feature::new("test.process-settings").session("test.process-settings", move |names| {
                Ok(Box::new(ProcessSettings::new(names, capture.clone())?))
            }),
        );
        let server = serve(
            builder,
            ServerConfig {
                product_home: Some(home.clone()),
                target_home: Some(home.join("runtime")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let _session = server
            .agent()
            .unwrap()
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let (settings, loader) = services.borrow().clone().unwrap();
        let cancel = CancellationToken::new();
        let root_a = settings
            .authorize_project(project_a.clone(), &cancel)
            .await
            .unwrap();
        let expected = settings
            .read::<McpSettings>(SettingsScope::Project(root_a), &cancel)
            .await
            .unwrap();
        loader.restart("FileStorage").await.unwrap();
        let current = services
            .wait_for(|value| {
                value
                    .as_ref()
                    .is_some_and(|(current, _)| !std::sync::Arc::ptr_eq(current, &settings))
            })
            .await
            .unwrap()
            .as_ref()
            .unwrap()
            .0
            .clone();
        assert!(matches!(
            settings
                .read::<McpSettings>(SettingsScope::Project(root_a), &cancel)
                .await,
            Err(SettingsError::Closed)
        ));
        assert_eq!(current.project_root(&project_a), Some(root_a));
        let root_b = current
            .authorize_project(project_b.clone(), &cancel)
            .await
            .unwrap();
        assert_ne!(root_a, root_b);
        assert_eq!(
            current.authorize_project(project_a, &cancel).await.unwrap(),
            root_a
        );
        assert!(
            current
                .read::<McpSettings>(SettingsScope::Project(root_a), &cancel)
                .await
                .unwrap()
                == expected
        );
        assert!(
            current
                .read::<McpSettings>(SettingsScope::Project(root_b), &cancel)
                .await
                .unwrap()
                .document
                != expected.document
        );
        server.shutdown();
        server.join().await.unwrap();
        fs::remove_dir_all(home).unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn late_workspace_authorization_and_settings_survive_dependency_reload() {
    use dim_app_server::mcp::config::McpSettings;
    let home = home();
    let root = Ctx::root().unwrap();
    let views = vec![
        root.plugin(FileStoragePlugin::new(vec![home.clone()])),
        root.plugin(settings_part(0, vec![])),
    ];
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let settings = root.get::<SettingsService>().unwrap();
    let project = home.join("later");
    fs::create_dir(&project).unwrap();
    assert_eq!(settings.project_root(&project), None);
    let mut authorized = listen::<ProjectAuthorized>(&root, &project_authorizations());
    let (first, same) = tokio::join!(
        settings.authorize_project(project.clone(), &cancel),
        settings.authorize_project(project.clone(), &cancel)
    );
    let project_root = first.unwrap();
    assert_eq!(project_root, same.unwrap());
    // Authorizing the same directory twice announces it once. Events of one
    // channel arrive in emit order, so a later authorization is a barrier.
    let barrier = home.join("barrier");
    fs::create_dir(&barrier).unwrap();
    settings
        .authorize_project(barrier.clone(), &cancel)
        .await
        .unwrap();
    saw(&mut authorized, |a: &ProjectAuthorized| a.cwd == barrier).await;
    let announced: Vec<_> = authorized
        .borrow()
        .iter()
        .filter(|a| a.cwd == project)
        .map(|a| a.root)
        .collect();
    assert_eq!(announced, [project_root]);
    let scope = SettingsScope::Project(project_root);
    let snapshot = settings.read::<McpSettings>(scope, &cancel).await.unwrap();
    let document = serde_json::from_value::<McpSettings>(serde_json::json!({"mcpServers":{"late":{"command":"node","enabled":false}},"future":{"keep":7}})).unwrap();
    let saved = settings
        .update(scope, &snapshot.revision, document, &cancel)
        .await
        .unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        settings.authorize_project(home.clone(), &cancelled).await,
        Err(SettingsError::Cancelled)
    ));
    assert_eq!(settings.project_root(&home), None);
    for plugin in [&views[1], &views[0]] {
        let old = root.get::<SettingsService>().unwrap();
        plugin.restart().await.unwrap();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let current = root.get::<SettingsService>().unwrap();
        assert!(matches!(
            old.authorize_project(project.clone(), &cancel).await,
            Err(SettingsError::Closed)
        ));
        assert_eq!(current.project_root(&project), Some(project_root));
        assert_eq!(
            current
                .authorize_project(project.clone(), &cancel)
                .await
                .unwrap(),
            project_root
        );
        assert!(current.read::<McpSettings>(scope, &cancel).await.unwrap() == saved);
    }
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Document {
    value: String,
}
impl BusinessDocument for Document {
    const NAMESPACE: &'static str = "example";
    const USER_FILE: &'static str = "example.json";
    const PROJECT_FILE: Option<&'static str> = Some(".example.json");
    fn validate(&self) -> Result<()> {
        if self.value == "invalid" {
            Err(SettingsError::Invalid("invalid example"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Other {
    value: String,
}
impl BusinessDocument for Other {
    const NAMESPACE: &'static str = "other";
    const USER_FILE: &'static str = "other.json";
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}
fn home() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("dim-settings-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path.canonicalize().unwrap()
}

#[tokio::test]
async fn native_import_plain_v13_cas_failure_and_restart() {
    let home = home();
    let database = home.join("settings.sqlite");
    let original = br#"{"value":"original"}"#;
    fs::write(home.join("example.json"), original).unwrap();
    fs::write(home.join("other.json"), b"broken").unwrap();
    let cancel = CancellationToken::new();
    let mut saved = None;
    for restarted in [false, true] {
        let root = Ctx::root().unwrap();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![home.clone()])),
            root.plugin(SqliteStoragePlugin::new(database.clone())),
            root.plugin(native_settings_part(0, vec![])),
        ];
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let settings = root.get::<SettingsService>().unwrap();
        let mut changes = listen::<SettingsChanged>(&root, &settings_changes("example"));
        assert!(changes.borrow_and_update().is_empty());
        let current = settings
            .read::<Document>(SettingsScope::User, &cancel)
            .await
            .unwrap();
        assert!(!changes.has_changed().unwrap());
        let db = rusqlite::Connection::open(&database).unwrap();
        if restarted {
            assert_eq!(Some(current), saved);
        } else {
            assert_eq!(current.revision, "native:user/example:1");
            assert_eq!(current.document.value, "original");
            let (body, imported): (String, String) = db.query_row("SELECT document, imported_revision FROM settings_documents WHERE source='user/example'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!(
                serde_json::from_str::<Document>(&body).unwrap(),
                current.document
            );
            assert_eq!(imported.len(), 64);
            assert_eq!(fs::read(home.join("example.json")).unwrap(), original);
            // A corrupt optional source does not block plugin assembly or another namespace.
            assert!(matches!(
                settings.read::<Other>(SettingsScope::User, &cancel).await,
                Err(SettingsError::Invalid(_))
            ));
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM settings_documents WHERE source='user/other'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            fs::write(home.join("other.json"), br#"{"value":"repaired"}"#).unwrap();
            let other = settings
                .read::<Other>(SettingsScope::User, &cancel)
                .await
                .unwrap();
            settings
                .update(
                    SettingsScope::User,
                    &other.revision,
                    Other::default(),
                    &cancel,
                )
                .await
                .unwrap();
            assert!(!changes.has_changed().unwrap());
            db.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE ON settings_documents BEGIN SELECT RAISE(ABORT, 'save failed'); END").unwrap();
            let next = Document {
                value: "saved".into(),
            };
            assert!(matches!(
                settings
                    .update(
                        SettingsScope::User,
                        &current.revision,
                        next.clone(),
                        &cancel
                    )
                    .await,
                Err(SettingsError::Persistence)
            ));
            assert_eq!(
                settings
                    .read::<Document>(SettingsScope::User, &cancel)
                    .await
                    .unwrap(),
                current
            );
            assert!(!changes.has_changed().unwrap());
            db.execute_batch("DROP TRIGGER fail_save").unwrap();
            let (a, b) = tokio::join!(
                settings.update(
                    SettingsScope::User,
                    &current.revision,
                    next.clone(),
                    &cancel
                ),
                settings.update(SettingsScope::User, &current.revision, next, &cancel)
            );
            let winner = match (a, b) {
                (Ok(value), Err(SettingsError::Conflict))
                | (Err(SettingsError::Conflict), Ok(value)) => value,
                result => panic!("unexpected CAS: {result:?}"),
            };
            saw(&mut changes, change(SettingsScope::User, &winner.revision)).await;
            assert_eq!(winner.revision, "native:user/example:2");
            saved = Some(winner);
            fs::write(home.join("example.json"), b"ignored malformed legacy data").unwrap();
            // Explicitly seed the historical v13 payload-only contract and token.
            db.execute("INSERT INTO settings_documents(source,version,document,imported_revision) VALUES ('user/mcp',7,'{\"mcpServers\":{},\"extension\":\"v13\"}','missing')", []).unwrap();
            let compat = settings
                .read::<dim_app_server::mcp::config::McpSettings>(SettingsScope::User, &cancel)
                .await
                .unwrap();
            assert_eq!(compat.revision, "native:user/mcp:7");
            assert!(compat.document.mcp_servers.is_empty());
            assert_eq!(compat.document.extra["extension"], "v13");
            assert_eq!(
                db.query_row(
                    "SELECT imported_revision FROM settings_documents WHERE source='user/example'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                imported
            );
        }
        root.shutdown().await.unwrap();
        assert!(matches!(
            settings
                .read::<Document>(SettingsScope::User, &cancel)
                .await,
            Err(SettingsError::Closed)
        ));
    }
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn file_cas_project_isolation_notifications_and_accepted_work_ownership() {
    let home = home();
    let project = home.join("project");
    let second = home.join("second");
    fs::create_dir(&project).unwrap();
    fs::create_dir(&second).unwrap();
    let root = Ctx::root().unwrap();
    let views = vec![
        root.plugin(FileStoragePlugin::new(vec![
            home.clone(),
            project.clone(),
            second.clone(),
        ])),
        root.plugin(settings_part(0, vec![1, 2])),
    ];
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let settings = root.get::<SettingsService>().unwrap();
    let files = root.get::<FileStorage>().unwrap();
    let mut changes = listen::<SettingsChanged>(&root, &settings_changes("example"));
    let empty = settings
        .read::<Document>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    assert_eq!(empty.revision, "missing");
    let document = Document {
        value: "saved".into(),
    };
    let (a, b) = tokio::join!(
        settings.update(
            SettingsScope::User,
            &empty.revision,
            document.clone(),
            &cancel
        ),
        settings.update(
            SettingsScope::User,
            &empty.revision,
            document.clone(),
            &cancel
        )
    );
    let saved = match (a, b) {
        (Ok(value), Err(SettingsError::Conflict)) | (Err(SettingsError::Conflict), Ok(value)) => {
            value
        }
        result => panic!("unexpected file CAS: {result:?}"),
    };
    saw(&mut changes, change(SettingsScope::User, &saved.revision)).await;
    assert_eq!(settings.project_path(1).unwrap(), project);
    assert_eq!(settings.project_sources().len(), 2);
    assert!(matches!(
        settings
            .read::<Document>(SettingsScope::Project(0), &cancel)
            .await,
        Err(SettingsError::Invalid(_))
    ));
    let a = settings
        .read::<Document>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap();
    let b = settings
        .read::<Document>(SettingsScope::Project(2), &cancel)
        .await
        .unwrap();
    // The user write above; counted below, behind a barrier.
    let before = 1;
    let project_saved = settings
        .update(
            SettingsScope::Project(1),
            &a.revision,
            document.clone(),
            &cancel,
        )
        .await
        .unwrap();
    // Each write is announced once, with its own scope and revision.
    saw(
        &mut changes,
        change(SettingsScope::Project(1), &project_saved.revision),
    )
    .await;
    assert_eq!(
        settings
            .read::<Document>(SettingsScope::Project(2), &cancel)
            .await
            .unwrap(),
        b
    );
    let other_saved = settings
        .update(
            SettingsScope::Project(2),
            &b.revision,
            Document {
                value: "other project".into(),
            },
            &cancel,
        )
        .await
        .unwrap();
    saw(
        &mut changes,
        change(SettingsScope::Project(2), &other_saved.revision),
    )
    .await;
    // One source's events arrive in emit order, so the last write is a
    // barrier: one event per write, none from the conflicting writer.
    assert_eq!(changes.borrow().len(), before + 2);
    assert_eq!(
        settings
            .read::<Document>(SettingsScope::Project(1), &cancel)
            .await
            .unwrap()
            .revision,
        project_saved.revision
    );
    assert!(!changes.has_changed().unwrap());
    fs::write(project.join(".example.json"), b"corrupt").unwrap();
    assert!(matches!(
        settings
            .read::<Document>(SettingsScope::Project(1), &cancel)
            .await,
        Err(SettingsError::Invalid(_))
    ));
    fs::write(project.join(".example.json"), br#"{"value":"repaired"}"#).unwrap();
    assert_eq!(
        settings
            .read::<Document>(SettingsScope::Project(1), &cancel)
            .await
            .unwrap()
            .document
            .value,
        "repaired"
    );
    fs::write(home.join("example.json"), br#"{"value":"external"}"#).unwrap();
    assert!(matches!(
        settings
            .update(
                SettingsScope::User,
                &saved.revision,
                document.clone(),
                &cancel
            )
            .await,
        Err(SettingsError::Conflict)
    ));
    let current = settings
        .read::<Document>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    let path = files.path(0, "example.json").unwrap();
    let lock = files.lock(&path, &cancel).await.unwrap();
    let stopped = CancellationToken::new();
    let update = settings.update(
        SettingsScope::User,
        &current.revision,
        document.clone(),
        &stopped,
    );
    tokio::pin!(update);
    tokio::select! { result = &mut update => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(50)) => {} }
    stopped.cancel();
    assert!(matches!(update.await, Err(SettingsError::Cancelled)));
    assert!(!changes.has_changed().unwrap());
    // Dropping the caller does not discard admitted work or its commit notification.
    {
        let update = settings.update(
            SettingsScope::User,
            &current.revision,
            document.clone(),
            &cancel,
        );
        tokio::pin!(update);
        tokio::select! { result = &mut update => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(50)) => {} }
    }
    // Nothing can commit while the test holds the lease.
    let before = changes.borrow().len();
    drop(lock);
    tokio::time::timeout(
        Duration::from_secs(5),
        changes.wait_for(|log| log.len() > before),
    )
    .await
    .unwrap()
    .unwrap();
    let saved = settings
        .read::<Document>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    assert_eq!(
        changes.borrow().last().map(|c| c.revision.as_str()),
        Some(saved.revision.as_str())
    );
    assert_eq!(saved.document, document);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.join("example.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    // Unload cancels lease waiters and drains them before releasing ownership.
    // The event is published under the writer's lease, so it may still be held.
    let lock = lock_when_free(&files, &path, &cancel).await;
    {
        let update = settings.update(
            SettingsScope::User,
            &saved.revision,
            Document::default(),
            &cancel,
        );
        tokio::pin!(update);
        tokio::select! { result = &mut update => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(50)) => {} }
    }
    views[1].dispose().await.unwrap();
    drop(lock);
    assert!(!changes.has_changed().unwrap());
    assert!(matches!(
        settings
            .update(
                SettingsScope::User,
                &saved.revision,
                Document::default(),
                &cancel
            )
            .await,
        Err(SettingsError::Closed)
    ));
    let view = root.plugin(settings_part(0, vec![1, 2]));
    wait_active(&root, &[view], Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let fresh = root.get::<SettingsService>().unwrap();
    assert_eq!(
        fresh
            .read::<Document>(SettingsScope::User, &cancel)
            .await
            .unwrap(),
        saved
    );
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn business_documents_reject_escaping_filenames() {
    let home = home();
    let root = Ctx::root().unwrap();
    let views = vec![
        root.plugin(FileStoragePlugin::new(vec![home.clone()])),
        root.plugin(settings_part(0, vec![])),
    ];
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let settings = root.get::<SettingsService>().unwrap();
    #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
    struct BadPath;
    impl BusinessDocument for BadPath {
        const NAMESPACE: &'static str = "bad";
        const USER_FILE: &'static str = "../escape.json";
        fn validate(&self) -> Result<()> {
            Ok(())
        }
    }
    assert!(matches!(
        settings.read::<BadPath>(SettingsScope::User, &cancel).await,
        Err(SettingsError::Invalid(_))
    ));
    // A namespace or file belongs to the first document type that used it.
    #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
    struct SameNamespace;
    impl BusinessDocument for SameNamespace {
        const NAMESPACE: &'static str = "example";
        const USER_FILE: &'static str = "same-namespace.json";
        fn validate(&self) -> Result<()> {
            Ok(())
        }
    }
    #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
    struct SameFile;
    impl BusinessDocument for SameFile {
        const NAMESPACE: &'static str = "same-file";
        const USER_FILE: &'static str = "example.json";
        fn validate(&self) -> Result<()> {
            Ok(())
        }
    }
    settings
        .read::<Document>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    for result in [
        settings
            .read::<SameNamespace>(SettingsScope::User, &cancel)
            .await
            .map(drop),
        settings
            .read::<SameFile>(SettingsScope::User, &cancel)
            .await
            .map(drop),
    ] {
        assert!(matches!(result, Err(SettingsError::Invalid(_))));
    }
    settings
        .read::<Document>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// The source the acceptance flows declare (settings-service §11): user and
/// project files, a schema with a `default`, one user-only member.
fn declaration(source: &str) -> SourceDeclaration {
    SourceDeclaration {
        owner: "example-package".into(),
        source: source.into(),
        user_file: format!("{source}.json"),
        project_file: Some(format!(".{source}.json")),
        schema: json!({
            "type": "object",
            "properties": {
                "searchService": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string"},
                        "apiKey": {"type": "string"}
                    },
                    "required": ["kind"]
                },
                "maxResults": {"type": "integer", "minimum": 1, "default": 5}
            },
            "additionalProperties": false
        }),
        user_only: vec!["/searchService".into()],
    }
}
/// One call of the owner service `settings`, as any language makes it.
async fn owner_call(dispatch: &Arc<dyn HostDispatch>, method: &str, request: Json) -> Json {
    match dispatch
        .invoke(method, RpcValue::List(vec![request.into()]))
        .unwrap()
    {
        RpcValue::Reference(reply) if reply.is_future() => {
            reply.wait_async().await.unwrap().json().unwrap()
        }
        value => value.json().unwrap(),
    }
}

#[tokio::test]
async fn declared_source_owner_and_client_writes_in_both_profiles() {
    for native in [false, true] {
        let home = home();
        let project = home.join("project");
        fs::create_dir(&project).unwrap();
        let root = Ctx::root().unwrap();
        let settings_plugin = if native {
            native_settings_part(0, vec![])
        } else {
            settings_part(0, vec![])
        };
        let mut views = vec![root.plugin(FileStoragePlugin::new(vec![home.clone()]))];
        if native {
            views.push(root.plugin(SqliteStoragePlugin::new(home.join("settings.sqlite"))));
        }
        views.push(root.plugin(settings_plugin));
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let settings = root.get::<SettingsService>().unwrap();
        // The owner registers its source (settings-service §3.1).
        settings.register(&root, declaration("example")).unwrap();
        let owner = root
            .get_at::<dyn HostDispatch>(&host_key("settings"))
            .unwrap();
        let mut changes = listen::<SettingsChanged>(&root, &settings_changes("example"));
        let user = json!({"source": "example", "scope": "user"});

        // The first read is the default document.
        let first = owner_call(&owner, "read", user.clone()).await;
        assert_eq!(first["ok"], true);
        assert_eq!(first["value"]["document"], json!({"maxResults": 5}));
        let first = first["value"]["revision"].as_str().unwrap().to_owned();
        let initial = if native {
            "native:user/example:1"
        } else {
            "missing"
        };
        assert_eq!(first, initial);
        let update = |revision: &str, document: Json| json!({"source": "example", "scope": "user", "expectedRevision": revision, "document": document});
        let saved = owner_call(&owner, "update", update(&first, json!({"maxResults": 7}))).await;
        assert_eq!(saved["value"]["document"], json!({"maxResults": 7}));
        let second = saved["value"]["revision"].as_str().unwrap().to_owned();
        assert_ne!(second, first);
        // Rejected writes change nothing and announce nothing.
        for (document, code) in [
            (json!({"maxResults": 8}), "conflict"),
            (json!({"maxResults": 0}), "invalid_document"),
            (json!({"searchService": {"kind": "web"}}), "user_only_field"),
            (json!([]), "invalid_document"),
        ] {
            let revision = if code == "conflict" { &first } else { &second };
            let rejected = owner_call(&owner, "update", update(revision, document)).await;
            assert_eq!(rejected["ok"], false);
            assert_eq!(rejected["error"]["code"], code, "{rejected}");
        }
        assert_eq!(
            owner_call(&owner, "read", user.clone()).await["value"]["revision"],
            second.as_str()
        );
        // A client write may change the user-only member; the owner may then
        // keep it, but not change it.
        let service = json!({"kind": "web", "apiKey": "web-search"});
        let third = settings
            .update_declared(
                "example",
                SettingsScope::User,
                &second,
                json!({"maxResults": 7, "searchService": service}),
                Writer::Client,
                &cancel,
            )
            .await
            .unwrap()
            .revision;
        let changed = owner_call(
            &owner,
            "update",
            update(&third, json!({"searchService": {"kind": "other"}})),
        )
        .await;
        assert_eq!(changed["error"]["code"], "user_only_field");
        let kept = owner_call(
            &owner,
            "update",
            update(&third, json!({"searchService": service})),
        )
        .await;
        let stored = json!({"maxResults": 5, "searchService": service});
        assert_eq!(kept["value"]["document"], stored);
        let fourth = kept["value"]["revision"].as_str().unwrap().to_owned();
        // One event per committed write, in commit order, on both paths.
        saw(&mut changes, change(SettingsScope::User, &fourth)).await;
        let announced: Vec<_> = changes
            .borrow()
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect();
        assert_eq!(
            announced,
            [&second, &third, &fourth].map(
                |revision| json!({"source": "example", "scope": "user", "revision": revision})
            )
        );
        if native {
            let db = rusqlite::Connection::open(home.join("settings.sqlite")).unwrap();
            let body: String = db
                .query_row(
                    "SELECT document FROM settings_documents WHERE source='user/example'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(serde_json::from_str::<Json>(&body).unwrap(), stored);
            assert!(!home.join("example.json").exists());
        } else {
            let file = fs::read(home.join("example.json")).unwrap();
            assert_eq!(serde_json::from_slice::<Json>(&file).unwrap(), stored);
        }

        // Project scope needs an authorized root; `projects` lists them.
        let unauthorized = owner_call(
            &owner,
            "read",
            json!({"source": "example", "scope": {"project": 99}}),
        )
        .await;
        assert_eq!(unauthorized["error"]["code"], "invalid_scope");
        let mut authorized = listen::<ProjectAuthorized>(&root, &project_authorizations());
        let project_root = settings
            .authorize_project(project.clone(), &cancel)
            .await
            .unwrap();
        saw(&mut authorized, |a: &ProjectAuthorized| {
            a.root == project_root
        })
        .await;
        let listed = json!({"root": project_root, "cwd": project});
        assert_eq!(
            serde_json::to_value(&authorized.borrow()[0]).unwrap(),
            listed
        );
        assert_eq!(
            owner_call(&owner, "projects", json!({})).await,
            json!({"ok": true, "value": {"projects": [listed]}})
        );
        let in_project = owner_call(
            &owner,
            "read",
            json!({"source": "example", "scope": {"project": project_root}}),
        )
        .await;
        assert_eq!(
            in_project["value"],
            json!({"revision": "missing", "document": {"maxResults": 5}})
        );

        // Only declared sources are served, and a Rust type cannot take one.
        let mcp = owner_call(&owner, "read", json!({"source": "mcp", "scope": "user"})).await;
        assert_eq!(mcp["error"]["code"], "unknown_source");
        assert!(matches!(
            settings
                .read::<Document>(SettingsScope::User, &cancel)
                .await,
            Err(SettingsError::Invalid(
                "settings source belongs to another document type"
            ))
        ));
        root.shutdown().await.unwrap();
        assert_eq!(
            owner_call(&owner, "read", user).await["error"]["code"],
            "closed"
        );
        fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test]
async fn invalid_or_conflicting_registrations_are_refused_and_withdrawn_ones_return() {
    let mut unreachable = declaration("example");
    unreachable.user_only = vec!["/searchService/missing".into()];
    let mut invalid_default = declaration("example");
    invalid_default.schema["properties"]["maxResults"]["default"] = json!(0);
    let mut escaping = declaration("example");
    escaping.user_file = "../example.json".into();
    let home = home();
    let root = Ctx::root().unwrap();
    let views = vec![
        root.plugin(FileStoragePlugin::new(vec![home.clone()])),
        root.plugin(settings_part(0, vec![]).reserve::<dim_app_server::mcp::config::McpSettings>()),
    ];
    wait_active(
        &root,
        &views,
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let settings = root.get::<SettingsService>().unwrap();
    for (declaration, reason) in [
        (declaration("mcp"), "another document type"),
        (unreachable, "does not name a declared member"),
        (invalid_default, "default document is invalid"),
        (escaping, ""),
    ] {
        let error = settings.register(&root, declaration).unwrap_err();
        assert!(error.contains(reason), "{error}");
    }
    // A source is one registration's; it returns once that one is withdrawn.
    let first = root.plugin(Registrant(settings.clone()));
    wait_active(
        &root,
        std::slice::from_ref(&first),
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let duplicate = settings
        .register(&root, declaration("example"))
        .unwrap_err();
    assert!(duplicate.contains("already registered"), "{duplicate}");
    first.dispose().await.unwrap();
    let cancel = CancellationToken::new();
    assert!(matches!(
        settings
            .read_declared("example", SettingsScope::User, &cancel)
            .await,
        Err(SettingsError::UnknownSource)
    ));
    settings.register(&root, declaration("example")).unwrap();
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// A plugin that registers the source `example` while it runs.
struct Registrant(Arc<SettingsService>);
impl rutis::host::DynPlugin for Registrant {
    fn name(&self) -> &str {
        "test.settings-registrant"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            self.0
                .register(ctx, declaration("example"))
                .map_err(|error| {
                    rutis::Error::from(rutis::host::ErrorKind::PluginFailed(error.into()))
                })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}
