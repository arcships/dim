//! The `compaction` settings source (#2235 PR 1b): the declared schema's
//! write boundary, the defaults a reader sees, and the port the model
//! stacks read through.

use dim_agent_capabilities::compaction_settings::CompactionSettingsCell;
use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_capabilities::methods::Owner as MethodOwner;
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_agent_types::compaction_settings::{
    CompactionWindowSpec, ShapedWindow, shape_compaction_window,
};
use dim_app_server::codex_oauth::OpenAICodexOAuthPlugin;
use dim_app_server::compaction_settings::{COMPACTION_SOURCE, CompactionSettingsPlugin};
use dim_app_server::grok_oauth::GrokOAuthPlugin;
use dim_app_server::managed_provider::ManagedProvidersPlugin;
use dim_app_server::methods::{ManagementMethods, ManagementMethodsPlugin};
use dim_app_server::provider::{ModelPreferences, ProviderPlugin, ProviderService};
use dim_app_server::settings::{
    SettingsError, SettingsPlugin, SettingsScope, SettingsService, Writer,
};
use dim_app_server::settings_methods::SettingsMethodsPlugin;
use dim_app_server::storage::SqliteStoragePlugin;
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_loader::resolve::Build;
use serde_json::json;
use std::fs;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn home() -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("dim-compaction-settings-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path.canonicalize().unwrap()
}

#[tokio::test]
async fn the_compaction_source_validates_writes_and_serves_typed_reads() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let home = home();
        let database = home.join("settings.sqlite");
        let root = Ctx::root().unwrap();
        let catalog = dim_app_server::catalog();
        let build = Build::default();
        let names = || dim_agent_runtime::Names::copy(&catalog, &build);
        let cell = CompactionSettingsCell::default();
        let plugin = CompactionSettingsPlugin::new(&names(), cell.clone()).unwrap();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![home.clone()])),
            root.plugin(SqliteStoragePlugin::new(database.clone())),
            root.plugin(SettingsPlugin::native(&names(), 0, vec![]).unwrap()),
            root.plugin(plugin),
        ];
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        let settings = root.get::<SettingsService>().unwrap();

        // defaults before any write: auto, no per-model entries
        let initial = settings
            .read_declared(COMPACTION_SOURCE, SettingsScope::User, &cancel)
            .await
            .unwrap();
        assert_eq!(
            initial.document,
            json!({"default": "auto", "models": {}}),
            "the schema fills the default document"
        );

        // the write boundary accepts every valid shape
        let document = json!({"default": "40%", "models": {"p/m": 400000, "m": "128k", "bare/auto": "auto", "wide/m": "100%", "tiny/m": "0.05%", "exact/m": "100.0%", "cap/k": "900719925474k", "cap/plain": "999999999999999", "cap/m": "999999999m"}});
        let saved = settings
            .update_declared(
                COMPACTION_SOURCE,
                SettingsScope::User,
                &initial.revision,
                document.clone(),
                Writer::Client,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(saved.document, document, "valid values round-trip unchanged");

        // the port serves the typed document with the fill applied
        let host = cell.host().expect("the row publishes the port");
        let typed = (host.0)(cancel.clone()).await.unwrap();
        assert_eq!(typed.default, CompactionWindowSpec::Percent(40.0));
        assert_eq!(typed.models["p/m"], CompactionWindowSpec::Tokens(400_000));
        assert_eq!(typed.models["m"], CompactionWindowSpec::Tokens(128_000));
        assert_eq!(typed.models["bare/auto"], CompactionWindowSpec::Auto);
        assert_eq!(typed.models["wide/m"], CompactionWindowSpec::Percent(100.0));
        assert_eq!(typed.spec_for("p", "m"), CompactionWindowSpec::Tokens(400_000));
        assert_eq!(typed.spec_for("other", "m"), CompactionWindowSpec::Tokens(128_000));
        assert_eq!(typed.spec_for("other", "missing"), CompactionWindowSpec::Percent(40.0));
        // an explicit "auto" entry is a hit and overrides the non-auto default
        assert_eq!(typed.spec_for("bare", "auto"), CompactionWindowSpec::Auto);
        assert_eq!(
            shape_compaction_window(typed.spec_for("wide", "m"), 64_000),
            ShapedWindow::Shaped { tokens: 64_000, raised_from: None }
        );

        // the write boundary rejects zero, over-100 percentages and malformed values
        for invalid in [
            json!({"default": 0}),
            json!({"default": "0"}),
            json!({"default": "0k"}),
            json!({"default": "0%"}),
            json!({"default": "0.0%"}),
            json!({"default": "101%"}),
            json!({"default": "150%"}),
            json!({"default": "-40%"}),
            json!({"default": "abc"}),
            json!({"default": true}),
            json!({"default": "AUTO"}),
            json!({"models": {"p/m": -5}}),
            json!({"models": {"p/m": "200%"}}),
            json!({"models": {"p/m": null}}),
            json!({"models": {"p/m": "999999999999999999m"}}),
            json!({"models": {"p/m": "+5%"}}),
            json!({"models": {"p/m": ".5%"}}),
            json!({"models": {"p/m": "05%"}}),
            json!({"models": {"p/m": "0400k"}}),
            json!({"models": {"p/m": "0400"}}),
            json!({"models": {"p/m": "9007199254740k"}}),
            json!({"models": {"p/m": "1000000000000000"}}),
            json!({"models": {"p/m": "9999999999m"}}),
            json!({"unexpected": "auto"}),
        ] {
            let attempt = settings
                .update_declared(
                    COMPACTION_SOURCE,
                    SettingsScope::User,
                    &saved.revision,
                    invalid.clone(),
                    Writer::Client,
                    &cancel,
                )
                .await;
            assert!(
                matches!(attempt, Err(SettingsError::Document(_))),
                "{invalid} must fail as a document violation: {:?}",
                attempt.map(|read| read.document)
            );
        }
        // nothing of the invalid writes landed
        let current = settings
            .read_declared(COMPACTION_SOURCE, SettingsScope::User, &cancel)
            .await
            .unwrap();
        assert_eq!(current.document, document);
        assert_eq!(current.revision, saved.revision);

        // a CAS conflict is reported, not retried
        assert!(
            matches!(
                settings
                    .update_declared(
                        COMPACTION_SOURCE,
                        SettingsScope::User,
                        &initial.revision,
                        json!({"default": "auto"}),
                        Writer::Client,
                        &cancel,
                    )
                    .await,
                Err(SettingsError::Conflict)
            ),
            "a stale revision conflicts"
        );

        // the document lives in the native store by its source key
        let db = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM settings_documents WHERE source='user/compaction'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1,
            "the native store keys the document by source"
        );
        fs::remove_dir_all(home).unwrap();
    })
    .await
    .unwrap();
}

/// `settings.sources` lists the compaction source with its user scope, the
/// catalog entry clients discover it through (#2235 PR 1b).
#[tokio::test]
async fn the_compaction_source_is_listed_in_settings_sources() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let home = home();
        let database = home.join("settings.sqlite");
        let root = Ctx::root().unwrap();
        let catalog = dim_app_server::catalog();
        let build = Build::default();
        let names = || Names::copy(&catalog, &build);
        let cell = CompactionSettingsCell::default();
        let plugin = CompactionSettingsPlugin::new(&names(), cell).unwrap();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![home.clone()])),
            root.plugin(SqliteStoragePlugin::new(database)),
            root.plugin(SettingsPlugin::native(&names(), 0, vec![]).unwrap()),
            root.plugin(ManagementMethodsPlugin::typed(&catalog).unwrap()),
            root.plugin(SettingsMethodsPlugin::new(&names()).unwrap()),
            root.plugin(plugin),
        ];
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();

        let table = root.get::<ManagementMethods>().unwrap();
        let provider = match table.owner("settings.sources") {
            MethodOwner::Active(provider) => provider,
            _ => panic!("settings.sources has no active provider"),
        };
        let listed = provider
            .accept(
                "settings.sources".to_owned(),
                json!({}),
                None,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap()
            .reply
            .await
            .unwrap()
            .unwrap();
        let compaction = listed["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["source"] == "compaction")
            .unwrap_or_else(|| panic!("settings.sources lists compaction: {listed}"));
        assert_eq!(compaction["scopes"], json!(["user"]));
        fs::remove_dir_all(home).unwrap();
    })
    .await
    .unwrap();
}
/// A token or percentage window adopted after an explicit `auto` resets the
/// auto-only explicit fact (`startup.rs`'s Tokens/Percent branches): the
/// adopted metadata carries the window and no `autoCompactWindowExplicit`,
/// so the startup shape of a token adoption matches one that never named
/// `auto` (#2235 PR 1b). In-process because the wire projection of a
/// `context/configured` event omits startup metadata.
#[tokio::test]
async fn a_token_window_after_an_explicit_auto_resets_the_explicit_fact() {
    let home = home();
    fs::create_dir_all(&home).unwrap();
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = || dim_agent_runtime::Names::copy(&catalog, &build);
    let root = Ctx::root().unwrap();
    let views = vec![
        root.plugin(ManagementMethodsPlugin::typed(&catalog).unwrap()),
        root.plugin(FileStoragePlugin::new(vec![home.clone()])),
        root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite"))),
        root.plugin(ManagedProvidersPlugin::new(None)),
        root.plugin(OpenAICodexOAuthPlugin::new(&names(), 0).unwrap()),
        root.plugin(GrokOAuthPlugin::new(&names(), 0).unwrap()),
        root.plugin(dim_agent_aimux::AimuxPlugin::default()),
        root.plugin(ProviderPlugin::new(&names()).unwrap()),
    ];
    let cancel = CancellationToken::new();
    wait_active(&root, &views, Duration::from_secs(5), &cancel)
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let input = |window: serde_json::Value| {
        serde_json::from_value(json!({
            "connection": {"baseUrl": "http://127.0.0.1:1/v1", "apiFormat": "openai-compatible", "apiKey": "fixture"},
            "contextWindow": 128_000,
            "maxOutputTokens": 4_096,
            "autoCompactWindow": window,
        }))
        .unwrap()
    };
    let target = || dim_app_server::protocol::ModelSelection {
        provider: None,
        model_id: Some("startup-fixture".into()),
    };
    let launch = provider
        .startup_configuration(
            target(),
            &home,
            ModelPreferences::default(),
            input(json!("auto")),
            None,
            &cancel,
        )
        .await
        .unwrap();
    let adopted = launch
        .model
        .metadata
        .as_ref()
        .unwrap()
        .startup
        .as_ref()
        .unwrap();
    assert_eq!(
        adopted.auto_compact_window, None,
        "explicit auto holds no window"
    );
    assert!(
        adopted.auto_compact_window_explicit,
        "explicit auto records the explicit fact"
    );
    // A resume's shape: the CLI still carries the startup connection, and
    // the adoption names the window this time.
    for window in [json!(128_000u64), json!("40%")] {
        let configuration = dim_agent_types::ModelConfiguration {
            provider_id: launch.model.provider_id.clone(),
            model_id: launch.model.model_id.clone(),
            capability_snapshot: launch.model.capability_snapshot.clone(),
            tools: None,
            tool_sources: Vec::new(),
            config: launch.model.config.clone(),
            metadata: launch.model.metadata.clone(),
        };
        let resumed = provider
            .startup_configuration(
                target(),
                &home,
                ModelPreferences::default(),
                input(window.clone()),
                Some(&configuration),
                &cancel,
            )
            .await
            .unwrap();
        let startup = resumed
            .model
            .metadata
            .as_ref()
            .unwrap()
            .startup
            .as_ref()
            .unwrap();
        assert!(
            startup.auto_compact_window.is_some(),
            "{window}: the token window is adopted"
        );
        assert!(
            !startup.auto_compact_window_explicit,
            "{window}: the explicit-auto fact resets when a token or percentage window is adopted"
        );
    }
    fs::remove_dir_all(home).unwrap();
}
