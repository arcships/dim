//! The `agent` tool's `models` action through an actual Session, ToolRegistry
//! and ToolExecutor: it lists the models Provider would serve, with their
//! provider IDs, so a leader can name one for a profile.
use super::*;

const PROVIDER: &str = "018ec73f-0000-7000-8000-000000000051";

async fn create_session(client: &mut Client) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": ["agent"], "interactive": false,
                "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

async fn submit(client: &mut Client, id: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"tool:agent {\"action\":\"models\"}"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
}

async fn result(session: &dim_agent_runtime::Session) -> ToolResult {
    dim_agent_runtime::testing::branch_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| {
            if let EventKind::ToolResult(batch) = &event.kind {
                batch.results.first().map(|entry| entry.result.clone())
            } else {
                None
            }
        })
        .expect("tool result")
}

/// The `agent` declaration the model's first request carried.
async fn declaration(
    recorded: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<ToolDefinition>>,
) -> ToolDefinition {
    recorded
        .recv()
        .await
        .expect("a model request")
        .into_iter()
        .find(|tool| tool.name == "agent")
        .expect("the agent declaration")
}

fn actions(definition: &ToolDefinition) -> Vec<String> {
    let schema = serde_json::to_value(&definition.input_schema).unwrap();
    serde_json::from_value(schema["properties"]["action"]["enum"].clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_tool_lists_the_provider_ids_a_member_can_run() {
    let home = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("dm-agent-models-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let (server, _requests, _release, mut recorded) =
        super::session_typescript::start_recording(ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        })
        .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":PROVIDER,"displayName":"Models fixture",
            "connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},
            "models":[
                {"modelId":"served","displayName":"Served","status":"active",
                 "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000,16_384)},
                {"modelId":"gone","displayName":"Gone","status":"unavailable",
                 "capabilities":dim_agent_types::CatalogCapabilities::with_limits(64_000,8_192)}],
            "enabled":true}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":PROVIDER,"modelId":"served"}),
            )
            .await,
    );
    let id = create_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();

    submit(&mut client, &id).await;
    let declared = declaration(&mut recorded).await;
    assert!(actions(&declared).iter().any(|action| action == "models"));
    assert!(declared.description.contains("- models:"));
    let listed = result(&session).await;
    assert_ne!(listed.is_error, Some(true), "{listed:?}");
    assert_eq!(
        listed.structured_content.as_ref().unwrap()["models"],
        json!([{
            "providerId": PROVIDER,
            "modelId": "served",
            "contextWindow": 128_000,
            "maxOutputTokens": 16_384,
            "vision": false,
            "reasoning": false,
        }])
    );

    let area = client
        .rpc(
            "provider.management",
            json!({"providerId":PROVIDER, "modelId":"served"}),
        )
        .await;
    assert_ok(&area);
    assert_eq!(
        area["result"]["providers"][0]["models"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let missing = client
        .rpc(
            "provider.management",
            json!({"providerId":PROVIDER,"modelId":"missing"}),
        )
        .await;
    assert!(missing.get("error").is_some());
    assert!(
        client
            .rpc("provider.management", json!({}))
            .await
            .get("error")
            .is_some()
    );
    let version = area["result"]["providers"][0]["version"].clone();
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId":PROVIDER,"expectedVersion":version,"modelIds":[]}),
            )
            .await,
    );
    submit(&mut client, &id).await;
    let none = result(&session).await;
    assert_ne!(none.is_error, Some(true), "{none:?}");
    assert_eq!(none.structured_content.unwrap()["models"], json!([]));

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployment_without_the_selectable_read_offers_no_models_action() {
    let (server, _requests, _release, mut recorded) =
        super::session_typescript::start_recording(ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = create_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();

    submit(&mut client, &id).await;
    let declared = declaration(&mut recorded).await;
    assert!(!actions(&declared).iter().any(|action| action == "models"));
    assert!(!declared.description.contains("- models:"));
    let refused = result(&session).await;
    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    assert!(
        matches!(&refused.content, dim_agent_types::ToolContent::Text(text)
            if text.contains("invalid_input: this deployment does not describe selectable models")),
        "{refused:?}"
    );

    server.shutdown();
    server.join().await.unwrap();
}

/// The `--model-config` profile: aimux owns the catalog and provides the
/// same read, so the action and the bullet appear there too.
fn configured_models() -> Arc<dim_agent_aimux::AimuxModels> {
    let file: dim_agent_aimux::config::ModelConfigFile = serde_json::from_value(json!({
        "defaultModel": {"providerId":"local","modelId":"main"},
        "providers": [{
            "id": "local",
            "kind": "openai-compatible",
            "apiKeyEnv": null,
            "baseUrl": "http://127.0.0.1:1/v1",
            "compatible": null,
            "models": [{
                "id": "main",
                "contextWindow": 128000,
                "maxOutputTokens": 16384,
                "config": {"maxTokens": 16384}
            }]
        }]
    }))
    .unwrap();
    Arc::new(dim_agent_aimux::AimuxModels::from_config(file).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_configured_model_profile_lists_its_models() {
    let home = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("dm-agent-models-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let (server, _requests, _release, mut recorded) =
        super::session_typescript::start_recording(ServerConfig {
            model_catalog: Some(configured_models()),
            target_home: Some(home.clone()),
            session_defaults: dim_agent_runtime::SessionConfig {
                model: dim_agent_types::ModelSelection {
                    provider_id: "local".into(),
                    model_id: "main".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                },
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = create_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();

    submit(&mut client, &id).await;
    let declared = declaration(&mut recorded).await;
    assert!(actions(&declared).iter().any(|action| action == "models"));
    assert!(declared.description.contains("- models:"));
    let listed = result(&session).await;
    assert_ne!(listed.is_error, Some(true), "{listed:?}");
    assert_eq!(
        listed.structured_content.unwrap()["models"],
        json!([{
            "providerId": "local",
            "modelId": "main",
            "contextWindow": 128_000,
            "maxOutputTokens": 16_384,
            "vision": false,
            "reasoning": false,
        }])
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
