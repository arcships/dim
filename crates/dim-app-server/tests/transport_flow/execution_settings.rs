use super::*;

/// The execution switches are user settings read at each process start: with
/// the sandbox on, a write outside the workspace fails, and the same write
/// succeeds once the sandbox is off. A call that still sends `sandbox` is
/// rejected rather than ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_settings_decide_the_next_process_boundary() {
    use dim_agent_capabilities::model_provider::ModelProviderPlugin;
    use dim_agent_capabilities::timeline_tree::BranchToolConfig;
    use dim_agent_capabilities::workspace_tools::WorkspaceToolsConfig;
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset};
    use std::os::unix::fs::DirBuilderExt;

    let home = std::path::PathBuf::from("/tmp").join(format!("ex-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    let work = home.join("work");
    std::fs::create_dir_all(work.join(".dimcode")).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: native.join("outputs"),
        },
        BranchToolConfig::default(),
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
                    preset: PermissionPreset::FullAccess,
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
    for method in ["settings.read", "settings.update"] {
        assert!(
            client.features.as_array().unwrap().contains(&json!(method)),
            "{method} is not offered"
        );
    }
    let caps = dim_agent_types::CatalogCapabilities::with_limits(32000, 2048);
    let entry =
        json!({"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps});
    let provider = json!({
        "providerId": "018ec73f-0000-7000-8000-000000000002",
        "displayName": "Fixture",
        "connection": {
            "driverKind": "openai-compatible",
            "baseUrl": "https://model.example/v1",
            "auth": {"type": "bearer"},
            "credential": {"action": "set", "value": "fixture-access"}
        },
        "models": [entry],
        "enabled": true
    });
    assert_ok(&client.rpc("provider.create", provider).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000002","modelId":"alpha"}),
            )
            .await,
    );
    let created = client.rpc("session.create", json!({"cwd":work})).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);

    let branch = json!({"sessionId":sid,"branchId":"main"});
    let outside = home.join("outside.txt");
    macro_rules! run {
        ($input:expr) => {{
            assert_ok(
                &client
                    .rpc(
                        "session.submit",
                        json!({"sessionId":sid,"branchId":"main","text":format!("tool:exec {}", $input)}),
                    )
                    .await,
            );
            assert_ok(&client.rpc("session.idle", branch.clone()).await);
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
        }};
    }

    let unreadable = home.join("execution.json");
    std::fs::create_dir(&unreadable).unwrap();
    let unconfigured = run!(json!({"command":"printf x"}));
    assert_eq!(unconfigured.is_error, Some(true));
    assert_eq!(
        unconfigured
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code"))
            .and_then(Value::as_str),
        Some("isolation_config_invalid"),
        "{unconfigured:?}"
    );
    std::fs::remove_dir(&unreadable).unwrap();

    let backend = client.rpc("host.execution.backend", json!({})).await;
    assert_ok(&backend);
    let available = backend["result"]["available"].as_bool().unwrap();
    #[cfg(target_os = "macos")]
    assert!(available, "macOS has the seatbelt backend: {backend}");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::fs::PermissionsExt;
        let expected = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|directory| {
                let bwrap = directory.join("bwrap");
                std::fs::metadata(&bwrap).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
        });
        assert_eq!(available, expected, "{backend}");
    }
    if !available {
        assert!(
            backend["result"]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "{backend}"
        );
    }

    let initial = client
        .rpc(
            "settings.read",
            json!({"source":"execution","scope":"user"}),
        )
        .await;
    assert_ok(&initial);
    assert_eq!(
        initial["result"]["document"],
        json!({"sandbox": false, "network": false})
    );
    let first = initial["result"]["revision"].clone();
    let sandboxed = client
        .rpc(
            "settings.update",
            json!({"source":"execution","scope":"user","expectedRevision":first,"document":{"sandbox":true,"network":false}}),
        )
        .await;
    assert_ok(&sandboxed);
    assert_eq!(
        sandboxed["result"]["document"],
        json!({"sandbox": true, "network": false})
    );
    assert_ne!(sandboxed["result"]["revision"], first);
    assert_eq!(
        client
            .rpc(
                "settings.update",
                json!({"source":"execution","scope":"user","expectedRevision":first,"document":{"sandbox":false,"network":false}}),
            )
            .await["error"]["data"]["appError"],
        "settings_conflict"
    );

    let rejected = run!(json!({"command":"printf x","sandbox":"host"}));
    assert_eq!(rejected.is_error, Some(true));
    assert!(
        matches!(
            &rejected.content,
            dim_agent_types::ToolContent::Text(text) if text.contains("exec accepts command")
        ),
        "{:?}",
        rejected.content
    );

    let mut revision = sandboxed["result"]["revision"].clone();
    if available {
        let denied = run!(json!({"command": format!(
            "printf hidden > {} && printf inside > inside.txt",
            outside.display()
        )}));
        assert_eq!(denied.is_error, Some(true));
        assert!(!outside.exists(), "the sandbox let a write out");
        assert!(!work.join("inside.txt").exists());

        let allowed = run!(json!({"command":"printf inside > inside.txt"}));
        assert_eq!(allowed.is_error, None);
        assert_eq!(
            std::fs::read_to_string(work.join("inside.txt")).unwrap(),
            "inside"
        );

        let networked = client
            .rpc(
                "settings.update",
                json!({"source":"execution","scope":"user","expectedRevision":revision,"document":{"sandbox":true,"network":true}}),
            )
            .await;
        assert_ok(&networked);
        assert_eq!(
            networked["result"]["document"],
            json!({"sandbox": true, "network": true})
        );
        revision = networked["result"]["revision"].clone();
        let denied = run!(json!({"command": format!(
            "printf hidden > {} && printf inside > networked.txt",
            outside.display()
        )}));
        assert_eq!(denied.is_error, Some(true), "Network widened file access");
        assert!(!outside.exists(), "the sandbox let a write out");
        assert!(!work.join("networked.txt").exists());
        let allowed = run!(json!({"command":"printf inside > networked.txt"}));
        assert_eq!(allowed.is_error, None);
    } else {
        let unavailable = run!(json!({"command":"printf x"}));
        assert_eq!(
            unavailable
                .structured_content
                .as_ref()
                .and_then(|content| content.get("code"))
                .and_then(Value::as_str),
            Some("isolation_unavailable"),
            "{unavailable:?}"
        );
    }

    let unsandboxed = client
        .rpc(
            "settings.update",
            json!({"source":"execution","scope":"user","expectedRevision":revision,"document":{"sandbox":false,"network":false}}),
        )
        .await;
    assert_ok(&unsandboxed);
    let written = run!(json!({"command": format!("printf visible > {}", outside.display())}));
    assert_eq!(written.is_error, None);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "visible");

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
}
