use super::deletion::Store;
use super::*;

/// A Session that can open lines through the `branch` tool.
fn with_branch() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch"],
        "permissionPolicy": {"allowedTools": ["branch"]}
    })
}

/// `agent.create` for a profile; `model` is `None` for a prompt-only profile.
async fn profile(client: &mut Client, name: &str, model: Option<(&str, &str)>) -> String {
    let model =
        model.map(|(provider_id, model_id)| json!({"providerId":provider_id,"modelId":model_id}));
    let mut definition = json!({
        "name": name,
        "description": "",
        "systemPrompt": format!("{name} instructions."),
        "tools": [],
    });
    if let Some(model) = model {
        definition["model"] = model;
    }
    let created = client
        .rpc("agent.create", json!({"profile":definition}))
        .await;
    assert_ok(&created);
    created["result"]["agentId"].as_str().unwrap().to_owned()
}

/// The profile `session.agent.get` answers for `branch`; `None` when absent.
async fn adopted(client: &mut Client, session: &str, branch: &str) -> Option<String> {
    let reply = client
        .rpc(
            "session.agent.get",
            json!({"sessionId":session,"branchId":branch}),
        )
        .await;
    assert_ok(&reply);
    reply["result"]["agentId"].as_str().map(str::to_owned)
}

async fn adopt(client: &mut Client, session: &str, agent: &str) -> Value {
    let reply = client
        .rpc(
            "session.agent.update",
            json!({"sessionId":session,"branchId":"main","agentId":agent}),
        )
        .await;
    assert_ok(&reply);
    reply["result"].clone()
}

/// The last rest point of `branch`, as a cut for a child.
async fn through(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["through"].as_u64().unwrap()
}

async fn open_line(client: &mut Client, session: &str, parent: &str, prompt: &str) -> String {
    let at = through(client, session, parent).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":parent,"at":at,"prompt":prompt}),
        )
        .await;
    assert_ok(&created);
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// #1172: the read answers the profile the branch's configuration was adopted
/// from — from another client, for a child cut after the adoption but not one
/// before it, after a restart — and names the existing errors for an ended or
/// unknown branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_get_reads_the_adopted_profile_by_branch_and_after_a_restart() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.methods")),
        "the method table is advertised: {:?}",
        client.features
    );
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        None,
        "a Session that adopted no profile reads none"
    );
    let before = open_line(&mut client, &id, "main", "cut before the adoption").await;
    session.idle(&before).await.unwrap();

    let reviewer = profile(&mut client, "Reviewer", Some(("mock", "deterministic"))).await;
    assert_eq!(
        adopt(&mut client, &id, &reviewer).await["outcome"],
        "adopted"
    );
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        Some(reviewer.clone())
    );
    assert_eq!(
        session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .configuration
            .unwrap()
            .metadata
            .unwrap()
            .agent_id,
        Some(reviewer.clone()),
        "the record is the adopted configuration's own metadata"
    );
    assert_eq!(
        adopt(&mut client, &id, &reviewer).await["outcome"],
        "unchanged",
        "an identical adoption writes nothing"
    );
    let after = open_line(&mut client, &id, "main", "cut after the adoption").await;
    session.idle(&after).await.unwrap();
    assert_eq!(
        adopted(&mut client, &id, &after).await,
        Some(reviewer.clone()),
        "a child cut after the adoption inherits the record"
    );
    assert_eq!(
        adopted(&mut client, &id, &before).await,
        None,
        "a child cut before the adoption keeps none"
    );

    let mut other = Client::new_version(&server, false, 3).await;
    assert_eq!(
        adopted(&mut other, &id, "main").await,
        Some(reviewer.clone()),
        "another authorized client reads the same profile"
    );

    let mut legacy = Client::new_version(&server, false, 2).await;
    assert_eq!(
        legacy
            .rpc(
                "session.agent.get",
                json!({"sessionId":id,"branchId":"main"})
            )
            .await["error"]["data"]["appError"],
        "unknown_method"
    );
    assert_eq!(
        client
            .rpc(
                "session.agent.get",
                json!({"sessionId":id,"branchId":"missing"})
            )
            .await["error"]["data"]["appError"],
        "session_invalid_input",
        "an unknown branch keeps the existing error"
    );

    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":before}),
            )
            .await,
    );
    assert_eq!(
        client
            .rpc(
                "session.agent.get",
                json!({"sessionId":id,"branchId":before})
            )
            .await["error"]["data"]["appError"],
        "branch_ended",
        "an ended branch answers the existing ended-branch error"
    );

    drop(legacy);
    drop(other);
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        Some(reviewer),
        "the profile survives the restart"
    );
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1172: a preference-only update and a model selection keep the recorded
/// profile; both replace the model, not the prompt the branch runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preference_update_and_a_model_selection_keep_the_profile() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset};
    use std::os::unix::fs::DirBuilderExt;

    let home = std::path::PathBuf::from("/tmp").join(format!("ag-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: native.join("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            unix_path: Some(native.join("daemon.sock")),
            session_defaults: SessionConfig {
                tools: Some(["wait"].map(String::from).to_vec()),
                permission_policy: PermissionPolicy {
                    preset: PermissionPreset::WorkspaceWrite,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let provider = "018ec73f-0000-7000-8000-000000000001";
    let entry = |model: &str| {
        let capabilities = dim_agent_types::CatalogCapabilities {
            reasoning: true,
            ..dim_agent_types::CatalogCapabilities::with_limits(32000, 4096)
        };
        json!({"modelId":model,"displayName":model,"status":"active","capabilities":capabilities,
            "metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"],
                "off":{"kind":"thinking_type","value":"disabled"}}}})
    };
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":provider,"displayName":"Fixture",
                    "connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1",
                        "auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},
                    "models":[entry("alpha"),entry("beta")],"enabled":true}),
            )
            .await,
    );
    let created = client.rpc("session.create", json!({"cwd":work})).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let reviewer = profile(&mut client, "Reviewer", Some((provider, "alpha"))).await;
    assert_eq!(
        adopt(&mut client, &id, &reviewer).await["outcome"],
        "adopted"
    );
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        Some(reviewer.clone())
    );

    let preference = client
        .rpc(
            "session.model.update",
            json!({"sessionId":id,"branchId":"main","modelPreferences":{"reasoningEffort":"low"}}),
        )
        .await;
    assert_ok(&preference);
    assert_eq!(preference["result"]["outcome"], "adopted");
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        Some(reviewer.clone()),
        "a preference-only update keeps the profile"
    );

    let reselected = client
        .rpc(
            "session.model.update",
            json!({"sessionId":id,"branchId":"main","providerId":provider,"modelId":"beta"}),
        )
        .await;
    assert_ok(&reselected);
    assert_eq!(
        reselected["result"]["model"]["modelId"], "beta",
        "{reselected}"
    );
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        Some(reviewer),
        "a model selection keeps the profile whose prompt the branch runs"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
