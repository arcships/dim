use dim_agent_capabilities::{
    file_storage::{FileLease, FilePath, FileStorage, FileStorageError, FileStoragePlugin},
    methods::{Owner as MethodOwner, RpcError},
};
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_app_server::{
    methods::{ManagementMethods, ManagementMethodsPlugin},
    settings::{
        BusinessDocument, ProjectAuthorized, Result, SettingsChanged, SettingsError,
        SettingsPlugin, SettingsScope, SettingsService, SourceDeclaration, Writer,
        project_authorizations, settings_changes, without_write_only,
    },
    settings_methods::SettingsMethodsPlugin,
    storage::SqliteStoragePlugin,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberView, Key, TypeKey};
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

type Register = Arc<dyn Fn(&SettingsService, &Ctx) -> rutis::Result<()> + Send + Sync>;
/// A plugin that owns sources: it registers them with Settings in its
/// `apply`, so they are withdrawn when it unloads.
struct Owner {
    name: &'static str,
    settings: Key<SettingsService>,
    register: Register,
}
fn owner(
    name: &'static str,
    register: impl Fn(&SettingsService, &Ctx) -> rutis::Result<()> + Send + Sync + 'static,
) -> Owner {
    let catalog = dim_app_server::catalog();
    let build = Build::default();
    Owner {
        name,
        settings: Names::copy(&catalog, &build).key().unwrap(),
        register: Arc::new(register),
    }
}
impl DynPlugin for Owner {
    fn name(&self) -> &str {
        self.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.settings.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            (self.register)(&settings, ctx)?;
            Ok(Effect::Done)
        })
    }
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
        root.plugin(owner("test.mcp", |settings, ctx| {
            settings.register::<McpSettings>(ctx)
        })),
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
            root.plugin(owner("test.documents", |settings, ctx| {
                settings.register::<Document>(ctx)?;
                settings.register::<Other>(ctx)?;
                settings.register::<dim_app_server::mcp::config::McpSettings>(ctx)
            })),
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
            db.execute("INSERT INTO settings_documents(source,version,document,imported_revision) VALUES ('user/mcp',7,'{\"mcpServers\":{},\"extension\":\"v13\"}','missing')", []).unwrap();
            let compat = settings
                .read::<dim_app_server::mcp::config::McpSettings>(SettingsScope::User, &cancel)
                .await
                .unwrap();
            assert_eq!(compat.revision, "native:user/mcp:7");
            assert!(compat.document.entries().is_empty());
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
        root.plugin(owner("test.document", |settings, ctx| {
            settings.register::<Document>(ctx)
        })),
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
    wait_active(
        &root,
        &[view, views[2].clone()],
        Duration::from_secs(5),
        &cancel,
    )
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
async fn sources_appear_while_registered_and_names_are_exclusive() {
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
    let dispatch = root
        .get_at::<dyn HostDispatch>(&host_key("settings"))
        .unwrap();
    assert!(matches!(
        settings
            .read::<Document>(SettingsScope::User, &cancel)
            .await,
        Err(SettingsError::UnknownSource)
    ));
    let read = json!({"source": "declared", "scope": "user"});
    assert_eq!(
        owner_call(&dispatch, "read", read.clone()).await["error"]["code"],
        "unknown_source"
    );
    let source = root.plugin(owner("test.owner", |settings, ctx| {
        settings.register::<Document>(ctx)?;
        settings.register_declared(ctx, declaration("declared"))
    }));
    wait_active(
        &root,
        std::slice::from_ref(&source),
        Duration::from_secs(5),
        &cancel,
    )
    .await
    .unwrap();
    let empty = settings
        .read::<Document>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    let saved = settings
        .update(
            SettingsScope::User,
            &empty.revision,
            Document {
                value: "kept".into(),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        owner_call(&dispatch, "read", read.clone()).await["ok"],
        true
    );
    #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
    struct BadPath;
    impl BusinessDocument for BadPath {
        const NAMESPACE: &'static str = "bad";
        const USER_FILE: &'static str = "../escape.json";
        fn validate(&self) -> Result<()> {
            Ok(())
        }
    }
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
    assert!(settings.register::<BadPath>(&root).is_err());
    assert!(settings.register::<SameNamespace>(&root).is_err());
    assert!(settings.register::<SameFile>(&root).is_err());
    assert!(settings.register::<Document>(&root).is_err());
    assert!(
        settings
            .register_declared(&root, declaration("example"))
            .is_err()
    );
    assert!(
        settings
            .read::<SameNamespace>(SettingsScope::User, &cancel)
            .await
            .is_err()
    );
    source.dispose().await.unwrap();
    assert!(matches!(
        settings
            .read::<Document>(SettingsScope::User, &cancel)
            .await,
        Err(SettingsError::UnknownSource)
    ));
    assert_eq!(
        owner_call(&dispatch, "read", read).await["error"]["code"],
        "unknown_source"
    );
    assert!(home.join("example.json").exists());
    settings.register::<Document>(&root).unwrap();
    assert_eq!(
        settings
            .read::<Document>(SettingsScope::User, &cancel)
            .await
            .unwrap(),
        saved
    );
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// The source the acceptance flows declare (settings-service §11): user and
/// project files, a schema with a `default`, one user-only member.
fn declaration(source: &str) -> SourceDeclaration {
    SourceDeclaration {
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

/// The process parts of the client path (`settings.*`), as assembly mounts
/// them: the method table and `SettingsMethods` beside Settings.
fn client_sources(home: &std::path::Path, root: &Ctx) -> Vec<FiberView> {
    let catalog = dim_app_server::catalog();
    let build = Build::default();
    vec![
        root.plugin(FileStoragePlugin::new(vec![home.to_owned()])),
        root.plugin(settings_part(0, vec![])),
        root.plugin(ManagementMethodsPlugin::typed(&catalog).unwrap()),
        root.plugin(SettingsMethodsPlugin::new(&Names::copy(&catalog, &build)).unwrap()),
    ]
}

/// One `settings.*` management call, as the Rpc layer makes it.
async fn client_call(
    root: &Ctx,
    method: &str,
    params: Json,
) -> std::result::Result<Json, RpcError> {
    let table = root.get::<ManagementMethods>().unwrap();
    let provider = match table.owner(method) {
        MethodOwner::Active(provider) => provider,
        _ => panic!("{method} has no active provider"),
    };
    provider
        .accept(
            method.to_owned(),
            params,
            None,
            CancellationToken::new(),
            |_| {},
        )
        .unwrap()
        .reply
        .await
        .unwrap()
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
        views.push(root.plugin(owner("test.declared", |settings, ctx| {
            settings.register_declared(ctx, declaration("example"))
        })));
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let settings = root.get::<SettingsService>().unwrap();
        let owner = root
            .get_at::<dyn HostDispatch>(&host_key("settings"))
            .unwrap();
        let mut changes = listen::<SettingsChanged>(&root, &settings_changes("example"));
        let user = json!({"source": "example", "scope": "user"});

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

        let mcp = owner_call(&owner, "read", json!({"source": "mcp", "scope": "user"})).await;
        assert_eq!(mcp["error"]["code"], "unknown_source");
        root.shutdown().await.unwrap();
        assert_eq!(
            owner_call(&owner, "read", user).await["error"]["code"],
            "closed"
        );
        fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test]
async fn invalid_or_conflicting_registrations_fail_the_owner() {
    let mut unreachable = declaration("example");
    unreachable.user_only = vec!["/searchService/missing".into()];
    let mut invalid_default = declaration("example");
    invalid_default.schema["properties"]["maxResults"]["default"] = json!(0);
    let mut escaping = declaration("example");
    escaping.user_file = "../example.json".into();
    let cases: Vec<(Vec<SourceDeclaration>, &str)> = vec![
        (
            vec![declaration("mcp")],
            "settings source mcp: its name or file is registered already",
        ),
        (
            vec![declaration("example"), declaration("example")],
            "settings source example: its name or file is registered already",
        ),
        (
            vec![unreachable],
            "settings source example: user-only pointer /searchService/missing does not name a declared member",
        ),
        (
            vec![invalid_default],
            "settings source example: default document is invalid",
        ),
        (
            vec![escaping],
            "settings source example: invalid settings: invalid document filename",
        ),
    ];
    let home = home();
    for (index, (declarations, reason)) in cases.into_iter().enumerate() {
        let root = Ctx::root().unwrap();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![home.clone()])),
            root.plugin(settings_part(0, vec![])),
            root.plugin(owner("test.declared", move |settings, ctx| {
                if index == 0 {
                    settings.register::<dim_app_server::mcp::config::McpSettings>(ctx)?;
                }
                for declaration in &declarations {
                    settings.register_declared(ctx, declaration.clone())?;
                }
                Ok(())
            })),
        ];
        let error = wait_active(
            &root,
            &views,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains(reason), "{error}");
        root.shutdown().await.unwrap();
    }
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn model_log_switch_starts_the_recorder() {
    let home = home();
    let root = Ctx::root().unwrap();
    let views = vec![
        root.plugin(FileStoragePlugin::new(vec![home.clone()])),
        root.plugin(settings_part(0, vec![])),
        root.plugin(
            dim_app_server::model_log::ModelLogPlugin::new(
                &Names::copy(&dim_app_server::catalog(), &Build::default()),
                0,
            )
            .unwrap(),
        ),
    ];
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let settings = root.get::<SettingsService>().unwrap();
    let current = settings
        .read_declared("model-log", SettingsScope::User, &cancel)
        .await
        .unwrap();
    assert_eq!(current.document, json!({"enabled": false}));
    settings
        .update_declared(
            "model-log",
            SettingsScope::User,
            &current.revision,
            json!({"enabled": true}),
            Writer::Client,
            &cancel,
        )
        .await
        .unwrap();
    let recording = home.join("model-log").join("recordings.jsonl");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !recording.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the switch did not start the recorder"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::fs::metadata(home.join("model-log")).unwrap();
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        let file = std::fs::metadata(&recording).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
    }
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.3: with nothing registered, `settings.sources` replies an empty
/// listing, not an error.
#[tokio::test]
async fn client_settings_sources_lists_nothing_when_empty() {
    let home = home();
    let root = Ctx::root().unwrap();
    let views = client_sources(&home, &root);
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    assert_eq!(
        client_call(&root, "settings.sources", json!({}))
            .await
            .unwrap(),
        json!({
            "dialect": "https://json-schema.org/draft/2020-12/schema",
            "sources": [],
        })
    );
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.3: a declared source is listed with its schema verbatim (the `default`
/// members included), its scopes and its registered user-only pointers.
#[tokio::test]
async fn client_settings_sources_lists_a_registered_source_verbatim() {
    let home = home();
    let root = Ctx::root().unwrap();
    let mut views = client_sources(&home, &root);
    let owner = root.plugin(owner("test.sources-one", |settings, ctx| {
        settings.register_declared(ctx, declaration("workflow"))
    }));
    views.push(owner.clone());
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let expected = declaration("workflow");
    let reply = client_call(&root, "settings.sources", json!({}))
        .await
        .unwrap();
    assert_eq!(
        reply,
        json!({
            "dialect": "https://json-schema.org/draft/2020-12/schema",
            "sources": [{
                "source": "workflow",
                "schema": expected.schema,
                "scopes": ["user", "project"],
                "userOnly": ["/searchService"],
                "writeOnly": false,
            }],
        })
    );
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.3: a second declared source shaped like `web` carries its own
/// `writeOnly` hint, read from its schema.
#[tokio::test]
async fn client_settings_sources_carries_the_write_only_hint() {
    let home = home();
    let root = Ctx::root().unwrap();
    let mut views = client_sources(&home, &root);
    let owner = root.plugin(owner("test.sources-web", |settings, ctx| {
        settings.register_declared(ctx, declaration("workflow"))?;
        settings.register_declared(
            ctx,
            SourceDeclaration {
                source: "web".into(),
                user_file: "web.json".into(),
                project_file: None,
                schema: json!({
                    "type": "object",
                    "properties": {
                        "searchService": {
                            "type": "object",
                            "properties": {
                                "apiKey": {"type": "string", "writeOnly": true}
                            },
                            "required": ["apiKey"]
                        }
                    },
                    "additionalProperties": false
                }),
                user_only: vec![],
            },
        )
    }));
    views.push(owner.clone());
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let reply = client_call(&root, "settings.sources", json!({}))
        .await
        .unwrap();
    let sources = reply["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2, "{reply}");
    let web = sources.iter().find(|item| item["source"] == "web").unwrap();
    assert_eq!(web["writeOnly"], true);
    assert_eq!(web["scopes"], json!(["user"]));
    assert_eq!(web["userOnly"], json!([]));
    let workflow = sources
        .iter()
        .find(|item| item["source"] == "workflow")
        .unwrap();
    assert_eq!(workflow["writeOnly"], false);
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.1: a root-level `writeOnly` counts for the `settings.sources` hint but
/// hides nothing. The listing says `writeOnly: true` while the redaction the
/// agent's `plugin config` path applies (§1.2) over the same schema removes
/// no member and reports an empty `writeOnlySet`.
#[tokio::test]
async fn client_settings_sources_root_write_only_hints_without_hiding() {
    let home = home();
    let root = Ctx::root().unwrap();
    let mut views = client_sources(&home, &root);
    let owner = root.plugin(owner("test.sources-root-write-only", |settings, ctx| {
        settings.register_declared(
            ctx,
            SourceDeclaration {
                source: "sealed".into(),
                user_file: "sealed.json".into(),
                project_file: None,
                schema: json!({
                    "type": "object",
                    "writeOnly": true,
                    "properties": {"token": {"type": "string"}},
                    "additionalProperties": false
                }),
                user_only: vec![],
            },
        )
    }));
    views.push(owner.clone());
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let reply = client_call(&root, "settings.sources", json!({}))
        .await
        .unwrap();
    let item = &reply["sources"][0];
    assert_eq!(item["source"], "sealed", "{reply}");
    assert_eq!(item["writeOnly"], true, "{reply}");
    assert_eq!(item["userOnly"], json!([]), "{reply}");
    let document = json!({"token": "t"});
    let (shown, set) = without_write_only(&item["schema"], document.clone());
    assert_eq!(shown, document);
    assert!(set.is_empty(), "{set:?}");
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.3: a Rust source is never listed, and a declared source leaves the
/// listing the moment its owner unloads, when `settings.read` answers
/// `settings_unknown_source` again.
#[tokio::test]
async fn client_settings_sources_follows_registration_lifetime() {
    let home = home();
    let root = Ctx::root().unwrap();
    let mut views = client_sources(&home, &root);
    let owner = root.plugin(owner("test.sources-lifetime", |settings, ctx| {
        settings.register::<dim_app_server::mcp::config::McpSettings>(ctx)?;
        settings.register_declared(ctx, declaration("declared"))
    }));
    views.push(owner.clone());
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let reply = client_call(&root, "settings.sources", json!({}))
        .await
        .unwrap();
    let sources = reply["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1, "{reply}");
    assert_eq!(sources[0]["source"], "declared");
    let rust = client_call(
        &root,
        "settings.read",
        json!({"source": "mcp", "scope": "user"}),
    )
    .await
    .unwrap_err();
    assert_eq!(rust.data.unwrap().app_error, "settings_unknown_source");
    owner.dispose().await.unwrap();
    assert_eq!(
        client_call(&root, "settings.sources", json!({}))
            .await
            .unwrap(),
        json!({"dialect": "https://json-schema.org/draft/2020-12/schema", "sources": []})
    );
    let unregistered = client_call(
        &root,
        "settings.read",
        json!({"source": "declared", "scope": "user"}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        unregistered.data.unwrap().app_error,
        "settings_unknown_source"
    );
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.3 wire hygiene: no item carries a file name, an owner or a document,
/// and `settings.sources` rejects unknown members.
#[tokio::test]
async fn client_settings_sources_wire_hygiene() {
    let home = home();
    let root = Ctx::root().unwrap();
    let mut views = client_sources(&home, &root);
    let owner = root.plugin(owner("test.sources-hygiene", |settings, ctx| {
        settings.register_declared(ctx, declaration("example"))
    }));
    views.push(owner.clone());
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let reply = client_call(&root, "settings.sources", json!({}))
        .await
        .unwrap();
    let item = &reply["sources"][0];
    for member in ["userFile", "projectFile", "owner", "document"] {
        assert!(
            item.get(member).is_none(),
            "{member} must not be in the listing"
        );
    }
    let rejected = client_call(&root, "settings.sources", json!({"x": 1}))
        .await
        .unwrap_err();
    assert_eq!(rejected.code, -32602);
    assert_eq!(rejected.data.unwrap().app_error, "invalid_params");
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}

/// §4.3: the listing is in source-name order whatever the registration order.
#[tokio::test]
async fn client_settings_sources_lists_in_name_order() {
    let home = home();
    let root = Ctx::root().unwrap();
    let mut views = client_sources(&home, &root);
    let owner = root.plugin(owner("test.sources-order", |settings, ctx| {
        for source in ["zeta", "alpha", "echo"] {
            settings.register_declared(ctx, declaration(source))?;
        }
        Ok(())
    }));
    views.push(owner.clone());
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let reply = client_call(&root, "settings.sources", json!({}))
        .await
        .unwrap();
    let names: Vec<_> = reply["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["source"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "echo", "zeta"]);
    root.shutdown().await.unwrap();
    fs::remove_dir_all(home).unwrap();
}
