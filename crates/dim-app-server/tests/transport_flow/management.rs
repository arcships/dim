use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_contributions_survive_reload_and_keep_entry_boundaries() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("dm-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
    )
    .await;
    // Methods contract §3.2: Rpc depends on the management table but
    // reaches no management owner (a part that registers with the table)
    // through its dependencies, directly or through Agents, so no owner can
    // keep Rpc from loading or reload it.
    let snapshot = server.diagnostics();
    let rpc = snapshot.plugins.iter().find(|p| p.name == "Rpc").unwrap();
    let owners: std::collections::BTreeSet<_> = snapshot
        .plugins
        .iter()
        .filter(|plugin| {
            plugin.id != rpc.id
                && plugin
                    .resolved_dependencies
                    .iter()
                    .any(|dependency| dependency.key.describe().contains("ManagementMethods"))
        })
        .map(|plugin| plugin.id)
        .collect();
    assert!(owners.len() >= 5, "management owners: {owners:?}");
    let mut pending = vec![rpc.id];
    let mut reached = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        let plugin = snapshot.plugins.iter().find(|p| p.id == id).unwrap();
        for dependency in &plugin.resolved_dependencies {
            if reached.insert(dependency.provider) {
                pending.push(dependency.provider);
            }
        }
    }
    let names = |ids: &std::collections::BTreeSet<_>| {
        snapshot
            .plugins
            .iter()
            .filter(|p| ids.contains(&p.id))
            .map(|p| p.name.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        reached.is_disjoint(&owners),
        "Rpc depends on management owners: {:?}",
        names(&reached.intersection(&owners).copied().collect())
    );
    let mut client = Client::new_version(&server, false, 3).await;
    let features = client.features.clone();
    for method in [
        "account.providers",
        "account.codex.status",
        "account.grok.status",
        "provider.list",
        "mcp.catalog",
        "skills.catalog",
    ] {
        assert!(
            features.as_array().unwrap().contains(&json!(method)),
            "{method}"
        );
        assert_ok(&client.rpc(method, json!({})).await);
    }
    assert!(
        !features
            .as_array()
            .unwrap()
            .contains(&json!("management.methods"))
    );
    assert_eq!(
        client.rpc("management.methods", json!({})).await["error"]["code"],
        -32601
    );
    assert_eq!(
        client.rpc("account.codex.unknown", json!({})).await["error"]["code"],
        -32601
    );
    let reply = client
        .rpc(
            "mcp.extra.echo",
            json!({"sessionId": uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    assert_eq!(
        reply["error"]["data"]["appError"], "unknown_method",
        "{reply}"
    );
    server.restart("Mcp").await.unwrap();
    for method in ["mcp.catalog", "skills.catalog", "account.codex.status"] {
        assert_ok(&client.rpc(method, json!({})).await);
    }
    assert_eq!(
        Client::new_version(&server, false, 3).await.features,
        features
    );
    server.restart("Rpc").await.unwrap();
    client = Client::new_version(&server, false, 3).await;
    assert_eq!(client.features, features);
    for method in ["mcp.catalog", "skills.catalog", "account.codex.status"] {
        assert_ok(&client.rpc(method, json!({})).await);
    }
    let mut remote = Client::new_version(&server, true, 3).await;
    let mut legacy = Client::new_version(&server, false, 2).await;
    for (method, params) in [
        ("mcp.catalog", json!({})),
        ("skills.catalog", json!({})),
        ("skills.preview", json!({"source": {"path": "/"} })),
        ("skills.installations", json!({})),
        ("provider.catalog", json!({})),
        ("configuration.catalog", json!({})),
    ] {
        assert_eq!(
            remote.rpc(method, params.clone()).await["error"]["data"]["appError"],
            "local_transport_required"
        );
        assert_eq!(
            legacy.rpc(method, params).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
