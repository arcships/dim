use dim_agent_capabilities::file_storage::{FileStorage, FileStoragePlugin};
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_app_server::{
    mcp::{
        McpChange, McpError, McpPlugin, McpService, McpSource,
        config::{
            McpPluginServers, McpProjectEnablement, McpSecrets, McpServerSettings, McpSettings,
            RemoteTransport,
        },
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
        mcp_host::provide_credentials(&root);
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
    // The applied token is the catalog's token for the same source, so a
    // client holding both never reads a matching configuration as not applied.
    let project = fixture.mcp.application(SettingsScope::Project(1));
    assert_eq!(project.applied_revision, catalog.project_revision);
    assert!(project.error.is_none());
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
    // The application carries the token the update returned, which is the
    // catalog's token for the source: the document revision alone never
    // matches a project source's token.
    let application = settled_at(&fixture, SettingsScope::Project(1), &revision).await;
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

/// A project source is served from `.mcp.json` and the workspace's withheld
/// records, so its applied token covers both, exactly as the catalog's
/// `projectRevision` does (D5, D23). A records-only write renews both without
/// touching the project file, and the publisher refreshes to it.
#[tokio::test]
async fn a_project_application_follows_a_records_only_change() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let project_file = fixture.home.join("project/.mcp.json");
    fs::write(
        &project_file,
        json!({"mcpServers":{"shared":{"command":"project-server"}}}).to_string(),
    )
    .unwrap();
    let workspace = fs::canonicalize(fixture.home.join("project")).unwrap();
    let secrets_path = fixture.home.join("mcp-secrets.json");
    fs::write(
        &secrets_path,
        json!({"entries":{(workspace.to_str().unwrap()):{"shared":{"$generation":"one","args":["--first"]}}}})
            .to_string(),
    )
    .unwrap();
    let document = fs::read(&project_file).unwrap();

    let token = fixture
        .mcp
        .catalog(Some(1), &cancel)
        .await
        .unwrap()
        .project_revision
        .unwrap();
    let application = settled_at(&fixture, SettingsScope::Project(1), &token).await;
    assert_eq!(application.applied_revision, Some(token.clone()));

    // The record changes alone; the project document is not written.
    let snapshot = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    let revision = snapshot.revision;
    let mut records = snapshot.document;
    let record = records
        .entries
        .get_mut(workspace.to_str().unwrap())
        .and_then(|servers| servers.get_mut("shared"))
        .expect("the workspace's record is stored");
    record.generation = "two".into();
    record.args = Some(vec!["--second".into()]);
    fixture
        .settings
        .update(SettingsScope::User, &revision, records, &cancel)
        .await
        .unwrap();
    assert_eq!(fs::read(&project_file).unwrap(), document);

    let next = fixture
        .mcp
        .catalog(Some(1), &cancel)
        .await
        .unwrap()
        .project_revision
        .unwrap();
    assert_ne!(next, token, "a record write renews the source token");
    let application = settled_at(&fixture, SettingsScope::Project(1), &next).await;
    assert_eq!(application.applied_revision, Some(next));

    fixture.close().await;
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

/// Waits until `source` is applied at `token`, which the publisher announces
/// after the refresh a settings write wakes. A published state is a separate
/// task's answer, so a read of it races the write without this wait.
async fn settled_at(
    fixture: &Fixture,
    source: SettingsScope,
    token: &str,
) -> dim_app_server::mcp::McpApplication {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let application = fixture.mcp.application(source);
            if application.error.is_some() || application.applied_revision.as_deref() == Some(token)
            {
                return application;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the source settles at its token")
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

    let mut invalid =
        serde_json::from_value::<McpServerSettings>(json!({"command": "valid"})).unwrap();
    let McpServerSettings::Stdio { command, .. } = &mut invalid else {
        panic!("an untyped command is stdio");
    };
    command.clear();
    assert!(matches!(
        mcp.update(
            SettingsScope::User,
            revision.clone(),
            "shared".into(),
            McpChange::Put(invalid),
            &cancel
        )
        .await,
        Err(McpError::InvalidEntry(_))
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
        Err(McpError::Settings(SettingsError::DocumentShape { .. }))
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

#[tokio::test]
async fn scope_move_needs_readable_records_and_remove_cleans_the_overlay() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    let secrets_path = fixture.home.join("mcp-secrets.json");
    fs::create_dir(&secrets_path).unwrap();
    let result = fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision.clone(),
            catalog.project_revision.clone().unwrap(),
            &cancel,
        )
        .await;
    // The source token covers the withheld records, so a move that cannot read
    // them refuses before it writes anything: no half-changed scope is left.
    assert!(result.is_err(), "{result:?}");
    let project = fixture
        .settings
        .read::<McpSettings>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap()
        .document;
    assert!(!project.contains("user-only"));
    let user = fixture
        .settings
        .read::<McpSettings>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(user.contains("user-only"));

    fs::remove_dir(&secrets_path).unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(catalog.servers["user-only"].source, McpSource::User);
    // A project side takes its token, never its document revision: the
    // document revision alone is stale even when no record exists.
    let document_revision = fixture
        .settings
        .read::<McpSettings>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap()
        .revision;
    assert!(matches!(
        fixture
            .mcp
            .move_entry(
                McpSource::User,
                McpSource::Project(1),
                "user-only".into(),
                catalog.user_revision.clone(),
                document_revision,
                &cancel,
            )
            .await,
        Err(McpError::Settings(SettingsError::Conflict))
    ));
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision.clone(),
            catalog.project_revision.clone().unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(catalog.servers["user-only"].source, McpSource::Project(1));
    let project = fixture
        .settings
        .read::<McpSettings>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap()
        .document;
    let skeleton = serde_json::to_value(&project.entries()["user-only"]).unwrap();
    assert!(
        skeleton["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://example.test") && !url.contains("/mcp"))
    );
    assert!(skeleton.get("headers").is_none());
    assert!(skeleton.get("auth").is_none());
    assert!(skeleton.get("future").is_none());

    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    let overlay = secrets
        .entries
        .values()
        .find_map(|servers| servers.get("user-only"))
        .expect("the move writes the withheld part");
    assert!(overlay.auth.is_some());
    assert_eq!(overlay.url.as_deref(), Some("https://example.test/mcp"));
    assert!(overlay.extra.contains_key("future"));
    assert!(!skeleton.to_string().contains("private"));

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert!(!catalog.servers["user-only"].enabled());
    fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            catalog.project_revision.clone().unwrap(),
            "user-only".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(
        !secrets
            .entries
            .values()
            .any(|servers| servers.contains_key("user-only")),
        "removing the project entry deletes its overlay"
    );
    fixture.close().await;
}

#[tokio::test]
async fn scope_move_back_to_user_keeps_the_merged_entry_and_its_enablement() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    fixture
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
            "user-only".into(),
            McpChange::SetEnabled(true),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::Project(1),
            McpSource::User,
            "user-only".into(),
            catalog.project_revision.unwrap(),
            catalog.user_revision,
            &cancel,
        )
        .await
        .unwrap();

    let user = fixture
        .settings
        .read::<McpSettings>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    let McpServerSettings::Remote {
        url,
        headers,
        auth,
        extra,
        ..
    } = &user.entries()["user-only"]
    else {
        panic!("the moved entry is remote");
    };
    assert_eq!(url, "https://example.test/mcp");
    assert!(auth.is_some(), "the withheld bearer token comes with it");
    assert!(headers.is_empty());
    assert!(extra.contains_key("future"), "unknown fields survive");
    assert!(
        user.entries()["user-only"].enabled(),
        "the user entry keeps the destination's effective enablement"
    );
    let project = fixture
        .settings
        .read::<McpSettings>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap()
        .document;
    assert!(!project.contains("user-only"));
    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(secrets.entries.is_empty(), "the overlay is deleted");
    let enablement = fixture
        .settings
        .read::<McpProjectEnablement>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(enablement.enabled.is_empty(), "the record is cleared");
    fixture.close().await;
}

/// #953: an overlay `timeout` is not the entry's limit. A move to user scope
/// writes the declared value once, and writes none when the entry declares
/// none.
#[tokio::test]
async fn an_overlay_timeout_is_not_the_limit_a_move_to_user_carries() {
    for declared in [Some(1_000), None] {
        let fixture = Fixture::new().await;
        let cancel = CancellationToken::new();
        let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
        fixture
            .mcp
            .move_entry(
                McpSource::User,
                McpSource::Project(1),
                "user-only".into(),
                catalog.user_revision,
                catalog.project_revision.unwrap(),
                &cancel,
            )
            .await
            .unwrap();

        let project = fixture
            .settings
            .read::<McpSettings>(SettingsScope::Project(1), &cancel)
            .await
            .unwrap();
        let mut project_document = project.document;
        let McpServerSettings::Remote { timeout, .. } = project_document
            .configure_mut("user-only")
            .expect("the moved entry is remote")
        else {
            panic!("the moved entry is remote");
        };
        *timeout = declared;
        fixture
            .settings
            .update(
                SettingsScope::Project(1),
                &project.revision,
                project_document,
                &cancel,
            )
            .await
            .unwrap();

        let overlay = fixture
            .settings
            .read::<McpSecrets>(SettingsScope::User, &cancel)
            .await
            .unwrap();
        let mut overlay_document = overlay.document;
        let record = overlay_document
            .entries
            .values_mut()
            .filter_map(|servers| servers.get_mut("user-only"))
            .next()
            .expect("the move withheld a record");
        record.extra.insert("timeout".into(), json!(5_000));
        fixture
            .settings
            .update(
                SettingsScope::User,
                &overlay.revision,
                overlay_document,
                &cancel,
            )
            .await
            .unwrap();

        let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
        fixture
            .mcp
            .move_entry(
                McpSource::Project(1),
                McpSource::User,
                "user-only".into(),
                catalog.project_revision.unwrap(),
                catalog.user_revision,
                &cancel,
            )
            .await
            .unwrap();
        let user = fixture
            .settings
            .read::<McpSettings>(SettingsScope::User, &cancel)
            .await
            .unwrap()
            .document;
        let McpServerSettings::Remote { timeout, extra, .. } = &user.entries()["user-only"] else {
            panic!("the moved entry is remote");
        };
        assert_eq!(*timeout, declared, "{declared:?}");
        assert!(
            !extra.contains_key("timeout"),
            "the overlay member is not the entry's own"
        );
        let written = fs::read_to_string(fixture.home.join("mcp.json")).unwrap();
        assert_eq!(
            written.matches("\"timeout\"").count(),
            usize::from(declared.is_some()),
            "{written}"
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn scope_move_checks_the_destination_id_and_both_revisions() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    let user = catalog.user_revision.clone();
    let project = catalog.project_revision.unwrap();
    assert!(matches!(
        fixture
            .mcp
            .move_entry(
                McpSource::User,
                McpSource::Project(1),
                "shared".into(),
                user.clone(),
                project.clone(),
                &cancel,
            )
            .await,
        Err(McpError::MoveConflict)
    ));
    assert!(matches!(
        fixture
            .mcp
            .move_entry(
                McpSource::User,
                McpSource::Project(1),
                "user-only".into(),
                "stale".into(),
                project.clone(),
                &cancel,
            )
            .await,
        Err(McpError::Settings(SettingsError::Conflict))
    ));
    assert!(matches!(
        fixture
            .mcp
            .move_entry(
                McpSource::User,
                McpSource::Project(1),
                "user-only".into(),
                user,
                "stale".into(),
                &cancel,
            )
            .await,
        Err(McpError::Settings(SettingsError::Conflict))
    ));
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(catalog.servers["shared"].source, McpSource::Project(1));
    assert_eq!(catalog.servers["user-only"].source, McpSource::User);
    assert_eq!(catalog.project_revision, Some(project));
    fixture.close().await;
}

#[tokio::test]
async fn a_conflicting_remove_keeps_the_project_entry_and_its_overlay() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let revision = fixture
        .mcp
        .catalog(Some(1), &cancel)
        .await
        .unwrap()
        .project_revision
        .unwrap();
    let snapshot = fixture
        .settings
        .read::<McpSettings>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap();
    let mut document = snapshot.document;
    let Some(McpServerSettings::Remote { extra, .. }) = document.configure_mut("user-only") else {
        panic!("the moved entry is remote");
    };
    extra.insert("note".into(), json!(1));
    // A writer outside Mcp takes Settings' revision for the document, not the
    // source token a client holds.
    fixture
        .settings
        .update(
            SettingsScope::Project(1),
            &snapshot.revision,
            document,
            &cancel,
        )
        .await
        .unwrap();
    let patched = fixture
        .mcp
        .catalog(Some(1), &cancel)
        .await
        .unwrap()
        .project_revision
        .unwrap();

    assert!(matches!(
        fixture
            .mcp
            .update(
                SettingsScope::Project(1),
                revision,
                "user-only".into(),
                McpChange::Remove,
                &cancel,
            )
            .await,
        Err(McpError::Settings(SettingsError::Conflict))
    ));
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(catalog.servers["user-only"].source, McpSource::Project(1));
    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(
        secrets
            .entries
            .values()
            .any(|servers| servers.contains_key("user-only")),
        "a remove that loses to a concurrent write keeps the overlay"
    );

    let path = fixture.files.path(1, ".mcp.json").unwrap();
    let lease = fixture.files.lock(&path, &cancel).await.unwrap();
    let pending = fixture.mcp.update(
        SettingsScope::Project(1),
        patched,
        "user-only".into(),
        McpChange::Remove,
        &cancel,
    );
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err(),
        "the removal waits for the project document"
    );
    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(
        secrets
            .entries
            .values()
            .any(|servers| servers.contains_key("user-only")),
        "the overlay is deleted only after the entry removal commits"
    );
    drop(lease);
    pending.await.unwrap();
    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(
        !secrets
            .entries
            .values()
            .any(|servers| servers.contains_key("user-only"))
    );
    fixture.close().await;
}
#[tokio::test]
async fn an_into_project_partial_is_finished_after_a_restart() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    // The state a move into project leaves when its record write fails after
    // the destination committed: the public skeleton is in the project, the
    // source entry is still in user scope, and no withheld record exists.
    let snapshot = fixture
        .settings
        .read::<McpSettings>(SettingsScope::Project(1), &cancel)
        .await
        .unwrap();
    let mut project = snapshot.document;
    project.insert(
        "user-only",
        serde_json::from_value(json!({"type":"http","url":"https://example.test"})).unwrap(),
    );
    fixture
        .settings
        .update(
            SettingsScope::Project(1),
            &snapshot.revision,
            project,
            &cancel,
        )
        .await
        .unwrap();

    fixture.publication.close().await;
    fixture.root.shutdown().await.unwrap();
    let restarted = Fixture::open(fixture.home.clone(), false).await;
    let catalog = restarted.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(catalog.servers["user-only"].source, McpSource::Project(1));
    assert!(
        !catalog.servers["user-only"].enabled(),
        "the partial destination stays disabled"
    );
    restarted
        .mcp
        .update(
            SettingsScope::Project(1),
            catalog.project_revision.unwrap(),
            "user-only".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    let catalog = restarted.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert_eq!(catalog.servers["user-only"].source, McpSource::User);
    restarted
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let secrets = restarted
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    let overlay = secrets
        .entries
        .values()
        .find_map(|servers| servers.get("user-only"))
        .expect("the retried move writes the withheld part");
    assert!(overlay.auth.is_some());
    assert_eq!(overlay.url.as_deref(), Some("https://example.test/mcp"));
    restarted.close().await;
}

#[tokio::test]
async fn a_save_racing_a_move_keeps_the_moves_overlay() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    let expected_version = catalog.project_revision.clone().unwrap();

    assert!(matches!(
        fixture
            .mcp
            .editable(McpSource::Project(1), "user-only".into(), &cancel)
            .await,
        Err(McpError::UnknownServer)
    ));
    assert!(
        fixture
            .mcp
            .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
            .await
            .unwrap()
            .is_none(),
        "client A saves an id the project does not hold and has no record for"
    );

    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision,
            expected_version.clone(),
            &cancel,
        )
        .await
        .unwrap();

    // B's move committed the entry and its record. A save that read the entry
    // first now sees the move's record and reads the entry again; the entry in
    // this state decides its answer: user-scoped, not the stale refusal that
    // names mcp.remove.
    let overlay = fixture
        .mcp
        .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
        .await
        .unwrap()
        .expect("the move writes the record A's guard now reads");
    assert!(overlay.auth.is_some());
    assert_eq!(overlay.url.as_deref(), Some("https://example.test/mcp"));
    let editable = fixture
        .mcp
        .editable(McpSource::Project(1), "user-only".into(), &cancel)
        .await
        .unwrap();
    assert!(editable.secrets_in_user_scope);
    let McpServerSettings::Remote { url, auth, .. } = &editable.configuration else {
        panic!("the moved entry is remote");
    };
    assert_eq!(url, "https://example.test/mcp");
    assert!(auth.is_some(), "the completed move keeps its credential");

    assert!(
        matches!(
            fixture
                .mcp
                .update(
                    SettingsScope::Project(1),
                    expected_version,
                    "user-only".into(),
                    McpChange::Put(
                        serde_json::from_value(json!({"command":"overwritten"})).unwrap()
                    ),
                    &cancel,
                )
                .await,
            Err(McpError::Settings(SettingsError::Conflict)),
        ),
        "the save that raced the move loses its project revision check"
    );
    assert!(
        fixture
            .mcp
            .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
            .await
            .unwrap()
            .is_some(),
        "the failed save does not delete the move's overlay"
    );
    fixture.close().await;
}

#[tokio::test]
async fn remove_keeps_a_replacement_moves_overlay() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let captured = fixture
        .mcp
        .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
        .await
        .unwrap()
        .expect("the move writes the record A's removal captures");

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            catalog.project_revision.unwrap(),
            "user-only".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .mcp
            .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
            .await
            .unwrap()
            .is_none(),
        "the removal deletes the record it captured"
    );

    let user_revision = fixture
        .mcp
        .catalog(Some(1), &cancel)
        .await
        .unwrap()
        .user_revision;
    let replacement: McpServerSettings = serde_json::from_value(json!({
        "type":"http",
        "url":"https://example.test/mcp",
        "auth":{"type":"bearer","token":"private"},
        "future":{"keep":true},
        "enabled":false
    }))
    .unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            user_revision,
            "user-only".into(),
            McpChange::Put(replacement),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "user-only".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let replacement_record = fixture
        .mcp
        .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
        .await
        .unwrap()
        .expect("the replacement move writes its own record");
    let mut captured_values = captured.clone();
    captured_values.generation.clear();
    let mut replacement_values = replacement_record.clone();
    replacement_values.generation.clear();
    assert!(
        replacement_values == captured_values,
        "the replacement writes the same withheld values as the removed record"
    );
    assert_ne!(
        replacement_record.generation, captured.generation,
        "the replacement writes a new generation"
    );

    fixture
        .mcp
        .clear_secret_if(
            SettingsScope::Project(1),
            "user-only".into(),
            captured.generation.clone(),
            &cancel,
        )
        .await
        .unwrap();
    let overlay = fixture
        .mcp
        .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
        .await
        .unwrap()
        .expect("the replacement move's record survives the earlier removal's cleanup");
    assert_eq!(overlay.generation, replacement_record.generation);
    assert!(overlay.auth.is_some());
    assert_eq!(overlay.url.as_deref(), Some("https://example.test/mcp"));

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            catalog.project_revision.unwrap(),
            "user-only".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .mcp
            .overlay(SettingsScope::Project(1), "user-only".into(), &cancel)
            .await
            .unwrap()
            .is_none(),
        "a later removal deletes the replacement's own record"
    );
    fixture.close().await;
}

/// #951: entries decode one by one, and a write keeps every entry it did
/// not change exactly as it was read, broken entries included.
#[tokio::test]
async fn entries_decode_one_by_one_and_writes_keep_every_other_entry_as_written() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let marker = "token-like-value-4c1e";
    let user_file = json!({
        "mcpServers": {
            "valid": {"command": "user-server", "args": ["--flag"], "future": {"keep": true}},
            "stream": {"type": "streamable-http", "url": "https://stream.test/mcp", "headers": {"X-Key": "value"}},
            "ws": {"type": "ws", "url": "wss://ws.test"},
            "untyped": {"url": "https://untyped.test/mcp"},
            "leaky": {"type": "http", "url": "https://leaky.test/mcp", "headers": {"Authorization": {"leak": marker}}}
        },
        "extension": {"preserve": 17}
    });
    let project_file = json!({
        "mcpServers": {
            "project-valid": {"type": "sse", "url": "https://project.test/sse"},
            "project-broken": {"command": ""}
        },
        "projectExtension": "keep"
    });
    let user_path = fixture.home.join("mcp.json");
    let project_path = fixture.home.join("project/.mcp.json");
    fs::write(&user_path, serde_json::to_vec(&user_file).unwrap()).unwrap();
    fs::write(&project_path, serde_json::to_vec(&project_file).unwrap()).unwrap();
    let user_bytes = fs::read(&user_path).unwrap();
    let project_bytes = fs::read(&project_path).unwrap();

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert!(catalog.servers.contains_key("valid"));
    assert!(catalog.servers.contains_key("stream"));
    assert!(catalog.servers.contains_key("project-valid"));
    assert_eq!(catalog.servers["stream"].source, McpSource::User);
    assert!(matches!(
        catalog.servers["stream"].configuration,
        McpServerSettings::Remote {
            kind: RemoteTransport::Http,
            ..
        }
    ));
    let broken: Vec<&str> = catalog
        .invalid
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    // Rows are ordered by source in precedence order, then by id.
    assert_eq!(broken, ["leaky", "untyped", "ws", "project-broken"]);
    let ws = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "ws")
        .unwrap();
    assert_eq!(ws.source, McpSource::User);
    assert_eq!(ws.error.field.as_deref(), Some("type"));
    assert!(ws.error.expected.contains("http or sse"));
    let untyped = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "untyped")
        .unwrap();
    assert_eq!(untyped.error.field.as_deref(), Some("type"));
    assert!(
        untyped
            .error
            .expected
            .contains("an entry with a url must name its type")
    );
    let leaky = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "leaky")
        .unwrap();
    assert_eq!(leaky.error.field.as_deref(), Some("headers.Authorization"));
    assert_eq!(leaky.error.expected, "a string");
    assert!(!leaky.error.to_string().contains(marker));
    let project_broken = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "project-broken")
        .unwrap();
    assert_eq!(project_broken.source, McpSource::Project(1));
    assert_eq!(project_broken.error.field.as_deref(), Some("command"));

    // A read writes nothing: bytes and revision stay as the file had them.
    assert_eq!(fs::read(&user_path).unwrap(), user_bytes);
    assert_eq!(fs::read(&project_path).unwrap(), project_bytes);
    assert_eq!(
        fixture
            .settings
            .read::<McpSettings>(SettingsScope::User, &cancel)
            .await
            .unwrap()
            .revision,
        catalog.user_revision
    );

    // A broken entry has no effective or editable configuration, and its
    // enablement is refused.
    for id in ["ws", "untyped"] {
        assert!(matches!(
            fixture.mcp.entry(McpSource::User, id.into(), &cancel).await,
            Err(McpError::InvalidEntry(_))
        ));
        assert!(matches!(
            fixture
                .mcp
                .editable(McpSource::User, id.into(), &cancel)
                .await,
            Err(McpError::InvalidEntry(_))
        ));
        assert!(matches!(
            fixture
                .mcp
                .update(
                    SettingsScope::User,
                    catalog.user_revision.clone(),
                    id.into(),
                    McpChange::SetEnabled(true),
                    &cancel,
                )
                .await,
            Err(McpError::InvalidEntry(_))
        ));
    }
    assert!(matches!(
        fixture
            .mcp
            .update(
                SettingsScope::Project(1),
                catalog.project_revision.clone().unwrap(),
                "project-broken".into(),
                McpChange::SetEnabled(false),
                &cancel,
            )
            .await,
        Err(McpError::InvalidEntry(_))
    ));

    // Removing one broken entry keeps every other entry and the document's
    // other members as written.
    let revision = fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "ws".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    let persisted: Value = serde_json::from_slice(&fs::read(&user_path).unwrap()).unwrap();
    assert!(persisted["mcpServers"].get("ws").is_none());
    assert_eq!(
        persisted["mcpServers"]["valid"],
        user_file["mcpServers"]["valid"]
    );
    assert_eq!(
        persisted["mcpServers"]["leaky"],
        user_file["mcpServers"]["leaky"]
    );
    assert_eq!(
        serde_json::to_string(&persisted["mcpServers"]["stream"]).unwrap(),
        serde_json::to_string(&user_file["mcpServers"]["stream"]).unwrap()
    );
    assert_eq!(persisted["extension"], user_file["extension"]);
    assert_eq!(fs::read(&project_path).unwrap(), project_bytes);

    // Saving an entry that was broken replaces only that entry.
    let replacement =
        serde_json::from_value(json!({"type": "http", "url": "https://replacement.test/mcp"}))
            .unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            revision,
            "leaky".into(),
            McpChange::Put(replacement),
            &cancel,
        )
        .await
        .unwrap();
    let persisted: Value = serde_json::from_slice(&fs::read(&user_path).unwrap()).unwrap();
    assert_eq!(
        persisted["mcpServers"]["leaky"],
        json!({"type": "http", "url": "https://replacement.test/mcp"})
    );
    assert_eq!(persisted["mcpServers"]["stream"]["type"], "streamable-http");

    // A broken project entry takes no overlay; its record is kept until the
    // entry is removed, and the removal deletes it.
    let workspace = fs::canonicalize(fixture.home.join("project")).unwrap();
    let workspace = workspace.to_str().unwrap();
    let secrets = json!({"entries": {workspace: {"project-broken": {
        "$generation": "generation-1",
        "headers": {"X-Key": "private"}
    }}}});
    fs::write(
        fixture.home.join("mcp-secrets.json"),
        serde_json::to_vec(&secrets).unwrap(),
    )
    .unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    assert!(
        catalog
            .invalid
            .iter()
            .any(|entry| entry.id == "project-broken")
    );
    assert!(!catalog.servers.contains_key("project-broken"));
    let stored = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(
        stored
            .entries
            .get(workspace)
            .and_then(|servers| servers.get("project-broken"))
            .is_some()
    );
    fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            catalog.project_revision.clone().unwrap(),
            "project-broken".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    let stored = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    assert!(
        stored
            .entries
            .get(workspace)
            .and_then(|servers| servers.get("project-broken"))
            .is_none()
    );
    let persisted: Value = serde_json::from_slice(&fs::read(&project_path).unwrap()).unwrap();
    assert!(persisted["mcpServers"].get("project-broken").is_none());
    assert_eq!(
        persisted["mcpServers"]["project-valid"],
        project_file["mcpServers"]["project-valid"]
    );
    assert_eq!(persisted["projectExtension"], "keep");

    fixture.close().await;
}

/// #951: a write keeps the document's id order and every untouched entry's
/// bytes: a new id is appended and a removed id leaves its place.
#[tokio::test]
async fn a_write_keeps_the_documents_id_order() {
    fn block<'a>(text: &'a str, id: &str) -> &'a str {
        let start = text.find(&format!("    \"{id}\": {{\n")).unwrap();
        let rest = &text[start..];
        let end = rest
            .find("\n    },")
            .or_else(|| rest.find("\n    }\n"))
            .unwrap();
        &rest[..end]
    }
    fn position(text: &str, id: &str) -> usize {
        text.find(&format!("    \"{id}\": {{\n")).unwrap()
    }

    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let path = fixture.home.join("mcp.json");
    let source = json!({
        "mcpServers": {
            "zeta": {"command": "z", "future": {"keep": [1, 2]}},
            "alpha": {"type": "http", "url": "https://alpha.test/mcp", "auth": {"type": "bearer", "token": "private"}, "unknown": true},
            "middle": {"command": "m", "enabled": false}
        },
        "extension": {"preserve": 17}
    });
    fs::write(&path, serde_json::to_vec_pretty(&source).unwrap()).unwrap();
    let snapshot = fixture
        .settings
        .read::<McpSettings>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    let before = fs::read_to_string(&path).unwrap();
    let replacement =
        serde_json::from_value(json!({"command": "m-edited", "enabled": false})).unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            snapshot.revision,
            "middle".into(),
            McpChange::Put(replacement),
            &cancel,
        )
        .await
        .unwrap();
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(block(&before, "zeta"), block(&after, "zeta"));
    assert_eq!(block(&before, "alpha"), block(&after, "alpha"));
    assert!(position(&after, "zeta") < position(&after, "alpha"));
    assert!(position(&after, "alpha") < position(&after, "middle"));
    assert!(after.contains("m-edited"), "{after}");

    // A new id is appended after the document's ids.
    let snapshot = fixture
        .settings
        .read::<McpSettings>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    let added = serde_json::from_value(json!({"command": "n"})).unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            snapshot.revision,
            "new".into(),
            McpChange::Put(added),
            &cancel,
        )
        .await
        .unwrap();
    let after = fs::read_to_string(&path).unwrap();
    assert!(position(&after, "middle") < position(&after, "new"));
    assert!(position(&after, "new") < after.find("  \"extension\"").unwrap());

    // A removed id leaves its place.
    let snapshot = fixture
        .settings
        .read::<McpSettings>(SettingsScope::User, &cancel)
        .await
        .unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            snapshot.revision,
            "alpha".into(),
            McpChange::Remove,
            &cancel,
        )
        .await
        .unwrap();
    let after = fs::read_to_string(&path).unwrap();
    assert!(!after.contains("\"alpha\""), "{after}");
    assert!(position(&after, "zeta") < position(&after, "middle"));
    assert!(position(&after, "middle") < position(&after, "new"));
    assert_eq!(block(&before, "zeta"), block(&after, "zeta"));
    fixture.close().await;
}

/// A broken project entry with the same id as a valid user entry must not hide
/// the user server: the catalog reports the valid row under `servers` and the
/// broken row under `invalid`, and runtime selection reaches the user entry.
#[tokio::test]
async fn a_broken_project_entry_does_not_hide_the_user_entry_with_the_same_id() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    // The default fixture gives `shared` a valid user entry; make the project
    // `shared` entry broken.
    let project_file = fixture.home.join("project/.mcp.json");
    fs::write(
        &project_file,
        json!({"mcpServers": {"shared": {"command": ""}}}).to_string(),
    )
    .unwrap();

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    let shared = catalog.servers.get("shared").unwrap();
    assert_eq!(shared.source, McpSource::User);
    assert!(matches!(
        shared.configuration,
        McpServerSettings::Stdio { ref command, .. } if command == "user-server"
    ));
    let broken = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "shared")
        .unwrap();
    assert_eq!(broken.source, McpSource::Project(1));
    assert_eq!(broken.error.field.as_deref(), Some("command"));

    // Runtime selection reaches the user entry, not the broken project row.
    let connection = fixture
        .mcp
        .server(Some(1), "shared".into(), &cancel)
        .await
        .unwrap();
    drop(connection);

    fixture.close().await;
}

/// #952: a reference keeps its spelling in the project file when an entry
/// moves into a workspace; the full text and the credentials stay in the
/// user's overlay.
#[tokio::test]
async fn a_moved_entry_keeps_a_reference_name_in_the_project_file() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "reference".into(),
            McpChange::Put(
                serde_json::from_value(json!({
                    "type": "http",
                    "url": "https://${MCP_HOST}/mcp",
                    "headers": {"Authorization": "Bearer ${MCP_TOKEN}"}
                }))
                .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "reference".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();

    let project: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("project/.mcp.json")).unwrap()).unwrap();
    assert_eq!(
        project["mcpServers"]["reference"]["url"],
        "https://${MCP_HOST}"
    );
    assert!(project["mcpServers"]["reference"].get("headers").is_none());

    let secrets = fixture
        .settings
        .read::<McpSecrets>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .document;
    let workspace = fs::canonicalize(fixture.home.join("project"))
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let secret = &secrets.entries[&workspace]["reference"];
    assert_eq!(
        secret.url.as_deref(),
        Some("https://${MCP_HOST}/mcp"),
        "the full text is withheld"
    );
    assert!(
        secret
            .headers
            .as_ref()
            .is_some_and(|headers| headers.contains_key("Authorization"))
    );

    fixture.close().await;
}

/// #952: a stored reference survives reads, an enablement write and a move in
/// each direction.
#[tokio::test]
async fn a_stored_reference_survives_reads_writes_and_moves() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "kept".into(),
            McpChange::Put(
                serde_json::from_value(json!({
                    "type": "http",
                    "url": "https://${MCP_HOST}/mcp",
                    "headers": {"Authorization": "Bearer ${MCP_TOKEN}"}
                }))
                .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    let user = |fixture: &Fixture| -> Value {
        serde_json::from_slice(&fs::read(fixture.home.join("mcp.json")).unwrap()).unwrap()
    };
    assert_eq!(
        user(&fixture)["mcpServers"]["kept"]["url"],
        "https://${MCP_HOST}/mcp"
    );
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "kept".into(),
            McpChange::SetEnabled(true),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        user(&fixture)["mcpServers"]["kept"]["url"],
        "https://${MCP_HOST}/mcp",
        "an enablement write keeps the text"
    );
    assert_eq!(
        user(&fixture)["mcpServers"]["kept"]["headers"]["Authorization"],
        "Bearer ${MCP_TOKEN}"
    );

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "kept".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let project: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("project/.mcp.json")).unwrap()).unwrap();
    assert_eq!(project["mcpServers"]["kept"]["url"], "https://${MCP_HOST}");

    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::Project(1),
            McpSource::User,
            "kept".into(),
            catalog.project_revision.unwrap(),
            catalog.user_revision,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        user(&fixture)["mcpServers"]["kept"]["url"],
        "https://${MCP_HOST}/mcp",
        "the move back restores the withheld text"
    );

    fixture.close().await;
}

/// #952: a write stores the reference, never an expanded value, even when the
/// daemon's environment has the name set.
#[tokio::test]
async fn a_write_never_stores_an_expanded_value() {
    unsafe {
        std::env::set_var("MCP_TEST_952_WRITE_URL", "https://expanded.test/mcp");
    }
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "kept".into(),
            McpChange::Put(
                serde_json::from_value(json!({
                    "type": "http",
                    "url": "${MCP_TEST_952_WRITE_URL}"
                }))
                .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    let stored: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        stored["mcpServers"]["kept"]["url"],
        "${MCP_TEST_952_WRITE_URL}"
    );
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    let kept = catalog.servers.get("kept").unwrap();
    assert_eq!(
        kept.referenced_variables,
        std::collections::BTreeSet::from(["MCP_TEST_952_WRITE_URL".to_owned()])
    );
    assert!(kept.unset_variables.is_empty());
    fixture.close().await;
}

/// #952: a move into a project writes the reference URL's public skeleton and
/// its full text to the overlay, for every shape a reference can take, and a
/// read back returns the stored text.
#[tokio::test]
async fn a_moved_reference_url_is_split_between_the_skeleton_and_the_overlay() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let workspace = fs::canonicalize(fixture.home.join("project")).unwrap();
    let workspace = workspace.to_str().unwrap();
    for (index, (url, skeleton)) in [
        (
            "http://localhost:${MCP_PORT}/mcp",
            "http://localhost:${MCP_PORT}",
        ),
        ("${MCP_BASE}/mcp?token=x", "${MCP_BASE}/mcp"),
        ("${SCHEME}://host/mcp", "${SCHEME}://host"),
        (
            "https://${HOST:-localhost:8080}/sse",
            "https://${HOST:-localhost:8080}",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("reference-{index}");
        let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
        fixture
            .mcp
            .update(
                SettingsScope::User,
                catalog.user_revision,
                id.clone(),
                McpChange::Put(
                    serde_json::from_value(json!({"type": "http", "url": url})).unwrap(),
                ),
                &cancel,
            )
            .await
            .unwrap();
        let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
        fixture
            .mcp
            .move_entry(
                McpSource::User,
                McpSource::Project(1),
                id.clone(),
                catalog.user_revision,
                catalog.project_revision.unwrap(),
                &cancel,
            )
            .await
            .unwrap_or_else(|error| panic!("the move of {url} failed: {error}"));

        let project: Value =
            serde_json::from_slice(&fs::read(fixture.home.join("project/.mcp.json")).unwrap())
                .unwrap();
        assert_eq!(project["mcpServers"][&id]["url"], skeleton, "{url}");
        let secrets: Value =
            serde_json::from_slice(&fs::read(fixture.home.join("mcp-secrets.json")).unwrap())
                .unwrap();
        assert_eq!(secrets["entries"][workspace][&id]["url"], url);

        let entry = fixture
            .mcp
            .entry(McpSource::Project(1), id.clone(), &cancel)
            .await
            .unwrap();
        let McpServerSettings::Remote { url: read_back, .. } = &entry.configuration else {
            panic!("the moved entry is remote");
        };
        assert_eq!(read_back, url, "a read back returns the stored text");
    }
    fixture.close().await;
}

/// #953: a stored plugin document decodes entry by entry. One out-of-range
/// timeout is invalid on its own, its plugin's other entries load, and a
/// re-registration replaces the document and heals it.
#[tokio::test]
async fn a_stored_plugin_document_decodes_entry_by_entry() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let dir = fixture.home.join("plugin-dir");
    fs::create_dir_all(&dir).unwrap();
    let stored = json!({
        "plugins": {
            "demo": {
                "dir": dir,
                "mcpServers": {
                    "good": {"command": "good-server"},
                    "bad": {"command": "bad-server", "timeout": 500}
                }
            }
        }
    });
    fs::write(
        fixture.home.join("mcp-plugins.json"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();

    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    assert!(
        catalog.servers.contains_key("demo:good"),
        "the plugin's valid entry keeps loading"
    );
    let bad = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "demo:bad")
        .expect("the broken plugin entry is listed as invalid");
    assert_eq!(bad.source, McpSource::Plugins);
    assert_eq!(bad.error.field.as_deref(), Some("timeout"));
    assert_eq!(
        bad.error.expected,
        "an integer number of milliseconds from 1000 to 86400000"
    );
    let Err(McpError::InvalidEntry(error)) = fixture
        .mcp
        .entry(McpSource::Plugins, "demo:bad".into(), &cancel)
        .await
    else {
        panic!("a path that contacts the server answers the entry's own error");
    };
    assert_eq!(error.field.as_deref(), Some("timeout"));

    fixture
        .mcp
        .register_plugin(
            "demo".into(),
            dir,
            json!({"mcpServers": {
                "good": {"command": "good-server"},
                "bad": {"command": "bad-server", "timeout": 5000}
            }}),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    assert!(catalog.servers.contains_key("demo:good"));
    assert!(catalog.servers.contains_key("demo:bad"));
    assert!(
        catalog.invalid.iter().all(|entry| entry.id != "demo:bad"),
        "the re-registration healed the entry"
    );

    fixture.close().await;
}

/// #953: a plugin document registers its entries; the wrapper's own members
/// are never the plugin's to set. A top-level `dir` and extension members are
/// dropped, so the stored file names one directory, the next read succeeds and
/// every plugin's entries still load.
#[tokio::test]
async fn a_plugin_document_cannot_set_the_wrappers_members() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let dir = fixture.home.join("plugin-dir");
    fs::create_dir_all(&dir).unwrap();
    fixture
        .mcp
        .register_plugin(
            "demo".into(),
            dir.clone(),
            json!({
                "mcpServers": {"good": {"command": "good-server"}},
                "dir": "/somewhere/else",
                "note": {"extension": true}
            }),
            &cancel,
        )
        .await
        .unwrap();

    let text = fs::read_to_string(fixture.home.join("mcp-plugins.json")).unwrap();
    assert_eq!(text.matches("\"dir\"").count(), 1, "{text}");
    let stored: McpPluginServers = serde_json::from_str(&text).unwrap();
    let document = &stored.plugins["demo"];
    assert_eq!(document.dir, dir);
    assert!(document.servers.contains("good"), "{text}");

    let other = fixture.home.join("other-dir");
    fs::create_dir_all(&other).unwrap();
    fixture
        .mcp
        .register_plugin(
            "other".into(),
            other,
            json!({"mcpServers": {"tool": {"command": "other-server"}}}),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    assert!(catalog.servers.contains_key("demo:good"));
    assert!(catalog.servers.contains_key("other:tool"));

    fixture.close().await;
}

async fn management(fixture: &Fixture, method: &str, params: Value) -> Value {
    fixture
        .root
        .get::<dim_app_server::methods::ManagementMethods>()
        .expect("management methods are installed")
        .provider(method)
        .expect("the method is registered")
        .accept(
            method.into(),
            params,
            None,
            CancellationToken::new(),
            |_| {},
        )
        .expect("the method is accepted")
        .reply
        .await
        .expect("the method replies")
        .expect("the method succeeds")
}

/// #952: a `://` inside a query value is withheld text, so a move never puts
/// the preceding query into the project file or the configuration view, and
/// the overlay keeps the full URL.
#[tokio::test]
async fn a_moved_reference_url_keeps_its_query_in_the_overlay() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let url = "${BASE}/mcp?token=demo-key&next=https://callback.example/done";
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "query".into(),
            McpChange::Put(serde_json::from_value(json!({"type": "http", "url": url})).unwrap()),
            &cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.mcp.catalog(Some(1), &cancel).await.unwrap();
    fixture
        .mcp
        .move_entry(
            McpSource::User,
            McpSource::Project(1),
            "query".into(),
            catalog.user_revision,
            catalog.project_revision.unwrap(),
            &cancel,
        )
        .await
        .unwrap();

    let project = fs::read_to_string(fixture.home.join("project/.mcp.json")).unwrap();
    assert!(
        !project.contains("demo-key"),
        "the project file keeps the query private: {project}"
    );
    assert!(
        !project.contains("callback.example"),
        "the project file keeps the query private: {project}"
    );

    let configuration = management(
        &fixture,
        "mcp.configuration",
        json!({"source": "project", "id": "query", "cwd": fixture.home.join("project")}),
    )
    .await;
    assert_eq!(configuration["configuration"]["endpoint"], "${BASE}/mcp");
    assert!(
        !configuration.to_string().contains("demo-key"),
        "the configuration view keeps the query private: {configuration}"
    );

    let secrets: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("mcp-secrets.json")).unwrap()).unwrap();
    let workspace = fs::canonicalize(fixture.home.join("project")).unwrap();
    assert_eq!(
        secrets["entries"][workspace.to_str().unwrap()]["query"]["url"],
        url,
        "the overlay keeps the full URL"
    );
    fixture.close().await;
}

/// #1014: a member dim does not read is unknown and goes back as written, and
/// `$generation` stays reserved: the entry that carries it reads as broken,
/// beside the entries that load.
#[tokio::test]
async fn unread_entry_members_are_kept_and_a_reserved_generation_is_broken() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let user_path = fixture.home.join("mcp.json");
    let user_file = json!({
        "mcpServers": {
            "legacy": {
                "command": "legacy-server",
                "transport": "stdio",
                "metadata": "x",
                "args": ["--flag"]
            },
            "collision": {"command": "server", "$generation": "g"}
        }
    });
    fs::write(&user_path, serde_json::to_vec(&user_file).unwrap()).unwrap();

    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    assert!(
        catalog.servers.contains_key("legacy"),
        "a member dim does not read keeps the entry: {:?}",
        catalog
            .invalid
            .iter()
            .map(|entry| entry.error.to_string())
            .collect::<Vec<_>>()
    );
    let collision = catalog
        .invalid
        .iter()
        .find(|entry| entry.id == "collision")
        .expect("the reserved member is the entry's error");
    assert_eq!(collision.error.field.as_deref(), Some("$generation"));
    assert!(
        !catalog.invalid.iter().any(|entry| entry.id == "legacy"),
        "the reserved member breaks only its own entry"
    );

    // Read the entry, edit another member, and write it back.
    let revision = fixture
        .settings
        .read::<McpSettings>(SettingsScope::User, &cancel)
        .await
        .unwrap()
        .revision;
    let mut editable = fixture
        .mcp
        .editable(McpSource::User, "legacy".into(), &cancel)
        .await
        .unwrap();
    let McpServerSettings::Stdio { args, .. } = &mut editable.configuration else {
        panic!("the fixture entry is stdio");
    };
    args.push("--edited".into());
    fixture
        .mcp
        .update(
            SettingsScope::User,
            revision,
            "legacy".into(),
            McpChange::Put(editable.configuration),
            &cancel,
        )
        .await
        .unwrap();

    let persisted: Value = serde_json::from_slice(&fs::read(&user_path).unwrap()).unwrap();
    assert_eq!(persisted["mcpServers"]["legacy"]["transport"], "stdio");
    assert_eq!(persisted["mcpServers"]["legacy"]["metadata"], "x");
    assert_eq!(
        persisted["mcpServers"]["legacy"]["args"],
        json!(["--flag", "--edited"])
    );
    assert_eq!(
        persisted["mcpServers"]["collision"],
        user_file["mcpServers"]["collision"]
    );

    // The read back returns the stored text of both unknown members.
    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    let McpServerSettings::Stdio { extra, .. } = &catalog.servers["legacy"].configuration else {
        panic!("the edited entry is stdio");
    };
    assert_eq!(extra["transport"], json!("stdio"));
    assert_eq!(extra["metadata"], json!("x"));

    fixture.close().await;
}

/// A project server the user disables stops even while the withheld records
/// cannot be read: the user's decision does not depend on them (D24).
#[tokio::test]
async fn a_disabled_project_server_stops_when_the_records_cannot_be_read() {
    let fixture = Fixture::new().await;
    let cancel = CancellationToken::new();
    let project_file = fixture.home.join("project/.mcp.json");
    fs::write(
        &project_file,
        json!({"mcpServers":{"shared":{"command":"project-server"}}}).to_string(),
    )
    .unwrap();
    let workspace = fixture.home.join("project");
    let secrets_path = fixture.home.join("mcp-secrets.json");
    fs::write(
        &secrets_path,
        json!({"entries":{(workspace.to_str().unwrap()):{"shared":{"$generation":"one","args":["--first"]}}}})
            .to_string(),
    )
    .unwrap();
    set_project(&fixture.mcp, 1, true, &cancel).await.unwrap();
    let server = fixture
        .mcp
        .server(Some(1), "shared".into(), &cancel)
        .await
        .unwrap();
    let token = fixture
        .mcp
        .catalog(Some(1), &cancel)
        .await
        .unwrap()
        .project_revision
        .unwrap();

    // The records stop being readable.
    fs::remove_file(&secrets_path).unwrap();
    fs::create_dir(&secrets_path).unwrap();

    let disabled = fixture
        .mcp
        .update(
            SettingsScope::Project(1),
            token,
            "shared".into(),
            McpChange::SetEnabled(false),
            &cancel,
        )
        .await;
    assert!(disabled.is_ok(), "{disabled:?}");
    assert!(
        server.withdrawn(),
        "the generation is withdrawn though its records could not be read"
    );

    fs::remove_dir(&secrets_path).unwrap();
    fixture.close().await;
}
