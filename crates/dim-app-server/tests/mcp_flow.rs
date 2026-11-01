use dim_agent_capabilities::file_storage::{FileStorage, FileStoragePlugin};
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_app_server::{
    mcp::{
        McpChange, McpError, McpPlugin, McpService, McpSource,
        config::{McpServerSettings, McpSettings},
    },
    settings::{
        SettingsChanged, SettingsError, SettingsPlugin, SettingsScope, SettingsService,
        settings_changes,
    },
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberView, Key, TypeKey};
use rutis::{Ctx, Plugin};
use rutis_loader::resolve::Build;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[path = "fixtures/mcp_host.rs"]
mod mcp_host;

type Services = (Arc<McpService>, Arc<SettingsService>, Arc<FileStorage>);
struct Capture {
    services: Arc<OnceLock<Services>>,
    mcp: Key<McpService>,
    settings: Key<SettingsService>,
    files: Key<FileStorage>,
}
impl Capture {
    fn new(names: &Names<'_>, services: Arc<OnceLock<Services>>) -> rutis::Result<Self> {
        Ok(Self {
            services,
            mcp: names.key()?,
            settings: names.key()?,
            files: names.key()?,
        })
    }
}
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "McpTest"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.mcp.erase(), self.settings.erase(), self.files.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let mcp = ctx.require_at(&self.mcp)?;
            let settings = ctx.require_at(&self.settings)?;
            let files = ctx.require_at(&self.files)?;
            assert!(self.services.set((mcp, settings, files)).is_ok());
            Ok(Effect::Done)
        })
    }
}

struct Fixture {
    root: Ctx,
    publication: mcp_host::Host,
    loader: rutis_loader::Loader,
    plugin: FiberView,
    mcp: mcp_host::Directory,
    settings: Arc<SettingsService>,
    files: Arc<FileStorage>,
    home: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let home = std::env::temp_dir().join(format!("dim-mcp-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join("project")).unwrap();
        let home = fs::canonicalize(home).unwrap();
        fs::write(home.join("mcp.json"), serde_json::to_vec(&json!({
            "mcpServers": {
                "shared": {"command":"user-server", "env":{"TOKEN":"user-secret"}},
                "user-only": {"type":"http", "url":"https://example.test/mcp", "auth":{"type":"bearer", "token":"private"}, "future":{"keep":true}}
            },
            "extension": {"preserve":17}
        })).unwrap()).unwrap();
        fs::write(home.join("project/.mcp.json"), serde_json::to_vec(&json!({
            "mcpServers": {
                "shared": {"type":"sse", "url":"https://project.test/sse", "enabled":false, "headers":{"X-Key":"project-secret"}, "future":[1,2]}
            },
            "projectExtension": "keep"
        })).unwrap()).unwrap();
        Self::open(home, false).await
    }
    async fn open(home: PathBuf, reversed: bool) -> Self {
        let root = Ctx::root().unwrap();
        let catalog = dim_app_server::catalog();
        let build = Build::default();
        let names = Names::copy(&catalog, &build);
        root.plugin(
            dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
                .unwrap(),
        );
        let (publication, loader) = mcp_host::Host::start(&root).await;
        let capture = Arc::new(OnceLock::new());
        root.provide_as::<dim_agent_runtime::PartRegistry>(Arc::new(
            dim_agent_runtime::PartRegistry::default(),
        ))
        .unwrap();
        let plugin = root.plugin(McpPlugin::new(&names, 0).unwrap());
        let mut roots = vec![home.clone(), home.join("project")];
        if reversed {
            roots.reverse();
        }
        let views = vec![
            root.plugin(FileStoragePlugin::new(roots)),
            root.plugin(
                SettingsPlugin::new(&names, usize::from(reversed), vec![usize::from(!reversed)])
                    .unwrap(),
            ),
            plugin.clone(),
            root.plugin(Capture::new(&names, capture.clone()).unwrap()),
        ];
        wait_active(
            &root,
            &views,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let (mcp, settings, files) = capture.get().unwrap().clone();
        let mcp = mcp_host::Directory::new(mcp, &root, loader.clone());
        Self {
            root,
            publication,
            loader,
            plugin,
            mcp,
            settings,
            files,
            home,
        }
    }
    async fn close(&self) {
        self.publication.close().await;
        self.root.shutdown().await.unwrap();
        fs::remove_dir_all(&self.home).unwrap();
    }
}

#[tokio::test]
async fn loader_publication_is_scoped_secret_free_and_restart_stable() {
    use dim_app_server::mcp::server::McpServerState;
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let user = fixture
        .mcp
        .server(None, "shared".into(), &cancel)
        .await
        .unwrap();
    assert!(matches!(
        fixture.mcp.server(Some(1), "shared".into(), &cancel).await,
        Err(McpError::Disabled)
    ));
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(
        fixture
            .mcp
            .application(SettingsScope::User)
            .applied_revision,
        Some(catalog.user_revision)
    );
    assert_eq!(
        fixture
            .mcp
            .application(SettingsScope::Project(1))
            .applied_revision,
        catalog.project_revision
    );
    let rows = fixture.loader.layers()[0].patches[0]
        .insert
        .clone()
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "only the authority and immutable template are root rows"
    );
    assert_eq!(
        fixture.loader.layers().len(),
        1,
        "MCP must not replace root layers"
    );
    let public = format!("{:?}", fixture.loader.layers());
    for secret in [
        "user-secret",
        "project-secret",
        "private",
        "TOKEN",
        "X-Key",
        "user-server",
        "project.test",
    ] {
        assert!(
            !public.contains(secret),
            "generic loader state leaked {secret}"
        );
    }

    let revision = fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            fixture
                .mcp
                .catalog(Some(1), &cancel)
                .await
                .unwrap()
                .project_revision
                .unwrap(),
            "shared".into(),
            McpChange::Put(
                serde_json::from_value(json!({"type":"http", "url":"ftp://invalid.test/mcp"}))
                    .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture.mcp.server(Some(1), "shared".into(), &cancel).await,
        Err(McpError::Disabled)
    ));
    let application = fixture.mcp.application(SettingsScope::Project(1));
    assert_eq!(application.applied_revision, Some(revision.clone()));
    fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            revision.clone(),
            "shared".into(),
            McpChange::SetEnabled(true),
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture.mcp.server(Some(1), "shared".into(), &cancel).await,
        Err(McpError::Server(
            dim_app_server::mcp::server::McpServerError::Configuration(_)
        ))
    ));
    assert_eq!(
        fixture
            .mcp
            .catalog(Some(1), &cancel)
            .await
            .unwrap()
            .project_revision,
        Some(revision)
    );
    assert_eq!(user.state(), McpServerState::Disconnected);

    fixture.publication.close().await;
    fixture.root.shutdown().await.unwrap();
    assert_eq!(user.state(), McpServerState::Closed);
    let restarted = Fixture::open(fixture.home.clone(), true).await;
    restarted
        .mcp
        .server(None, "shared".into(), &cancel)
        .await
        .unwrap();
    let new_rows = restarted.loader.layers()[0].patches[0]
        .insert
        .clone()
        .unwrap();
    assert_eq!(rows, new_rows);
    assert!(
        restarted
            .mcp
            .catalog(Some(0), &cancel)
            .await
            .unwrap()
            .servers["shared"]
            .enabled()
    );
    assert!(
        restarted
            .mcp
            .server(Some(0), "shared".into(), &cancel)
            .await
            .is_err()
    );
    restarted.close().await;
}

#[tokio::test]
async fn project_entries_start_disabled_and_only_the_users_record_enables_them() {
    use dim_app_server::mcp::server::McpServerState;
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let config = json!({"command":"project-server", "args":["--first"], "env":{"SECRET":"do-not-export"}, "enabled":true});
    let project_file = fixture.home.join("project/.mcp.json");
    let write = |value: &Value| {
        fs::write(
            &project_file,
            json!({"mcpServers":{"shared":value}}).to_string(),
        )
        .unwrap()
    };
    write(&config);
    let grants = r#"{"version":1,"approved":{}}"#;
    fs::write(fixture.home.join("mcp-approvals.json"), grants).unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert!(!catalog.servers["shared"].enabled());
    assert!(matches!(
        fixture.mcp.server(Some(1), "shared".into(), &cancel).await,
        Err(McpError::Disabled)
    ));

    let written = fs::read(&project_file).unwrap();
    assert!(matches!(
        fixture
            .mcp
            .update(
                SettingsScope::Project(1),
                catalog.user_revision.clone(),
                "shared".into(),
                McpChange::SetEnabled(true),
                &cancel,
            )
            .await,
        Err(McpError::Settings(SettingsError::Conflict))
    ));
    assert!(matches!(
        fixture
            .mcp
            .update(
                SettingsScope::Project(1),
                catalog.project_revision.clone().unwrap(),
                "missing".into(),
                McpChange::SetEnabled(true),
                &cancel,
            )
            .await,
        Err(McpError::UnknownServer)
    ));
    set_project(&fixture.mcp, 1, true, &cancel).await.unwrap();
    assert!(fixture.mcp.catalog(Some(1), &cancel).await.unwrap().servers["shared"].enabled());
    let first = fixture
        .mcp
        .server(Some(1), "shared".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(fs::read(&project_file).unwrap(), written);
    let record = fs::read_to_string(fixture.home.join("mcp-projects.json")).unwrap();
    let workspace = fixture.home.join("project");
    assert_eq!(
        serde_json::from_str::<Value>(&record).unwrap(),
        json!({"enabled":{(workspace.to_str().unwrap()):["shared"]}})
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("mcp-approvals.json")).unwrap(),
        grants
    );

    let other = fixture.home.join("other");
    fs::create_dir(&other).unwrap();
    fs::write(
        other.join(".mcp.json"),
        json!({"mcpServers":{"shared":config}}).to_string(),
    )
    .unwrap();
    let other = fixture
        .settings
        .authorize_project(other, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .mcp
            .server(Some(other), "shared".into(), &cancel)
            .await,
        Err(McpError::Disabled)
    ));

    // ADR 003 D4: a changed project command runs without a new decision, and
    // a retained connection keeps its generation.
    let mut changed = config.clone();
    changed["args"] = json!(["--second"]);
    write(&changed);
    assert!(fixture.mcp.catalog(Some(1), &cancel).await.unwrap().servers["shared"].enabled());
    let second = fixture
        .mcp
        .server(Some(1), "shared".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(first.state(), McpServerState::Disconnected);
    drop((first, second));

    let record_path = fixture.home.join("mcp-projects.json");
    fs::write(&record_path, b"{invalid").unwrap();
    let degraded = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert!(!degraded.servers["shared"].enabled());
    assert!(matches!(
        fixture.mcp.server(Some(1), "shared".into(), &cancel).await,
        Err(McpError::Disabled)
    ));
    fixture
        .mcp
        .server(Some(1), "user-only".into(), &cancel)
        .await
        .unwrap();
    assert!(
        fixture
            .mcp
            .application(SettingsScope::Project(1))
            .error
            .is_some()
    );
    assert!(set_project(&fixture.mcp, 1, false, &cancel).await.is_err());
    fs::write(&record_path, &record).unwrap();

    fixture.publication.close().await;
    fixture.root.shutdown().await.unwrap();
    let restarted = Fixture::open(fixture.home.clone(), true).await;
    let mcp = restarted.mcp.clone();
    let set = |enabled: bool| {
        let mcp = mcp.clone();
        let cancel = cancel.clone();
        async move { set_project(&mcp, 0, enabled, &cancel).await }
    };
    let connection = mcp.server(Some(0), "shared".into(), &cancel).await.unwrap();
    set(false).await.unwrap();
    assert_eq!(connection.state(), McpServerState::Closed);
    assert!(matches!(
        mcp.server(Some(0), "shared".into(), &cancel).await,
        Err(McpError::Disabled)
    ));
    assert_eq!(
        fs::read_to_string(restarted.home.join("mcp-projects.json")).unwrap(),
        "{\n  \"enabled\": {}\n}\n"
    );

    set(true).await.unwrap();
    let revision = mcp
        .catalog(Some(0), &cancel)
        .await
        .unwrap()
        .project_revision
        .unwrap();
    let revision = mcp
        .update(
            SettingsScope::Project(0),
            revision,
            "shared".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    mcp.update(
        SettingsScope::Project(0),
        revision,
        "shared".into(),
        McpChange::Put(serde_json::from_value(config.clone()).unwrap()),
        &cancel,
    )
    .await
    .unwrap();
    assert!(!mcp.catalog(Some(0), &cancel).await.unwrap().servers["shared"].enabled());

    set(true).await.unwrap();
    let retained = mcp.server(Some(0), "shared".into(), &cancel).await.unwrap();
    let written = fs::read(&project_file).unwrap();
    let broken = || async {
        fs::write(&project_file, b"{broken").unwrap();
        assert!(mcp.catalog(Some(0), &cancel).await.is_err());
        tokio::time::timeout(Duration::from_secs(5), async {
            while mcp.application(SettingsScope::Project(0)).error.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the publisher reads the broken file");
    };
    broken().await;
    assert_eq!(retained.state(), McpServerState::Disconnected);
    mcp.update(
        SettingsScope::Project(0),
        "stale".into(),
        "shared".into(),
        McpChange::SetEnabled(false),
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(retained.state(), McpServerState::Closed);
    assert_eq!(fs::read(&project_file).unwrap(), b"{broken");
    fs::write(&project_file, &written).unwrap();
    assert!(!mcp.catalog(Some(0), &cancel).await.unwrap().servers["shared"].enabled());

    set(true).await.unwrap();
    let retained = mcp.server(Some(0), "shared".into(), &cancel).await.unwrap();
    broken().await;
    let record = restarted.home.join("mcp-projects.json");
    let kept = fs::read(&record).unwrap();
    fs::write(&record, b"{invalid").unwrap();
    assert!(mcp.catalog(Some(0), &cancel).await.is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        while retained.state() != McpServerState::Closed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("an unreadable record withdraws an unreadable file's entries");
    fs::write(&record, kept).unwrap();
    fs::write(&project_file, &written).unwrap();
    assert!(mcp.catalog(Some(0), &cancel).await.unwrap().servers["shared"].enabled());
    drop((connection, retained));
    restarted.close().await;
}

async fn set_project(
    mcp: &McpService,
    root: usize,
    enabled: bool,
    cancel: &CancellationToken,
) -> dim_app_server::mcp::Result<String> {
    let revision = mcp
        .catalog(Some(root), cancel)
        .await?
        .project_revision
        .unwrap();
    mcp.update(
        SettingsScope::Project(root),
        revision,
        "shared".into(),
        McpChange::SetEnabled(enabled),
        cancel,
    )
    .await
}

#[tokio::test]
async fn directory_preserves_scope_and_cas_through_plugin_management() {
    let fixture = Fixture::new().await;
    let mcp = &fixture.mcp;
    let cancel = CancellationToken::new();
    let user = mcp.catalog(None, &cancel).await.unwrap();
    assert!(user.project_revision.is_none());
    assert!(user.servers["shared"].enabled());
    let project = mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(project.servers.len(), 2);
    assert_eq!(project.servers["shared"].source, McpSource::Project(1));
    assert!(!project.servers["shared"].enabled());
    assert_eq!(project.servers["user-only"].source, McpSource::User);
    assert!(matches!(
        project.servers["shared"].configuration,
        McpServerSettings::Remote { .. }
    ));

    let original_user = fs::read(fixture.home.join("mcp.json")).unwrap();
    let original_project = fs::read(fixture.home.join("project/.mcp.json")).unwrap();
    let project_revision = project.project_revision.unwrap();
    mcp.update(
        SettingsScope::Project(1),
        project_revision.clone(),
        "shared".into(),
        McpChange::SetEnabled(true),
        &cancel,
    )
    .await
    .unwrap();
    let enabled = mcp.catalog(Some(1), &cancel).await.unwrap();
    assert!(enabled.servers["shared"].enabled());
    assert_eq!(enabled.project_revision, Some(project_revision.clone()));
    assert_eq!(
        fs::read(fixture.home.join("project/.mcp.json")).unwrap(),
        original_project
    );
    assert_eq!(
        fs::read(fixture.home.join("mcp.json")).unwrap(),
        original_user
    );

    mcp.update(
        SettingsScope::Project(1),
        project_revision,
        "shared".into(),
        McpChange::Remove,
        &cancel,
    )
    .await
    .unwrap();
    let persisted: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("project/.mcp.json")).unwrap()).unwrap();
    assert_eq!(persisted["projectExtension"], "keep");
    let revealed = mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(revealed.servers["shared"].source, McpSource::User);
    assert!(revealed.servers["shared"].enabled());

    let new_server =
        serde_json::from_value(json!({"command":"other", "args":["--read-only"]})).unwrap();
    let (a, b) = tokio::join!(
        mcp.update(
            SettingsScope::User,
            user.user_revision.clone(),
            "shared".into(),
            McpChange::SetEnabled(false),
            &cancel
        ),
        mcp.update(
            SettingsScope::User,
            user.user_revision,
            "new".into(),
            McpChange::Put(new_server),
            &cancel
        ),
    );
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(McpError::Settings(SettingsError::Conflict)))
            | (Err(McpError::Settings(SettingsError::Conflict)), Ok(_))
    ));
    let saved = mcp.catalog(None, &cancel).await.unwrap();
    assert_eq!(saved.servers.contains_key("new"), b.is_ok());
    assert_eq!(saved.servers["shared"].enabled(), a.is_err());
    let persisted: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(persisted["extension"], json!({"preserve":17}));
    assert_eq!(
        persisted["mcpServers"]["user-only"]["auth"]["token"],
        "private"
    );
    assert_eq!(
        persisted["mcpServers"]["user-only"]["future"],
        json!({"keep":true})
    );

    let replacement = serde_json::from_value(json!({
        "type":"http", "url":"https://replacement.test/mcp"
    }))
    .unwrap();
    let revision = mcp
        .update(
            SettingsScope::User,
            saved.user_revision,
            "shared".into(),
            McpChange::Put(replacement),
            &cancel,
        )
        .await
        .unwrap();
    let persisted: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        persisted["mcpServers"]["shared"],
        json!({
            "type":"http", "url":"https://replacement.test/mcp"
        })
    );
    assert_eq!(persisted["extension"], json!({"preserve":17}));

    let invalid = serde_json::from_value(json!({"command":""})).unwrap();
    assert!(matches!(
        mcp.update(
            SettingsScope::User,
            revision.clone(),
            "shared".into(),
            McpChange::Put(invalid),
            &cancel
        )
        .await,
        Err(McpError::Settings(SettingsError::Invalid(_)))
    ));
    assert_eq!(
        mcp.catalog(None, &cancel).await.unwrap().user_revision,
        revision
    );

    assert!(matches!(
        mcp.catalog(Some(2), &cancel).await,
        Err(McpError::Settings(SettingsError::Invalid(
            "unauthorized project"
        )))
    ));
    assert!(matches!(
        mcp.update(
            SettingsScope::User,
            revision.clone(),
            "absent".into(),
            McpChange::SetEnabled(false),
            &cancel
        )
        .await,
        Err(McpError::UnknownServer)
    ));
    assert!(matches!(
        mcp.update(
            SettingsScope::User,
            revision,
            " ".into(),
            McpChange::Remove,
            &cancel
        )
        .await,
        Err(McpError::InvalidId)
    ));

    fs::write(fixture.home.join("project/.mcp.json"), b"{").unwrap();
    assert!(matches!(
        mcp.catalog(Some(1), &cancel).await,
        Err(McpError::Settings(SettingsError::Invalid(_)))
    ));
    assert!(mcp.catalog(None, &cancel).await.is_ok());
    fixture.close().await;
}

#[tokio::test]
async fn editing_reads_the_declared_entry_while_running_resolves_it() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let user = fixture.mcp.catalog(None, &cancel).await.unwrap();

    let entry = fixture
        .mcp
        .editable(McpSource::User, "shared".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(entry.revision.as_deref(), Some(user.user_revision.as_str()));
    assert!(entry.enabled);
    let McpServerSettings::Stdio { cwd, .. } = &entry.configuration else {
        panic!("the user's shared entry is stdio");
    };
    assert!(cwd.is_none());

    let project = fixture.home.join("project");
    let revision = fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            fixture
                .mcp
                .catalog(Some(1), &cancel)
                .await
                .unwrap()
                .project_revision
                .unwrap(),
            "local".into(),
            McpChange::Put(serde_json::from_value(json!({"command":"project-server"})).unwrap()),
            &cancel,
        )
        .await
        .unwrap();
    let entry = fixture
        .mcp
        .editable(McpSource::Project(1), "local".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(entry.revision.as_deref(), Some(revision.as_str()));
    assert!(!entry.enabled, "a project entry starts disabled");
    let McpServerSettings::Stdio { cwd, .. } = &entry.configuration else {
        panic!("the project entry is stdio");
    };
    assert!(
        cwd.is_none(),
        "the edit read keeps the declared working directory"
    );
    let running = fixture
        .mcp
        .entry(McpSource::Project(1), "local".into(), &cancel)
        .await
        .unwrap();
    let McpServerSettings::Stdio { cwd, .. } = &running.configuration else {
        panic!("the project entry is stdio");
    };
    assert_eq!(
        cwd.as_deref(),
        project.to_str(),
        "the run view resolves the workspace"
    );

    fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            revision.clone(),
            "local".into(),
            McpChange::SetEnabled(true),
            &cancel,
        )
        .await
        .unwrap();
    let entry = fixture
        .mcp
        .editable(McpSource::Project(1), "local".into(), &cancel)
        .await
        .unwrap();
    assert!(entry.enabled, "enablement comes from the user's record");
    assert_eq!(entry.revision.as_deref(), Some(revision.as_str()));

    assert!(matches!(
        fixture
            .mcp
            .editable(McpSource::User, " ".into(), &cancel)
            .await,
        Err(McpError::InvalidId)
    ));
    assert!(matches!(
        fixture
            .mcp
            .editable(McpSource::User, "absent".into(), &cancel)
            .await,
        Err(McpError::UnknownServer)
    ));
    fixture.close().await;
}

#[tokio::test]
async fn framework_precancellation_closes_admission_before_dependent_cleanup_finishes() {
    struct SlowConsumer {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl Plugin for SlowConsumer {
        type Deps = Arc<McpService>;
        fn name(&self) -> &str {
            "SlowMcpConsumer"
        }
        async fn apply(&self, ctx: &Ctx, _: Self::Deps) -> rutis::Result {
            let entered = self.entered.clone();
            let release = self.release.clone();
            ctx.effect(move || async move {
                entered.notify_one();
                release.notified().await;
                Ok(())
            })?;
            Ok(())
        }
    }

    let fixture = Fixture::new().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let consumer = fixture.root.plugin(SlowConsumer {
        entered: entered.clone(),
        release: release.clone(),
    });
    wait_active(
        &fixture.root,
        &[consumer],
        Duration::from_secs(5),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let closing = tokio::spawn(fixture.plugin.shutdown());
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let result = fixture.mcp.catalog(None, &CancellationToken::new()).await;
    release.notify_one();
    closing.await.unwrap().unwrap();
    fixture.close().await;
    assert!(matches!(result, Err(McpError::Closed)));
}

#[tokio::test]
async fn admitted_writes_survive_dropped_waiters_and_stop_with_the_plugin() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let path = fixture.files.path(0, "mcp.json").unwrap();
    let snapshot = fixture.mcp.catalog(None, &cancel).await.unwrap();
    let changed = Arc::new(tokio::sync::Notify::new());
    let ping = changed.clone();
    fixture
        .root
        .listen::<SettingsChanged>()
        .name(settings_changes("mcp"))
        .on(move |_, _| {
            ping.notify_one();
            async { Ok(None) }
        })
        .unwrap();
    let lease = fixture.files.lock(&path, &cancel).await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            fixture.mcp.update(
                SettingsScope::User,
                snapshot.user_revision,
                "shared".into(),
                McpChange::SetEnabled(false),
                &cancel,
            )
        )
        .await
        .is_err()
    );
    drop(lease);
    tokio::time::timeout(Duration::from_secs(5), changed.notified())
        .await
        .unwrap();
    let snapshot = fixture.mcp.catalog(None, &cancel).await.unwrap();
    assert!(!snapshot.servers["shared"].enabled());
    let bytes = fs::read(fixture.home.join("mcp.json")).unwrap();

    let lease = fixture.files.lock(&path, &cancel).await.unwrap();
    let cancelled = CancellationToken::new();
    let pending = fixture.mcp.update(
        SettingsScope::User,
        snapshot.user_revision.clone(),
        "shared".into(),
        McpChange::SetEnabled(true),
        &cancelled,
    );
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut pending)
            .await
            .is_err()
    );
    cancelled.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap(),
        Err(McpError::Settings(SettingsError::Cancelled))
    ));
    drop(lease);
    assert_eq!(fs::read(fixture.home.join("mcp.json")).unwrap(), bytes);

    let lease = fixture.files.lock(&path, &cancel).await.unwrap();
    let pending = fixture.mcp.update(
        SettingsScope::User,
        snapshot.user_revision,
        "shared".into(),
        McpChange::SetEnabled(true),
        &cancel,
    );
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut pending)
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(5), fixture.plugin.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        pending.await,
        Err(McpError::Settings(SettingsError::Cancelled))
    ));
    assert!(matches!(
        fixture.mcp.catalog(None, &cancel).await,
        Err(McpError::Closed)
    ));
    assert!(matches!(
        fixture
            .mcp
            .update(
                SettingsScope::User,
                "missing".into(),
                "x".into(),
                McpChange::Remove,
                &cancel
            )
            .await,
        Err(McpError::Closed)
    ));
    drop(lease);
    assert!(matches!(
        fixture
            .settings
            .read::<McpSettings>(SettingsScope::User, &cancel)
            .await,
        Err(SettingsError::UnknownSource)
    ));
    assert_eq!(fs::read(fixture.home.join("mcp.json")).unwrap(), bytes);
    fixture.close().await;
}
