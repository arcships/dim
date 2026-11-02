//! Hooks configuration: the `hooks`, `hooks-projects` and `hooks-plugins`
//! Settings sources, the project file `.dimcode/hooks.json` and the `hooks.*`
//! management methods (Hooks design A.1, A.2, A.7, appendix D item 8).
use dim_agent_capabilities::{file_storage::FileStoragePlugin, methods::Owner};
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_app_server::{
    hooks_settings::{HooksService, HooksSettingsPlugin},
    methods::{ManagementMethods, ManagementMethodsPlugin},
    settings::SettingsPlugin,
};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_loader::resolve::Build;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

struct Fixture {
    root: Ctx,
    home: PathBuf,
    project: PathBuf,
    cancel: CancellationToken,
}
impl Fixture {
    async fn new() -> Self {
        let home =
            std::env::temp_dir().join(format!("dim-hooks-settings-{}", uuid::Uuid::new_v4()));
        let project = home.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(home.join(".dimcode")).unwrap();
        let home = fs::canonicalize(home).unwrap();
        let project = home.join("project");
        let root = Ctx::root().unwrap();
        let catalog = dim_app_server::catalog();
        let build = Build::default();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![home.clone(), project.clone()])),
            root.plugin(SettingsPlugin::new(&Names::copy(&catalog, &build), 0, vec![1]).unwrap()),
            root.plugin(
                HooksSettingsPlugin::new(&Names::copy(&catalog, &build), Some(home.clone()))
                    .unwrap(),
            ),
            root.plugin(ManagementMethodsPlugin::typed(&catalog).unwrap()),
        ];
        let cancel = CancellationToken::new();
        wait_active(&root, &views, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        Self {
            root,
            home,
            project,
            cancel,
        }
    }

    /// One `hooks.*` call, as the RPC layer makes it.
    async fn call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, dim_agent_capabilities::methods::RpcError> {
        let table = self.root.get::<ManagementMethods>().unwrap();
        let provider = match table.owner(method) {
            Owner::Active(provider) => provider,
            _ => panic!("{method} has no active provider"),
        };
        provider
            .accept(method.to_owned(), params, self.cancel.clone(), |_| {})
            .unwrap()
            .reply
            .await
            .unwrap()
    }

    fn write_project(&self, document: Value) {
        fs::create_dir_all(self.project.join(".dimcode")).unwrap();
        fs::write(
            self.project.join(".dimcode/hooks.json"),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
    }
}

fn handler(command: &str) -> Value {
    json!({"type": "command", "command": command})
}

fn documents(hooks: Value) -> Value {
    json!({"hooks": hooks})
}

#[tokio::test]
async fn catalog_reports_sources_states_and_never_reads_foreign_hook_files() {
    let fixture = Fixture::new().await;
    let catalog = fixture.call("hooks.catalog", json!({})).await.unwrap();
    assert_eq!(catalog["sources"][0]["source"], "user");
    assert_eq!(catalog["sources"][0]["state"], "missing");
    assert_eq!(catalog["hooks"], json!([]));

    // A Claude or Codex file in the workspace and in the home is not a source.
    for path in [
        fixture.project.join(".claude/settings.json"),
        fixture.home.join(".claude/settings.json"),
        fixture.project.join(".codex/hooks.json"),
        fixture.home.join(".codex/hooks.json"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&documents(json!({
                "Stop": [{"hooks": [handler("foreign")]}],
            })))
            .unwrap(),
        )
        .unwrap();
    }
    let catalog = fixture
        .call(
            "hooks.catalog",
            json!({"cwd": fixture.project.to_str().unwrap()}),
        )
        .await
        .unwrap();
    assert_eq!(catalog["hooks"], json!([]));
    let sources = catalog["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2, "{sources:?}");
    assert_eq!(sources[1]["source"], "project");
    assert_eq!(sources[1]["state"], "missing");

    fixture.write_project(documents(json!({
        "Stop": [{"hooks": [handler("notify")]}],
        "UserPromptSubmit": [{"hooks": [handler("prompt-check")]}],
        "PreToolUse": [{"matcher": "read", "hooks": [handler("guard")]}],
        "UnknownEvent": [{"hooks": [handler("ignored")]}],
    })));
    let catalog = fixture
        .call(
            "hooks.catalog",
            json!({"cwd": fixture.project.to_str().unwrap()}),
        )
        .await
        .unwrap();
    assert_eq!(catalog["sources"][1]["state"], "ok");
    assert_eq!(catalog["sources"][1]["enabled"], false);
    let hooks = catalog["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 4, "{hooks:?}");
    let by_event = |event: &str| {
        hooks
            .iter()
            .find(|entry| entry["event"] == event)
            .unwrap_or_else(|| panic!("{event} is listed"))
    };
    assert_eq!(by_event("Stop")["state"], "ok");
    assert_eq!(by_event("Stop")["source"], "project");
    assert_eq!(by_event("UserPromptSubmit")["state"], "ok");
    assert_eq!(by_event("PreToolUse")["state"], "ok");
    assert_eq!(by_event("UnknownEvent")["state"], "unsupported");

    // An invalid document is a source state, not a failed call.
    fs::write(fixture.project.join(".dimcode/hooks.json"), b"{ nope").unwrap();
    let catalog = fixture
        .call(
            "hooks.catalog",
            json!({"cwd": fixture.project.to_str().unwrap()}),
        )
        .await
        .unwrap();
    assert_eq!(catalog["sources"][1]["state"], "invalid");
    assert!(catalog["sources"][1]["message"].is_string());
    assert_eq!(catalog["hooks"], json!([]));
    fixture.root.shutdown().await.unwrap();
    fs::remove_dir_all(&fixture.home).unwrap();
}

#[tokio::test]
async fn project_hooks_stay_disabled_until_the_user_enables_them_and_edits_apply() {
    let fixture = Fixture::new().await;
    fixture.write_project(documents(json!({
        "Stop": [{"hooks": [handler("first")]}],
    })));
    let cwd = fixture.project.to_str().unwrap().to_owned();
    let catalog = fixture
        .call("hooks.catalog", json!({"cwd": cwd}))
        .await
        .unwrap();
    assert_eq!(catalog["sources"][1]["enabled"], false);
    let revision = catalog["projectsRevision"].as_str().unwrap().to_owned();

    let enabled = fixture
        .call(
            "hooks.set_enabled",
            json!({"cwd": cwd, "enabled": true, "expectedRevision": revision}),
        )
        .await
        .unwrap();
    let revision = enabled["revision"].as_str().unwrap().to_owned();
    let catalog = fixture
        .call("hooks.catalog", json!({"cwd": cwd}))
        .await
        .unwrap();
    assert_eq!(catalog["sources"][1]["enabled"], true);

    // A stale revision is a conflict; the workspace's own document cannot
    // change its enablement.
    let stale = fixture
        .call(
            "hooks.set_enabled",
            json!({"cwd": cwd, "enabled": false, "expectedRevision": "missing"}),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.data.as_ref().unwrap().app_error, "hooks_conflict");
    assert_eq!(
        fixture
            .call("hooks.catalog", json!({"cwd": cwd}))
            .await
            .unwrap()["sources"][1]["enabled"],
        true
    );

    // A later edit is read by the next catalog call: nothing is cached.
    fixture.write_project(documents(json!({
        "Stop": [{"hooks": [handler("second")]}],
    })));
    let catalog = fixture
        .call("hooks.catalog", json!({"cwd": cwd}))
        .await
        .unwrap();
    assert_eq!(catalog["hooks"][0]["command"], "second");
    assert_eq!(catalog["sources"][1]["enabled"], true);

    // Disabling again keeps the file and stops the source.
    let disabled = fixture
        .call(
            "hooks.set_enabled",
            json!({"cwd": cwd, "enabled": false, "expectedRevision": revision}),
        )
        .await
        .unwrap();
    assert!(disabled["revision"].is_string());
    assert_eq!(
        fixture
            .call("hooks.catalog", json!({"cwd": cwd}))
            .await
            .unwrap()["sources"][1]["enabled"],
        false
    );
    assert!(fixture.project.join(".dimcode/hooks.json").exists());
    fixture.root.shutdown().await.unwrap();
    fs::remove_dir_all(&fixture.home).unwrap();
}

#[tokio::test]
async fn a_user_document_is_saved_with_revisions_and_reports_disable_all_hooks() {
    let fixture = Fixture::new().await;
    let missing = fixture.call("hooks.catalog", json!({})).await.unwrap()["userRevision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(missing, "missing");
    let saved = fixture
        .call(
            "hooks.save",
            json!({
                "expectedRevision": missing,
                "document": documents(json!({
                    "SessionStart": [{"hooks": [handler("welcome")]}],
                    "PostToolUse": [{"matcher": "write|edit", "hooks": [handler("format")]}],
                })),
            }),
        )
        .await
        .unwrap();
    let revision = saved["revision"].as_str().unwrap().to_owned();
    assert_ne!(revision, missing);
    let catalog = fixture.call("hooks.catalog", json!({})).await.unwrap();
    assert_eq!(catalog["userRevision"], revision);
    assert_eq!(catalog["sources"][0]["state"], "ok");
    assert_eq!(catalog["hooks"].as_array().unwrap().len(), 2);
    assert_eq!(catalog["disableAllHooks"], false);
    // The reply carries the document a client edits: reading it and writing
    // it back is the client's whole edit path, with no second source.
    assert_eq!(
        catalog["userDocument"]["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "welcome"
    );
    let written = fixture
        .call(
            "hooks.save",
            json!({
                "expectedRevision": catalog["userRevision"],
                "document": catalog["userDocument"],
            }),
        )
        .await
        .unwrap();
    let revision = written["revision"].as_str().unwrap().to_owned();
    let catalog = fixture.call("hooks.catalog", json!({})).await.unwrap();
    assert_eq!(catalog["hooks"].as_array().unwrap().len(), 2);

    // A document that does not parse is refused without writing.
    let rejected = fixture
        .call(
            "hooks.save",
            json!({"expectedRevision": revision, "document": {"hooks": []}}),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code, -32602);
    assert_eq!(
        fixture.call("hooks.catalog", json!({})).await.unwrap()["userRevision"],
        revision
    );

    // The revision from before the update is stale: the CAS refuses it and
    // writes nothing.
    let previous = revision.clone();
    // disableAllHooks is reported and does not change the catalog.
    let updated = fixture
        .call(
            "hooks.save",
            json!({
                "expectedRevision": revision,
                "document": {
                    "disableAllHooks": true,
                    "hooks": {"SessionStart": [{"hooks": [handler("welcome")]}]},
                },
            }),
        )
        .await
        .unwrap();
    let revision = updated["revision"].as_str().unwrap().to_owned();
    let catalog = fixture.call("hooks.catalog", json!({})).await.unwrap();
    assert_eq!(catalog["userRevision"], revision);
    assert_eq!(catalog["disableAllHooks"], true);
    let stale = fixture
        .call(
            "hooks.save",
            json!({"expectedRevision": previous, "document": {"hooks": {}}}),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.data.as_ref().unwrap().app_error, "hooks_conflict");
    assert_eq!(
        fixture.call("hooks.catalog", json!({})).await.unwrap()["disableAllHooks"],
        true
    );
    fixture.root.shutdown().await.unwrap();
    fs::remove_dir_all(&fixture.home).unwrap();
}

/// A.2 and PluginManager §3.7: Hooks' owner operations keep a plugin's
/// document in `hooks-plugins`, list it as the source `plugin:<id>`, report a
/// document they refuse, and are the only writers of that source.
#[tokio::test]
async fn plugin_documents_are_registered_listed_and_kept_by_their_owner() {
    let fixture = Fixture::new().await;
    let service = fixture.root.get::<HooksService>().unwrap();
    let dir = fixture.home.join("plugins/kit");
    fs::create_dir_all(&dir).unwrap();
    let document = documents(json!({
        "Stop": [{"hooks": [handler("guard")]}],
    }));

    service
        .register_plugin("kit".into(), dir.clone(), document, &fixture.cancel)
        .await
        .unwrap();
    assert!(service.plugin_refusal("kit").is_none());
    assert_eq!(service.plugins(&fixture.cancel).await.unwrap(), ["kit"]);
    let stored: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("hooks-plugins.json")).unwrap())
            .unwrap();
    assert_eq!(
        stored["plugins"]["kit"]["dir"],
        dir.to_string_lossy().as_ref(),
        "{stored}"
    );
    assert_eq!(
        stored["plugins"]["kit"]["hooks"]["Stop"][0]["hooks"][0]["command"], "guard",
        "{stored}"
    );

    // The source is listed after the user's, enabled without any workspace
    // enablement, with the plugin directory as its path.
    let cwd = fixture.project.to_str().unwrap().to_owned();
    let catalog = fixture
        .call("hooks.catalog", json!({"cwd": cwd}))
        .await
        .unwrap();
    let sources = catalog["sources"].as_array().unwrap();
    let names: Vec<&Value> = sources.iter().map(|source| &source["source"]).collect();
    assert_eq!(
        names,
        [&json!("user"), &json!("project"), &json!("plugin:kit")]
    );
    assert_eq!(sources[2]["enabled"], true);
    assert_eq!(sources[2]["state"], "ok");
    assert_eq!(sources[2]["path"], dir.to_string_lossy().as_ref());
    assert_eq!(catalog["hooks"][0]["source"], "plugin:kit");
    assert_eq!(catalog["hooks"][0]["event"], "Stop");
    assert_eq!(catalog["hooks"][0]["state"], "ok");

    // No `hooks.*` method writes the registrations.
    let kept = fs::read(fixture.home.join("hooks-plugins.json")).unwrap();
    let user_revision = catalog["userRevision"].as_str().unwrap().to_owned();
    fixture
        .call(
            "hooks.save",
            json!({"expectedRevision": user_revision, "document": documents(json!({
                "Stop": [{"hooks": [handler("user")]}],
            }))}),
        )
        .await
        .unwrap();
    fixture
        .call(
            "hooks.set_enabled",
            json!({"cwd": cwd, "enabled": true, "expectedRevision": catalog["projectsRevision"]}),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read(fixture.home.join("hooks-plugins.json")).unwrap(),
        kept,
        "a hooks.* method wrote hooks-plugins"
    );

    // A document that does not parse is refused, leaves no entry, and is
    // remembered for PluginManager.
    let refused = service
        .register_plugin(
            "broken".into(),
            dir.clone(),
            json!({"hooks": []}),
            &fixture.cancel,
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("invalid hook document"),
        "{refused}"
    );
    assert!(service.plugin_refusal("broken").is_some());
    assert_eq!(service.plugins(&fixture.cancel).await.unwrap(), ["kit"]);

    // A second registration replaces the first, and withdrawal removes it.
    service
        .register_plugin(
            "kit".into(),
            dir.clone(),
            documents(json!({"Stop": [{"hooks": [handler("second")]}]})),
            &fixture.cancel,
        )
        .await
        .unwrap();
    let catalog = fixture.call("hooks.catalog", json!({})).await.unwrap();
    assert_eq!(catalog["hooks"][1]["command"], "second", "{catalog}");
    service
        .withdraw_plugin("kit".into(), &fixture.cancel)
        .await
        .unwrap();
    assert!(service.plugins(&fixture.cancel).await.unwrap().is_empty());
    let catalog = fixture.call("hooks.catalog", json!({})).await.unwrap();
    assert_eq!(
        catalog["sources"],
        json!([{"source": "user", "enabled": true, "state": "ok"}]),
        "{catalog}"
    );
    assert_eq!(catalog["hooks"][0]["source"], "user", "{catalog}");
    fixture.root.shutdown().await.unwrap();
    fs::remove_dir_all(&fixture.home).unwrap();
}
