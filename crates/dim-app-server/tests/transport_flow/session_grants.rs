use super::*;

/// A stored write grant lets the next write run without an approval prompt;
/// removing it makes the next write ask again. Both write paths are relative
/// to the Session workspace and name a file that does not exist yet, so the
/// admitted write and the later ask also pin the daemon's canonical
/// resolution. An outside read asks until a read grant admits it. Grant
/// matching is covered by `permission_flow`, and the stored rows by
/// `storage_flow`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_grant_admits_the_next_write_until_it_is_removed() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset};
    use std::os::unix::fs::DirBuilderExt;

    let home = std::path::PathBuf::from("/tmp").join(format!("dg-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let home = home.canonicalize().unwrap();
    let native = home.join("rust");
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let outside = home.join("outside.txt");
    let read_outside = home.join("read-outside.txt");
    std::fs::write(&read_outside, "read grant target").unwrap();
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
    dim_app_server::session_grants::install(&mut builder);
    dim_agent_capabilities::file_access::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            unix_path: Some(native.join("daemon.sock")),
            websocket_address: Some(([127, 0, 0, 1], 0).into()),
            session_defaults: SessionConfig {
                tools: Some(vec!["read".into(), "write".into()]),
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
    let caps = dim_agent_types::CatalogCapabilities::with_limits(32000, 2048);
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

    for method in ["session.grants.list", "session.grants.lookup"] {
        let reply = client.rpc(method, json!({"sessionId":sid})).await;
        assert_eq!(
            reply["error"]["data"]["appError"], "unknown_method",
            "{reply}"
        );
    }

    assert_ok(
        &client
            .rpc(
                "session.grants.add",
                json!({"sessionId":sid,"path":"../outside.txt","kind":"file","mode":"write"}),
            )
            .await,
    );
    let write = json!({
        "sessionId":sid,"branchId":"main",
        "text":format!("tool:write {}", json!({"path":outside,"content":"granted"}))
    });
    assert_ok(&client.rpc("session.submit", write).await);
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    let result = session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "granted");
    assert_eq!(approval_requests(&session).await, 0);

    assert_ok(
        &client
            .rpc(
                "session.grants.remove",
                json!({"sessionId":sid,"path":"../outside.txt"}),
            )
            .await,
    );
    let write = json!({
        "sessionId":sid,"branchId":"main",
        "text":format!("tool:write {}", json!({"path":outside,"content":"again"}))
    });
    assert_ok(&client.rpc("session.submit", write).await);
    let approval = client.notice("approval.request").await;
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":sid,"branchId":"main","requestId":approval["params"]["requestId"],"decision":"deny"}),
            )
            .await,
    );
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    assert_eq!(approval_requests(&session).await, 1);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "granted");

    // An outside read asks; a read grant added for the Session admits the
    // recheck after the answer.
    let read = json!({
        "sessionId":sid,"branchId":"main",
        "text":format!("tool:read {}", json!({"path":read_outside}))
    });
    assert_ok(&client.rpc("session.submit", read.clone()).await);
    let approval = client.notice("approval.request").await;
    assert_eq!(approval["params"]["call"]["name"], "read");
    assert!(
        approval["params"]["reason"]
            .as_str()
            .unwrap()
            .contains(&read_outside.to_string_lossy().to_string()),
        "{}",
        approval["params"]["reason"]
    );
    assert_ok(
        &client
            .rpc(
                "session.grants.add",
                json!({"sessionId":sid,"path":read_outside,"kind":"file","mode":"read"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":sid,"branchId":"main","requestId":approval["params"]["requestId"],"decision":"allow"}),
            )
            .await,
    );
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    let result = session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let dim_agent_types::ToolContent::Text(read_content) = &result.content else {
        panic!("text result expected: {result:?}");
    };
    assert!(read_content.contains("read grant target"), "{read_content}");

    // The read grant covers the next outside read without an approval.
    let before = approval_requests(&session).await;
    assert_ok(&client.rpc("session.submit", read).await);
    assert_ok(&client.rpc("session.idle", branch).await);
    assert_eq!(approval_requests(&session).await, before);

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

async fn approval_requests(session: &dim_agent_runtime::Session) -> usize {
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
        .count()
}
