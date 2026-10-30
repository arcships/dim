use super::*;

/// The command rules of the product profile: written through the
/// `permission` management methods, read by Permission on the next call of an
/// already open Session; a workspace file grants nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_rules_written_by_the_client_apply_to_the_next_call() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset, ToolContent};
    use std::os::unix::fs::DirBuilderExt;

    let home = std::path::PathBuf::from("/tmp").join(format!("dr-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    let work = home.join("work");
    std::fs::create_dir_all(work.join(".dimcode")).unwrap();
    let allow = json!({"command":"printf","match":"prefix","decision":"allow"});
    std::fs::write(
        work.join(".dimcode/permission.json"),
        json!({ "commandRules": [allow] }).to_string(),
    )
    .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
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
            websocket_address: Some(([127, 0, 0, 1], 0).into()),
            session_defaults: SessionConfig {
                tools: Some(vec!["exec".into()]),
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
    for method in ["permission.rules", "permission.set_rules"] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
    }
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(
        remote.rpc("permission.rules", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );

    let caps = dim_agent_types::ModelCapabilities {
        context_window: 32000,
        max_output_tokens: 2048,
        ..Default::default()
    };
    let entry =
        json!({"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps});
    assert_ok(&client.rpc("provider.create",json!({"providerId":"018ec73f-0000-7000-8000-000000000001","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},"models":[entry],"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000001","modelId":"alpha"}),
            )
            .await,
    );
    let created = client.rpc("session.create", json!({"cwd":work})).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let branch = json!({"sessionId":sid,"branchId":"main"});
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    let exec = json!({"sessionId":sid,"branchId":"main","text":"tool:exec {\"command\":\"printf ruled\",\"sandbox\":\"host\"}"});
    let last_result = async || {
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
                _ => None,
            })
            .unwrap()
    };
    let requests = async || {
        session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
            .count()
    };

    assert_ok(&client.rpc("session.submit", exec.clone()).await);
    let approval = client.notice("approval.request").await;
    assert_ok(&client.rpc("approval.respond", json!({"sessionId":sid,"branchId":"main","requestId":approval["params"]["requestId"],"decision":"deny"})).await);
    assert_ok(&client.rpc("session.idle", branch.clone()).await);

    let rules = client.rpc("permission.rules", json!({})).await;
    assert_ok(&rules);
    assert_eq!(rules["result"]["commandRules"], json!([]));
    let first = rules["result"]["revision"].clone();
    let written = client
        .rpc(
            "permission.set_rules",
            json!({"expectedRevision":first,"commandRules":[allow]}),
        )
        .await;
    assert_ok(&written);
    let second = written["result"]["revision"].clone();
    assert_ne!(second, first);
    assert_eq!(
        client
            .rpc(
                "permission.set_rules",
                json!({"expectedRevision":first,"commandRules":[]}),
            )
            .await["error"]["data"]["appError"],
        "permission_rules_conflict"
    );
    let separator = json!({"command":"printf a; rm","match":"exact","decision":"allow"});
    assert_eq!(
        client
            .rpc(
                "permission.set_rules",
                json!({"expectedRevision":second,"commandRules":[separator]}),
            )
            .await["error"]["data"]["appError"],
        "invalid_params"
    );
    let rules = client.rpc("permission.rules", json!({})).await;
    assert_eq!(rules["result"]["commandRules"], json!([allow]));
    assert_eq!(rules["result"]["revision"], second);

    assert_ok(&client.rpc("session.submit", exec.clone()).await);
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    assert_ne!(last_result().await.is_error, Some(true));
    assert_eq!(requests().await, 1);

    let deny = json!({"command":"printf","match":"prefix","decision":"deny"});
    assert_ok(
        &client
            .rpc(
                "permission.set_rules",
                json!({"expectedRevision":second,"commandRules":[allow, deny]}),
            )
            .await,
    );
    assert_ok(&client.rpc("session.submit", exec).await);
    assert_ok(&client.rpc("session.idle", branch).await);
    let denied = last_result().await;
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(
        denied.content,
        ToolContent::Text("Command denied by command rule `printf` (prefix)".into())
    );
    assert_eq!(requests().await, 1);
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
}
