//! The model-config profile titles a Session: the configured catalog owns the
//! auxiliary model, the SessionTitle part injects it, and the request reaches
//! the configured model ([AuxiliaryModel](../../../docs/design/appserver/auxiliary-model.md)
//! §11.8, [SessionTitle](../../../docs/design/plugins/SessionTitle.md)).
#![cfg(unix)]
mod support;
use axum::{Router, routing::post};
use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
use dim_agent_capabilities::{
    defaults, model_provider::ModelProviderPlugin, timeline_tree::BranchToolConfig,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
use dim_agent_types::ModelSelection;
use dim_app_server::{ServerConfig, serve};
use dim_client::{Client, Message};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use support::*;
use tokio::sync::mpsc;

async fn entry(client: &mut Client, id: &str) -> Value {
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();

    listed
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == id)
        .unwrap()
        .clone()
}

async fn titled(client: &mut Client, id: &str, text: &str, source: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = entry(client, id).await;
            if current["title"] == text
                && current["titleSource"] == source
                && client.notices.iter().any(|notice| {
                    matches!(notice,
                    Message::Notice { method, params }
                    if method == "session.notice" && params["sessionId"] == id
                        && params["kind"] == "title"
                        && params["data"]["titleRevision"] == current["titleRevision"])
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the committed title and notice arrive");
}

/// Creates an interactive Session and waits for the built-in SessionTitle part
/// to serve `session.rename`.
async fn create(client: &mut Client, home: &std::path::Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,
        "tools":["wait"],"interactive":true,
        "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await
        .unwrap();

    let id = created["sessionId"].as_str().unwrap().to_owned();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client
                .rpc("session.rename", json!({"sessionId":id,"title":0}))
                .await
                .unwrap_err();
            if dim_client::app_error(&reply) == Some("invalid_params") {
                break;
            }
            assert!(waiting(&reply), "{reply}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the built-in SessionTitle part activates");
    id
}

async fn submit(client: &mut Client, id: &str, text: &str) {
    client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":text}),
        )
        .await
        .unwrap();
}

/// The title request carries the SessionTitle system prompt and no tools; the
/// turn's request carries the Session's tools.
fn title_request(body: &Value) -> bool {
    body.get("tools").is_none()
        && body["messages"].as_array().is_some_and(|messages| {
            messages.iter().any(|message| {
                message["role"] == "system"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("Write a short title"))
            })
        })
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

fn configured_builder(directory: &Path, models: Arc<AimuxModels>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(dim_agent_capabilities::host::PrepareContextHost(
            Arc::new(dim_agent_capabilities::host::IdentityContext),
        )));
    dim_session_title::install(&mut builder);
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
        models,
        dim_agent_capabilities::summary_strategy::ProductSummaryStrategyConfig::default(),
    );
    dim_agent_aimux::install(&mut builder);
    dim_app_server::collaboration::install(&mut builder);
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_profile_titles_a_session_and_renames_it() {
    let store = Store::new();
    let directory = store.0.clone();
    let (sent, mut requests) = mpsc::unbounded_channel::<Request>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", post(complete))
        .with_state(sent);
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let models = Arc::new(
        AimuxModels::from_config(
            ModelConfigFile::load(&models_file(&directory, &format!("http://{address}/v1")))
                .unwrap(),
        )
        .unwrap(),
    );
    let server = serve(
        configured_builder(&directory, models.clone()),
        ServerConfig {
            unix_path: Some(directory.join("ipc/server.sock")),
            model_catalog: Some(models),
            target_home: Some(directory.join("state")),
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
    .unwrap();
    let mut client = client(&server).await;

    let id = create(&mut client, &directory).await;
    let first = "hello from the configured profile";
    submit(&mut client, &id, first).await;
    // The turn and the title run at the same time against this endpoint, in
    // either order. Tell them apart by shape and answer both: the turn ends on
    // its own once its request is answered.
    let (mut title, mut turn) = (false, false);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !title || !turn {
            let (request, release) = requests.recv().await.unwrap();
            if title_request(&request) {
                assert!(!title, "exactly one title request: {request}");
                assert_eq!(request["model"], "small");
                assert_eq!(request["messages"].as_array().unwrap().len(), 2);
                assert_eq!(request["messages"][1]["content"], first);
                release.send("Configured profile title".into()).unwrap();
                title = true;
            } else {
                assert!(!turn, "exactly one turn request: {request}");
                assert!(request.get("tools").is_some(), "{request}");
                release
                    .send("An answer to the first message".into())
                    .unwrap();
                turn = true;
            }
        }
    })
    .await
    .expect("the turn and the title requests arrive");
    titled(&mut client, &id, "Configured profile title", "auto").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if entry(&mut client, &id).await["running"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the turn ends on its own");

    let renamed = when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":"  Manual title  "}),
    )
    .await;
    assert_eq!(renamed["title"], "Manual title");
    titled(&mut client, &id, "Manual title", "manual").await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    http.abort();
}
