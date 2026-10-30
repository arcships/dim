use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolation_settings_are_local_revisioned_and_preserve_policy_on_errors() {
    use std::os::unix::fs::DirBuilderExt;

    let home = std::path::PathBuf::from("/tmp").join(format!("di-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let home = home.canonicalize().unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::new,
        dim_agent_capabilities::workspace_tools::WorkspaceToolsConfig {
            output_root: home.join("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.clone()),
            unix_path: Some(home.join("daemon.sock")),
            websocket_address: Some(([127, 0, 0, 1], 0).into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let mut remote = Client::new_version(&server, true, 3).await;
    for method in [
        "host.isolation.get",
        "host.isolation.update",
        "host.isolation.reset",
    ] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            remote.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let path = home.join("host-isolation.json");
    let initial = client.rpc("host.isolation.get", json!({})).await;
    assert_ok(&initial);
    assert_eq!(initial["result"]["path"], json!(path));
    assert_eq!(initial["result"]["source"], "defaults");
    assert_eq!(
        initial["result"]["effective"],
        json!({"sandbox":"workspace","escalation":"ask"})
    );
    assert!(!path.exists());
    let revision = initial["result"]["revision"].clone();
    let policy = json!({"version":2,"sandbox":"host","readOnlyPaths":[],"writablePaths":[],"environment":[]});
    let saved = client
        .rpc(
            "host.isolation.update",
            json!({"expectedRevision":revision,"policy":policy}),
        )
        .await;
    assert_ok(&saved);
    assert_eq!(saved["result"]["policy"], policy);
    assert_eq!(saved["result"]["effective"]["escalation"], "ask");
    assert_eq!(saved["result"]["source"], "file");
    let bytes = std::fs::read(&path).unwrap();
    let current_revision = saved["result"]["revision"].clone();
    for invalid in [
        json!({"version":2,"sandbox":"host","environment":["BAD=NAME"]}),
        json!({"version":2,"sandbox":"host","readOnlyPaths":["relative"]}),
        json!({"version":2,"environment":null}),
        json!({"version":2,"unexpected":true}),
    ] {
        let result = client
            .rpc(
                "host.isolation.update",
                json!({"expectedRevision":current_revision,"policy":invalid}),
            )
            .await;
        assert_eq!(
            result["error"]["data"]["appError"], "invalid_params",
            "{result}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    for method in ["host.isolation.update", "host.isolation.reset"] {
        let params = if method.ends_with("update") {
            json!({"expectedRevision":revision,"policy":policy})
        } else {
            json!({"expectedRevision":revision})
        };
        let result = client.rpc(method, params).await;
        assert_eq!(
            result["error"]["data"]["appError"],
            "isolation_policy_conflict"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    std::fs::write(&path, br#"{"version":2,"escalation":"deny"}"#).unwrap();
    let result = client
        .rpc(
            "host.isolation.reset",
            json!({"expectedRevision":current_revision}),
        )
        .await;
    assert_eq!(
        result["error"]["data"]["appError"],
        "isolation_policy_conflict"
    );
    let external = client.rpc("host.isolation.get", json!({})).await;
    assert_ok(&external);
    assert_eq!(external["result"]["effective"]["escalation"], "deny");
    let reset = client
        .rpc(
            "host.isolation.reset",
            json!({"expectedRevision":external["result"]["revision"]}),
        )
        .await;
    assert_ok(&reset);
    assert_eq!(reset["result"]["source"], "file");
    assert_eq!(
        reset["result"]["policy"],
        json!({"version":2,"sandbox":"workspace","escalation":"ask"})
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
