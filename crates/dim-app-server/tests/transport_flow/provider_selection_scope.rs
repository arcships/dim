//! `provider.selection` reads one scope's row, and the selection CAS is per
//! scope: the contract a client relies on when it writes the default a folder's
//! next Session reads. A `cwd` reads only that scope's row and answers `null`
//! when the folder has none; the workspace-over-global precedence is
//! `configuration`'s, which a new Session resolves through `session.create`.
use super::*;

const PROVIDER: &str = "018ec73f-0000-7000-8000-0000000000aa";

fn models() -> Value {
    let capabilities = dim_agent_types::CatalogCapabilities::with_limits(32_000, 2_048);
    let entry = |model: &str| {
        json!({"modelId":model,"displayName":model,"status":"active",
            "capabilities":capabilities.clone()})
    };
    json!([entry("alpha"), entry("beta")])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_selection_reads_only_the_scope_it_names() {
    let home = std::path::PathBuf::from("/tmp").join(format!("dp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let native = home.join("rust");
    let folder = home.join("workspace");
    let other = home.join("elsewhere");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":PROVIDER,"displayName":"Fixture",
                    "connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:1/v1","auth":{"type":"anonymous"}},
                    "models":models(),"enabled":true}),
            )
            .await,
    );
    let alpha = client
        .rpc(
            "provider.select",
            json!({"cwd":folder,"providerId":PROVIDER,"modelId":"alpha"}),
        )
        .await;
    assert_ok(&alpha);
    assert_eq!(alpha["result"]["version"], 1);
    let beta = client
        .rpc(
            "provider.select",
            json!({"providerId":PROVIDER,"modelId":"beta"}),
        )
        .await;
    assert_ok(&beta);
    assert_eq!(beta["result"]["version"], 1);

    assert_eq!(
        client
            .rpc("provider.selection", json!({"cwd":folder}))
            .await["result"],
        json!({"selection":{"provider_id":PROVIDER,"model_id":"alpha","version":1}}),
        "the folder reads its own row"
    );
    assert_eq!(
        client.rpc("provider.selection", json!({"cwd":other})).await["result"],
        json!({"selection":null}),
        "a folder without a row reads none, never the global one"
    );
    assert_eq!(
        client.rpc("provider.selection", json!({})).await["result"],
        json!({"selection":{"provider_id":PROVIDER,"model_id":"beta","version":1}}),
        "a request without a folder reads the global row"
    );

    // A new Session resolves the folder's row first, then the global one.
    let in_folder = client
        .rpc("session.create", json!({"cwd":folder,"tools":[]}))
        .await;
    assert_ok(&in_folder);
    assert_eq!(in_folder["result"]["model"]["modelId"], "alpha");
    let in_other = client
        .rpc("session.create", json!({"cwd":other,"tools":[]}))
        .await;
    assert_ok(&in_other);
    assert_eq!(in_other["result"]["model"]["modelId"], "beta");

    // The CAS is per scope: the folder's third write is at its own version,
    // and the global row's version cannot replace the folder's row.
    let advanced = client
        .rpc(
            "provider.select",
            json!({"cwd":folder,"providerId":PROVIDER,"modelId":"alpha","expectedVersion":1}),
        )
        .await;
    assert_ok(&advanced);
    assert_eq!(advanced["result"]["version"], 2);
    let stale = client
        .rpc(
            "provider.select",
            json!({"cwd":folder,"providerId":PROVIDER,"modelId":"beta","expectedVersion":1}),
        )
        .await;
    assert_eq!(
        stale["error"]["data"]["appError"], "provider_conflict",
        "{stale}"
    );
    let written = client
        .rpc(
            "provider.select",
            json!({"cwd":folder,"providerId":PROVIDER,"modelId":"beta","expectedVersion":2}),
        )
        .await;
    assert_ok(&written);
    assert_eq!(written["result"]["version"], 3);
    assert_eq!(
        client.rpc("provider.selection", json!({})).await["result"],
        json!({"selection":{"provider_id":PROVIDER,"model_id":"beta","version":1}}),
        "a workspace write leaves the global row where it was"
    );

    for session in [&in_folder, &in_other] {
        assert_ok(
            &client
                .rpc(
                    "session.close",
                    json!({"sessionId":session["result"]["sessionId"]}),
                )
                .await,
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
}
