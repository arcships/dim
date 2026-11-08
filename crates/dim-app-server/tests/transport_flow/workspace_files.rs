//! The sessionless workspace file search `workspace.files.search`
//! (WorkspaceTools §1): the same candidates as `session.files.search` over one
//! shared index, its limits, its `cwd` rule, and its local-only admission.
use super::*;
use dim_agent_capabilities::{
    model_provider::ModelProviderPlugin, timeline_tree::BranchToolConfig,
    workspace_tools::WorkspaceToolsConfig,
};
use std::path::{Path, PathBuf};

/// A product home, so `HostPlatform`, Settings and the Web entry are
/// assembled beside the search's own row.
fn home() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("dim-workspace-files-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}

/// The model catalog the mock hosts serve: one selectable model, so
/// `session.create` resolves without a model file. No request reaches it.
fn configured_models() -> Arc<dim_agent_aimux::AimuxModels> {
    Arc::new(
        dim_agent_aimux::AimuxModels::from_config(
            serde_json::from_value(json!({
                "defaultModel": {"providerId": "local", "modelId": "mock"},
                "providers": [{"id": "local", "kind": "openai-compatible",
                    "baseUrl": "https://model.example/v1",
                    "models": [{"id": "mock", "contextWindow": 128000, "maxOutputTokens": 4096,
                        "config": {"maxTokens": 64}}]}],
            }))
            .unwrap(),
        )
        .unwrap(),
    )
}

fn config(home: &Path) -> ServerConfig {
    let models = configured_models();
    let model = models.default_model().clone();
    ServerConfig {
        model_catalog: Some(models),
        target_home: Some(home.to_owned()),
        // Under /tmp so the socket path stays below SUN_LEN; the server
        // creates the missing parent 0700.
        unix_path: Some(
            PathBuf::from("/tmp").join(format!("dws-{}/server.sock", uuid::Uuid::new_v4())),
        ),
        session_defaults: dim_agent_runtime::SessionConfig {
            model: dim_agent_types::ModelSelection {
                provider_id: model.provider_id,
                model_id: model.model_id,
                capability_snapshot: None,
                config: None,
                metadata: None,
            },
            tools: Some(vec![]),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The local execution assembly: the WorkspaceTools Session part provides the
/// Session `ToolHost`, so this launcher installs no `tool_host` value of its
/// own (`shell_input.rs`'s launcher, without its probe parts).
async fn launch_server(home: &Path, web_assets: Option<PathBuf>) -> RunningServer {
    let mut config = config(home);
    config.web_assets = web_assets;
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: home.join("outputs"),
        },
        BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    serve(builder, config).await.unwrap()
}

/// A server whose Sessions mount WorkspaceTools, so `session.files.search`
/// and `workspace.files.search` can be compared over the same tree.
async fn launch(home: &Path) -> RunningServer {
    launch_server(home, None).await
}

/// A tree with several ranks of one query, plus one file outside it.
fn tree() -> PathBuf {
    let root = std::env::temp_dir().join(format!("dim-workspace-tree-{}", uuid::Uuid::new_v4()));
    for (path, content) in [
        ("notes.txt", "top"),
        ("docs/notes.md", "nested"),
        ("src/notes/main.rs", "deeper"),
        ("src/notes_helper.rs", "sibling"),
        ("other.txt", "other"),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    root
}

fn candidates(value: &Value) -> &Vec<Value> {
    value["result"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("{value}"))
}

/// Without a Session the method answers exactly what `session.files.search`
/// answers for a Session on the same tree, and both read one index: a file
/// that appears after the Session's walk is not offered by the second
/// method's first call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_search_matches_the_session_search_and_shares_its_index() {
    let home = home();
    let server = launch(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let root = tree();
    let created = client.rpc("session.create", json!({"cwd": root})).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();

    let session_search = client
        .rpc(
            "session.files.search",
            json!({"sessionId": session, "query": "notes", "limit": 10}),
        )
        .await;
    assert_ok(&session_search);
    assert!(!candidates(&session_search).is_empty(), "{session_search}");

    // The Session's walk is the index's walk. A file written after it is not
    // offered by the second method before the TTL expires, so a second index
    // (the Session part building its own) would fail here.
    std::fs::write(root.join("late-notes.txt"), "late").unwrap();
    let late = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "late-notes"}),
        )
        .await;
    assert_ok(&late);
    assert!(
        candidates(&late).is_empty(),
        "the workspace search reuses the Session search's walk: {late}"
    );

    let workspace_search = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "notes", "limit": 10}),
        )
        .await;
    assert_ok(&workspace_search);
    assert_eq!(
        candidates(&workspace_search),
        candidates(&session_search),
        "the same tree and query rank the same"
    );

    // Both entry points share the limit rule: 0 answers the first candidate.
    let zero = client
        .rpc(
            "session.files.search",
            json!({"sessionId": session, "query": "notes", "limit": 0}),
        )
        .await;
    assert_ok(&zero);
    assert_eq!(candidates(&zero).len(), 1, "{zero}");

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

/// `limit` defaults to 20 and clamps to `1..=50` on both methods, so 0 answers
/// one candidate; a negative value is the caller's error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_limit_defaults_clamps_and_refuses_a_negative() {
    let home = home();
    let server = launch(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let root = std::env::temp_dir().join(format!("dim-workspace-limit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..60 {
        std::fs::write(root.join(format!("match-{index:02}.txt")), "x").unwrap();
    }

    let default = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "match"}),
        )
        .await;
    assert_ok(&default);
    assert_eq!(candidates(&default).len(), 20, "{default}");

    let most = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "match", "limit": 100}),
        )
        .await;
    assert_ok(&most);
    assert_eq!(candidates(&most).len(), 50, "{most}");

    let zero = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "match", "limit": 0}),
        )
        .await;
    assert_ok(&zero);
    assert_eq!(candidates(&zero).len(), 1, "{zero}");

    // The Session method keeps the answer it always had on the same tree.
    let created = client.rpc("session.create", json!({"cwd": root})).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session_zero = client
        .rpc(
            "session.files.search",
            json!({"sessionId": session, "query": "match", "limit": 0}),
        )
        .await;
    assert_ok(&session_zero);
    assert_eq!(candidates(&session_zero).len(), 1, "{session_zero}");

    let negative = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "match", "limit": -1}),
        )
        .await;
    assert_eq!(
        negative["error"]["data"]["appError"], "invalid_params",
        "{negative}"
    );

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

/// `cwd` must be absolute, canonical and a directory; a refusal registers no
/// project with Settings, and a successful search does not either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_search_requires_an_absolute_existing_directory() {
    let home = home();
    let server = launch(&home).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let root = tree();
    let file = root.join("notes.txt");

    let before = client.rpc("settings.projects", json!({})).await;
    assert_ok(&before);
    assert_eq!(before["result"], json!({"projects": []}), "{before}");

    for cwd in [
        json!(root.join("missing")),
        json!(file),
        json!("relative/path"),
        json!(""),
    ] {
        let refused = client
            .rpc(
                "workspace.files.search",
                json!({"cwd": cwd, "query": "notes"}),
            )
            .await;
        assert_eq!(
            refused["error"]["data"]["appError"], "invalid_params",
            "{cwd}: {refused}"
        );
    }
    let after = client.rpc("settings.projects", json!({})).await;
    assert_eq!(after["result"], json!({"projects": []}), "{after}");

    let ok = client
        .rpc(
            "workspace.files.search",
            json!({"cwd": root, "query": "notes"}),
        )
        .await;
    assert_ok(&ok);
    let after = client.rpc("settings.projects", json!({})).await;
    assert_eq!(
        after["result"],
        json!({"projects": []}),
        "a sessionless search registers no project: {after}"
    );

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

/// `hello.features` lists the method on a local v3 connection; the Web entry
/// does not serve it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_search_is_local_only() {
    let home = home();
    let assets = super::web_entry::frontend_fixture();
    let server = launch_server(&home, Some(assets.clone())).await;
    let mut local = Client::new_version(&server, false, 3).await;
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("workspace.files.search")),
        "{}",
        local.features
    );

    let opened = local.rpc("web.open", json!({"port":0})).await["result"].clone();
    let address = opened["address"].as_str().unwrap().to_owned();
    let bootstrap = opened["url"]
        .as_str()
        .unwrap()
        .split_once("#bootstrap=")
        .unwrap()
        .1
        .to_owned();
    let origin = format!("http://{address}");
    let (status, head, _) = super::web_entry::http(
        &address,
        super::web_entry::post(
            &address,
            "/api/auth/exchange",
            &origin,
            &json!({"token": bootstrap}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let cookie = head
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut web = super::web_entry::web_client(&address, &cookie).await;
    assert!(
        !web.features
            .as_array()
            .unwrap()
            .contains(&json!("workspace.files.search")),
        "{}",
        web.features
    );
    let refused = web
        .rpc("workspace.files.search", json!({"cwd": home, "query": "x"}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "unknown_method",
        "{refused}"
    );

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&assets);
}
