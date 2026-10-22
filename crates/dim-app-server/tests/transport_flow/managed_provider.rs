use super::*;
use axum::{Router, body::Body, http::StatusCode, routing::post};
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    file_storage::{FileStorage, FileStorageError, FileStoragePlugin},
    history::HistoryRenderer,
    host::{CompactionStrategyHost, ContextModelHost, IdentityContext, PrepareContextHost},
    model_provider::ModelProviderPlugin,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_app_server::managed_provider::ManagedPluginConfig;
use std::path::PathBuf;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_RUTIS_SDK"]
async fn node_managed_provider_recovers_credentials_and_failure_is_local() {
    let node = PathBuf::from(std::env::var_os("DIM_TEST_NODE").expect("DIM_TEST_NODE"));
    let js_runtime =
        PathBuf::from(std::env::var_os("DIM_TEST_JS_RUNTIME").expect("DIM_TEST_JS_RUNTIME"));
    let sdk = url::Url::from_file_path(PathBuf::from(
        std::env::var_os("DIM_TEST_RUTIS_SDK").expect("DIM_TEST_RUTIS_SDK"),
    ))
    .unwrap();
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = home.canonicalize().unwrap();
    let plugin_dir = home.join("plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("package.json"),
        r#"{"type":"module","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("dim-plugin.json"),
        r#"{"schemaVersion":1,"hostApiVersion":2,"rutisVersion":"0.10.0","entry":"index.mjs"}"#,
    )
    .unwrap();
    let fixture = include_str!("../fixtures/managed-provider.mjs").replace(
        "'__RUTIS_SDK__'",
        &serde_json::to_string(sdk.as_str()).unwrap(),
    );
    std::fs::write(plugin_dir.join("index.mjs"), fixture).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(std::sync::Mutex::new(vec![]));
    let seen_handler = seen.clone();
    let app=Router::new().route("/v1/chat/completions",post(move|headers:axum::http::HeaderMap|{
        let seen=seen_handler.clone();async move {
            let bearer=headers.get("authorization").unwrap().to_str().unwrap().to_owned();
            seen.lock().unwrap().push(bearer.clone());
            if bearer=="Bearer initial-fixture-secret" {return axum::response::Response::builder().status(StatusCode::UNAUTHORIZED).header("content-type","application/json").body(Body::from(r#"{"error":{"message":"rejected"}}"#)).unwrap();}
            assert!(matches!(bearer.as_str(),"Bearer recovered-fixture-secret"|"Bearer static-fixture-secret"));
            axum::response::Response::builder().header("content-type","text/event-stream").body(Body::from("data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"managed reply\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")).unwrap()
        }
    }));
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let capabilities = dim_agent_types::ModelCapabilities {
        context_window: 32_000,
        max_output_tokens: 128,
        ..Default::default()
    };
    let entry = json!({"modelId":"fixture-model","displayName":"Fixture model","status":"active","capabilities":capabilities});
    std::fs::write(
        home.join("fixture.json"),
        json!({"baseUrl":base,"entry":entry}).to_string(),
    )
    .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
            output_root: home.join("output"),
        },
    );
    dim_app_server::permission_rules::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    // Neither the JS row ID nor its module path is registered in Builtins.
    let names = [
        "ManagedProviderPlugin".to_owned(),
        plugin_dir.join("index.mjs").to_str().unwrap().to_owned(),
    ];
    let counts: Vec<_> = names
        .iter()
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();
    for (name, count) in names.iter().zip(&counts) {
        let (name, count) = (name.clone(), count.clone());
        builder.session_plugin(move |_| reload::NamedExtension(name.clone(), count.clone()));
    }
    let config = ServerConfig {
        unix_path: Some(home.join("daemon.sock")),
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_plugin: Some(ManagedPluginConfig {
            plugin_dir,
            node,
            js_runtime,
        }),
        session_typescript: Some(process_typescript::file_parts()),
        session_defaults: dim_agent_runtime::SessionConfig {
            tools: [
                "read", "write", "edit", "glob", "grep", "exec", "wait", "skill",
            ]
            .map(String::from)
            .to_vec(),
            permission_policy: dim_agent_types::PermissionPolicy {
                preset: dim_agent_types::PermissionPreset::WorkspaceWrite,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let server = serve(builder, config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        process_typescript::files_available(&mut client, "files.missing", json!({})).await,
        json!({"ok":false,"error":"io"})
    );
    assert_eq!(
        client.rpc("account.providers", json!({})).await["result"][0]["providerId"],
        "managed-fixture"
    );
    let error = client
        .rpc("account.status", json!({"providerId":"fail"}))
        .await;
    assert_eq!(error["error"]["data"]["appError"], "account_invalid_store");
    assert!(!error.to_string().contains("private-error-secret"));
    let failed = client
        .rpc(
            "account.login.status",
            json!({"providerId":"managed-fixture","attemptId":"failed-attempt"}),
        )
        .await;
    assert_eq!(
        failed["result"],
        json!({"status":"failed","error":"Login failed"})
    );
    assert_eq!(
        client
            .rpc(
                "account.credential",
                json!({"providerId":"managed-fixture"})
            )
            .await["error"]["code"],
        -32601
    );
    assert_ok(
        &client
            .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
            .await,
    );
    async fn prompt(
        client: &mut Client,
        server: &RunningServer,
        home: &std::path::Path,
        provider: &str,
    ) -> String {
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"model":{"provider":provider,"modelId":"fixture-model"}}),
            )
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":sid,"branchId":"main","text":"Hello"}),
                )
                .await,
        );
        server
            .agent()
            .unwrap()
            .get_session(sid)
            .unwrap()
            .idle("main")
            .await
            .unwrap();
        let history = client
            .rpc(
                "session.history",
                json!({"sessionId":sid,"branchId":"main","after":0,"limit":1000}),
            )
            .await;
        assert_ok(&history);
        history.to_string()
    }
    let history = prompt(&mut client, &server, &home, "managed-fixture").await;
    for (name, count) in names.iter().zip(counts) {
        assert_eq!(count.load(Ordering::SeqCst), 1, "{name}");
    }
    assert!(history.contains("managed reply"), "{history}");
    assert!(!history.contains("fixture-secret"));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "Bearer initial-fixture-secret",
            "Bearer recovered-fixture-secret"
        ]
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let calls = std::fs::read_to_string(home.join("calls.jsonl")).unwrap();
            if calls.matches("release").count() == 2 {
                assert_eq!(calls.matches("recoverRejected").count(), 1);
                assert!(!calls.contains("fixture-secret"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_ok(&client.rpc("provider.save",json!({"provider":{"providerId":"static-fixture","displayName":"Static fixture","driverKind":"openai-compatible","baseUrl":base,"auth":{"type":"bearer"},"credential":{"action":"set","value":"static-fixture-secret"},"models":[entry],"enabled":true}})).await);
    // A separate storage owner probes physical contention, not a lock filename
    // or the server's in-memory lease registry.
    let probe = rutis::Ctx::root().unwrap();
    (&probe.plugin(capability_plugin(FileStoragePlugin::new(vec![
        home.clone(),
    ]))))
        .await
        .unwrap();
    let storage = probe.get::<FileStorage>().unwrap();
    let path = storage.path(0, "fixture-state").unwrap();
    let cancel = CancellationToken::new();
    let mut crash = Client::new_version(&server, false, 3).await;
    let pending = client
        .send("account.status", json!({"providerId":"terminate-runtime"}))
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while serde_json::from_slice::<Value>(&storage.read(&path, &cancel).await.unwrap()).unwrap()
            != json!({"version":1,"text":"before-crash"})
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("runtime entered the protected transaction");
    assert!(matches!(
        storage.lock(&path, &cancel).await,
        Err(FileStorageError::Locked)
    ));
    let crashed = crash
        .rpc("account.status", json!({"providerId":"crash-runtime"}))
        .await;
    assert!(crashed.get("error").is_some());
    let lost = client.response(pending).await;
    assert!(lost.get("error").is_some());
    assert!(!lost.to_string().contains("fixture-secret"));
    let mut lease = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match storage.lock(&path, &cancel).await {
                Ok(lease) => break lease,
                Err(FileStorageError::Locked) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("lock probe failed: {error}"),
            }
        }
    })
    .await
    .expect("runtime loss must release the native transaction");
    assert_eq!(
        serde_json::from_slice::<Value>(&lease.read(&cancel).await.unwrap()).unwrap(),
        json!({"version":1,"text":"before-crash"})
    );
    lease
        .replace(
            serde_json::to_vec(&json!({"version":1,"text":"after-crash"})).unwrap(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    drop(lease);
    probe.shutdown().await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(home.join("fixture-state")).unwrap())
            .unwrap(),
        json!({"version":1,"text":"after-crash"})
    );
    assert!(
        prompt(&mut client, &server, &home, "static-fixture")
            .await
            .contains("managed reply")
    );
    assert_eq!(
        seen.lock().unwrap().last().unwrap(),
        "Bearer static-fixture-secret"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    http.abort();

    for name in names {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.session_extension(&name, "extension", |_, _: &()| {
            reload::NamedExtension("extension".into(), Arc::new(AtomicUsize::new(0)))
        });
        let error = serve(builder, config.clone())
            .await
            .err()
            .expect("a group must not shadow a JS process module or row");
        assert!(
            error
                .to_string()
                .contains(&format!("reserved extension group: {name}")),
            "{error}"
        );
    }
    std::fs::remove_dir_all(home).unwrap();
}
