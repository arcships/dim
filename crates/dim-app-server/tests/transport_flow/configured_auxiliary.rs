//! The model-config profile provides the bindings its loaded rows require:
//! the auxiliary model (#899) and a plugin files root (#985).
use super::session_typescript::{fixture, when_served};
use super::*;
use axum::{Json, Router, extract::State, routing::post};
use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{
    host::{AuxiliaryErrorCode, AuxiliaryModelHost, AuxiliaryRequest},
    model_provider::ModelProviderPlugin,
    timeline_tree::BranchToolConfig,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::SessionConfig;
use dim_agent_types::{Message, ModelId, ModelSelection, Role};
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use std::path::{Path, PathBuf};

/// What the fake provider received.
#[derive(Default)]
struct Capture {
    requests: std::sync::Mutex<Vec<Value>>,
}

/// An OpenAI-compatible provider: the Session's turn gets a short answer and
/// the title request gets a title.
async fn completion(State(capture): State<Arc<Capture>>, Json(body): Json<Value>) -> String {
    let title = body.to_string().contains("Write a short title");
    capture.requests.lock().unwrap().push(body);
    let text = if title {
        "Configured profile title"
    } else {
        "ok"
    };
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"fixture","choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
    )
}

async fn provider(capture: Arc<Capture>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(completion))
        .with_state(capture);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn directory(name: &str) -> PathBuf {
    let directory = std::path::PathBuf::from("/tmp")
        .join(format!("dim-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// The profile's model file: one provider with one model and no credential.
fn models_file(directory: &Path, base_url: &str) -> PathBuf {
    let path = directory.join("models.json");
    std::fs::write(
        &path,
        json!({
            "defaultModel": {"providerId": "local", "modelId": "small"},
            "providers": [{"id": "local", "kind": "openai-chat", "baseUrl": base_url,
                "models": [{"id": "small", "contextWindow": 8000, "maxOutputTokens": 200,
                    "config": {"maxTokens": 200}}]}]
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn models(path: &Path) -> Arc<AimuxModels> {
    Arc::new(AimuxModels::from_config(ModelConfigFile::load(path).unwrap()).unwrap())
}

/// Serves the model-config profile: no product home, a configured catalog and
/// the native home under `directory`.
async fn launch_configured(
    directory: &Path,
    models: Arc<AimuxModels>,
    parts: Option<SessionTypeScriptConfig>,
    defaults: Option<PathBuf>,
) -> RunningServer {
    launch_configured_with(
        directory,
        models,
        parts,
        defaults,
        Some(directory.join("state")),
    )
    .await
}

/// Serves the model-config profile with no product home and a configured
/// catalog. `home` is `--home`; `None` starts the host with only the database
/// file under `directory`.
async fn launch_configured_with(
    directory: &Path,
    models: Arc<AimuxModels>,
    parts: Option<SessionTypeScriptConfig>,
    defaults: Option<PathBuf>,
    home: Option<PathBuf>,
) -> RunningServer {
    let database = home.is_none().then(|| directory.join("dim.sqlite"));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(dim_agent_capabilities::host::PrepareContextHost(
            Arc::new(dim_agent_capabilities::host::IdentityContext),
        )));
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
            output_root: directory.join("outputs"),
        },
        BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    dim_agent_capabilities::defaults::install_compaction(
        &mut builder,
        models.clone(),
        dim_agent_capabilities::summary_strategy::ProductSummaryStrategyConfig::default(),
    );
    dim_agent_aimux::install(&mut builder);
    dim_app_server::collaboration::install(&mut builder);
    serve(
        builder,
        ServerConfig {
            unix_path: Some(directory.join("ipc/server.sock")),
            model_catalog: Some(models),
            target_home: home,
            database_path: database,
            session_typescript: parts,
            plugin_defaults: defaults,
            session_defaults: SessionConfig {
                model: ModelSelection {
                    provider_id: "local".into(),
                    model_id: "small".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                },
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

fn model(model_id: &str) -> ModelId {
    ModelId {
        provider_id: "local".into(),
        model_id: model_id.into(),
    }
}

fn request(max_output_tokens: Option<u32>) -> AuxiliaryRequest {
    AuxiliaryRequest {
        system: Some("Title this conversation".into()),
        messages: vec![Message::text(Role::User, "hello")],
        max_output_tokens,
    }
}

fn max_tokens(body: &Value) -> Option<u64> {
    body.get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
}

/// The shared name is provided by the profile's own model owner, and the Rust
/// port runs the configured model under the declared limits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_configured_profile_provides_and_runs_the_auxiliary_model() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let capture = Arc::new(Capture::default());
        let address = provider(capture.clone()).await;
        let directory = directory("configured-aux");
        let models = models(&models_file(&directory, &format!("http://{address}/v1")));
        let server = launch_configured(&directory, models, None, None).await;

        let diagnostics = server.diagnostics();
        let key = rutis_bridge::session::host_key("auxiliaryModel").erase();
        let binding = diagnostics
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .expect("the configured profile provides auxiliaryModel");
        let owner = diagnostics
            .plugins
            .iter()
            .find(|plugin| plugin.id == binding.provider)
            .expect("the binding names its provider");
        assert_eq!(owner.name, "ConfiguredModels");

        let auxiliary = server
            .service::<AuxiliaryModelHost>()
            .expect("the auxiliary model host")
            .clone();
        let reply = auxiliary
            .0
            .complete(model("small"), request(Some(32)), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(reply.text, "ok");
        let sent = capture.requests.lock().unwrap().pop().unwrap();
        assert_eq!(sent["model"], "small");
        assert!(sent.to_string().contains("hello"), "{sent}");
        assert!(sent.get("tools").is_none(), "{sent}");
        assert_eq!(max_tokens(&sent), Some(32));

        auxiliary
            .0
            .complete(
                model("small"),
                request(Some(1_000_000)),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let sent = capture.requests.lock().unwrap().pop().unwrap();
        assert_eq!(max_tokens(&sent), Some(200));

        let missing = auxiliary
            .0
            .complete(model("absent"), request(None), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(missing.code, AuxiliaryErrorCode::ModelUnavailable);
        assert!(capture.requests.lock().unwrap().is_empty());

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// A Session part reaches the operation through the shared name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_configured_profile_reaches_the_auxiliary_model_from_a_session_part() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let capture = Arc::new(Capture::default());
        let address = provider(capture.clone()).await;
        let directory = directory("configured-aux-part");
        let models = models(&models_file(&directory, &format!("http://{address}/v1")));
        let mut parts = fixture();
        parts.rows = vec![json!({
            "id":"auxiliary",
            "name":parts.anchor.parent().unwrap().join("fixtures/auxiliary-model.ts"),
            "provides":{},
            "config":{},
        })];
        let server = launch_configured(&directory, models, Some(parts), None).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let id = create(&mut client, &directory).await;
        let reply = when_served(
            &mut client,
            "session.auxiliary.complete",
            json!({"sessionId":id,"request":{
                "model":{"providerId":"local","modelId":"small"},
                "request":{"system":"Write a title",
                    "messages":[{"role":"user","content":"hello"}],"maxOutputTokens":32}}}),
        )
        .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["text"], "ok");
        let sent = capture.requests.lock().unwrap().pop().unwrap();
        assert_eq!(sent["model"], "small");
        assert!(sent.get("tools").is_none(), "{sent}");
        assert_eq!(max_tokens(&sent), Some(32));

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// With no named root and no product home, the plugin files root is the
/// execution home `--home`, so the PluginFiles row runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_configured_profile_provides_a_plugin_files_root() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let capture = Arc::new(Capture::default());
        let address = provider(capture.clone()).await;
        let directory = directory("configured-plugin-files");
        let models = models(&models_file(&directory, &format!("http://{address}/v1")));
        let server = launch_configured(&directory, models, None, None).await;

        let diagnostics = server.diagnostics();
        let key = rutis_bridge::session::host_key("pluginFiles").erase();
        let binding = diagnostics
            .bindings
            .iter()
            .find(|binding| binding.key == key)
            .expect("the configured profile provides pluginFiles");
        let owner = diagnostics
            .plugins
            .iter()
            .find(|plugin| plugin.id == binding.provider)
            .expect("the binding names its provider");
        assert_eq!(owner.name, "PluginFiles");
        assert_eq!(owner.state, rutis::host::FiberState::Active);

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// A host started with only a database file has no plugin files root: it
/// registers no `pluginFiles` provider and never falls back to the database
/// directory or the working directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_configured_profile_with_only_a_database_provides_no_plugin_files_root() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let capture = Arc::new(Capture::default());
        let address = provider(capture.clone()).await;
        let directory = directory("configured-plugin-files-none");
        let models = models(&models_file(&directory, &format!("http://{address}/v1")));
        let server = launch_configured_with(&directory, models, None, None, None).await;

        let diagnostics = server.diagnostics();
        let key = rutis_bridge::session::host_key("pluginFiles").erase();
        assert!(
            diagnostics
                .bindings
                .iter()
                .all(|binding| binding.key != key),
            "a database-only host provides no pluginFiles"
        );
        assert!(
            diagnostics
                .plugins
                .iter()
                .all(|plugin| plugin.name != "PluginFiles"),
            "a database-only host starts no PluginFiles row"
        );

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// The default plugins this profile's file names load: the process plugins
/// run as defaults and the content plugin lists its Skill.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_configured_profile_runs_the_default_plugins() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let address = provider(capture.clone()).await;
        let directory = directory("configured-defaults");
        let models = models(&models_file(&directory, &format!("http://{address}/v1")));
        let mut parts = fixture();
        parts.rows.clear();
        let defaults = parts.anchor.with_file_name("default-plugins.json");
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(&defaults).unwrap()).unwrap();
        let names: Vec<String> = document["plugins"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let server = launch_configured(&directory, models, Some(parts), Some(defaults)).await;
        let mut client = Client::new_version(&server, false, 3).await;
        // The process plugins the product profile's flow waits for; the file
        // this profile loads names them too.
        let process = ["goal", "workflow", "scheduler"];
        for name in process {
            assert!(
                names.iter().any(|listed| listed == name),
                "{names:?} names {name}"
            );
        }
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let listed = client.rpc("plugin.list", json!({})).await;
                assert_ok(&listed);
                let items = listed["result"].as_array().unwrap();
                let found = |name: &str| items.iter().find(|item| item["name"] == name);
                let running = process
                    .iter()
                    .all(|name| found(name).is_some_and(|item| item["status"] == "running"));
                let by_default = names
                    .iter()
                    .all(|name| found(name).is_some_and(|item| item["use"] == "default"));
                if running && by_default {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the default plugins run");

        // The content plugin contributes its Skill.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let catalog = when_served(&mut client, "skills.catalog", json!({})).await;
                let listed = catalog["result"]["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        entry["id"]
                            .as_str()
                            .unwrap_or("")
                            .ends_with("plugin-authoring")
                    });
                if listed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the default content plugin lists its Skill");

        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

async fn create(client: &mut Client, home: &Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":[],"interactive":true,
                "model":{"provider":"local","modelId":"small"},
                "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    id
}
