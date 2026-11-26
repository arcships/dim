use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_shell_settings_are_revisioned_and_used_by_the_host() {
    let home = std::path::PathBuf::from("/tmp").join(format!("ds-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("native")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut remote = Client::new_version(&server, true, 3).await;
    for method in ["host.exec_shell.get", "host.exec_shell.update"] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            remote.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let initial = client.rpc("host.exec_shell.get", json!({})).await;
    assert_ok(&initial);
    assert_eq!(initial["result"]["revision"], "absent");
    assert!(initial["result"]["executablePath"].is_null());
    let executable = initial["result"]["shells"][0]["executablePath"].clone();
    assert!(executable.as_str().is_some_and(|path| !path.is_empty()));
    let saved = client
        .rpc(
            "host.exec_shell.update",
            json!({
                "expectedRevision":"absent", "executablePath":executable
            }),
        )
        .await;
    assert_ok(&saved);
    let path = home.join("exec-shell.json");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({
            "version":1,"executablePath":executable
        })
    );
    let restored = client.rpc("host.exec_shell.get", json!({})).await;
    assert_ok(&restored);
    assert_eq!(restored["result"]["revision"], saved["result"]["revision"]);
    assert_eq!(restored["result"]["executablePath"], executable);
    let env = std::env::vars_os().collect::<Vec<_>>();
    let resolution =
        dim_agent_capabilities::host_platform::resolve_shell(Ok(Some(bytes.clone())), &env).await;
    let dim_agent_capabilities::host_platform::ShellStatus::Available(shell) = resolution else {
        panic!("saved preference must be executable by HostPlatform");
    };
    assert_eq!(shell.executable_path.as_str(), executable.as_str().unwrap());
    for invalid in [
        json!({"expectedRevision":saved["result"]["revision"],"executablePath":"/missing/dim-test-shell"}),
        json!({"expectedRevision":saved["result"]["revision"],"executablePath":null}),
        json!({"expectedRevision":saved["result"]["revision"],"unexpected":true}),
    ] {
        let result = client.rpc("host.exec_shell.update", invalid).await;
        assert_eq!(
            result["error"]["data"]["appError"], "invalid_params",
            "{result}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    let conflict = client
        .rpc(
            "host.exec_shell.update",
            json!({"expectedRevision":"absent"}),
        )
        .await;
    assert_eq!(conflict["error"]["data"]["appError"], "exec_shell_conflict");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let automatic = client
        .rpc(
            "host.exec_shell.update",
            json!({"expectedRevision":saved["result"]["revision"]}),
        )
        .await;
    assert_ok(&automatic);
    assert!(automatic["result"]["executablePath"].is_null());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
        json!({"version":1})
    );
    std::fs::write(&path, b"invalid").unwrap();
    let broken = client.rpc("host.exec_shell.get", json!({})).await;
    assert_ok(&broken);
    assert!(broken["result"]["configError"].is_string());
    assert_ok(
        &client
            .rpc(
                "host.exec_shell.update",
                json!({"expectedRevision":broken["result"]["revision"]}),
            )
            .await,
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
