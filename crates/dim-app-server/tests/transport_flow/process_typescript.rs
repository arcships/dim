use super::*;
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use std::path::PathBuf;

fn parts() -> SessionTypeScriptConfig {
    let mut parts = session_typescript::fixture();
    parts.process_rows = vec![json!({
        "id":"process-example", "name":parts.anchor.parent().unwrap().join("fixtures/process-part.ts"),
        "provides":serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/process-part.json")).unwrap(),
        "settings":serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/settings-owner.settings.json")).unwrap(),
        "config":{},
    })];
    parts
}

pub(super) fn file_parts() -> SessionTypeScriptConfig {
    let mut parts = session_typescript::fixture();
    parts.rows.clear();
    parts.process_rows = vec![json!({
        "id":"files-process", "name":parts.anchor.parent().unwrap().join("fixtures/files-process.ts"),
        "provides":{"managementMethods:files":{"describe":"async","call":"async"}},
        "config":{},
    })];
    parts
}

pub(super) async fn files_available(client: &mut Client, method: &str, params: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc(method, params.clone()).await;
            if let Some(result) = reply.get("result") {
                return result.clone();
            }
            assert_eq!(
                reply["error"]["data"]["appError"], "service_reloading",
                "{reply}"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

fn config(home: &std::path::Path, parts: SessionTypeScriptConfig) -> ServerConfig {
    ServerConfig {
        product_home: Some(home.to_owned()),
        target_home: Some(home.join("rust")),
        session_typescript: Some(parts),
        ..Default::default()
    }
}

fn home() -> PathBuf {
    PathBuf::from("/tmp").join(format!("dm-proc-{}", uuid::Uuid::new_v4()))
}

async fn available(client: &mut Client, server: &RunningServer) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client
                .rpc("example.status", json!({"original":[1,true]}))
                .await;
            if reply.get("result").is_some() {
                return reply["result"].clone();
            }
            assert_eq!(
                reply["error"]["data"]["appError"],
                "service_reloading",
                "{reply}: {:?}",
                server
                    .diagnostics()
                    .plugins
                    .iter()
                    .filter(|plugin| plugin.error.is_some())
                    .collect::<Vec<_>>()
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

pub(super) async fn select_model(client: &mut Client) {
    assert_ok(&client.rpc("provider.save", json!({"provider":{
        "providerId":"process-fixture", "displayName":"Fixture", "driverKind":"openai-compatible",
        "baseUrl":"http://localhost/v1", "auth":{"type":"anonymous"}, "models":[{
            "modelId":"fixture", "displayName":"Fixture", "status":"active",
            "capabilities":dim_agent_types::ModelCapabilities::default()
        }], "enabled":true
    }})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"process-fixture","modelId":"fixture"}),
            )
            .await,
    );
}

async fn started(client: &mut Client, id: u64) {
    loop {
        let notice = client.notice("management.progress").await;
        if notice["params"]["id"] == id && notice["params"]["data"]["phase"] == "started" {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_public_plugin_files_process_storage_and_close_drain() {
    let home = home();
    std::fs::create_dir_all(home.join("public-files")).unwrap();
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut parts = file_parts();
    parts.process_rows[0]["config"]["gate"] = json!({
        "host":"127.0.0.1", "port":gate.local_addr().unwrap().port()
    });
    let (server, _, _) = session_typescript::start(config(&home, parts)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        files_available(&mut client, "files.read", json!({})).await,
        json!({"ok":true,"value":{"ok":true,"value":null}})
    );
    assert_eq!(
        client.rpc("files.write", json!({})).await["result"],
        json!({"ok":true,"value":{"ok":true,"value":null}})
    );
    assert_eq!(
        client.rpc("files.read", json!({})).await["result"],
        json!({"ok":true,"value":{"ok":true,"value":"public process text"}})
    );
    let stored = home.join("public-files/state.json");
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&stored).unwrap()).unwrap(),
        json!({"version":1,"text":"public process text"})
    );
    assert_eq!(
        client.rpc("files.missing", json!({})).await["result"],
        json!({"ok":false,"error":"io"})
    );
    assert!(!home.join("unprovisioned").exists());
    let invalid = home.join("public-files/invalid.json");
    let bytes = b"unversioned original text";
    std::fs::write(&invalid, bytes).unwrap();
    assert_eq!(
        client.rpc("files.invalid", json!({})).await["result"],
        json!({"ok":true,"value":{"ok":false,"error":"invalid_data"}})
    );
    assert_eq!(std::fs::read(invalid).unwrap(), bytes);

    let held = client.send("files.hold", json!({})).await;
    let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    started(&mut client, held).await;
    let close = client.send("files.close", json!({})).await;
    assert_eq!(
        client.rpc("files.read", json!({})).await["result"],
        json!({"ok":false,"error":"closed"})
    );
    {
        let closed = client.response(close);
        tokio::pin!(closed);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut closed)
                .await
                .is_err()
        );
        release.write_all(b"settle").await.unwrap();
        assert_ok(&closed.await);
    }
    assert_ok(&client.response(held).await);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(stored).unwrap()).unwrap(),
        json!({"version":1,"text":"settled public process text"})
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_public_plugin_files_session_only_configuration() {
    let home = home();
    std::fs::create_dir_all(home.join("public-files")).unwrap();
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut parts = session_typescript::fixture();
    assert!(parts.process_rows.is_empty());
    parts.rows = vec![json!({
        "id":"files-session", "name":parts.anchor.parent().unwrap().join("fixtures/files-session.ts"),
        "provides":{"sessionMethods:session.files":{"describe":"async","call":"async"}},
        "config":{"gate":{"host":"127.0.0.1", "port":gate.local_addr().unwrap().port()}},
    })];
    let (server, _, _) = session_typescript::start(config(&home, parts)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    let mut expected = json!(null);
    for _ in 0..2 {
        let session = session_typescript::new_session(&mut client, json!([])).await;
        let params = json!({"sessionId":session});
        assert_eq!(
            files_available(&mut client, "session.files.read", params.clone()).await,
            json!({"ok":true,"value":{"ok":true,"value":expected}})
        );
        assert_eq!(
            client.rpc("session.files.write", params.clone()).await["result"],
            json!({"ok":true,"value":{"ok":true,"value":null}})
        );
        let held = client.send("session.files.hold", params.clone()).await;
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let held = client.response(held).await;
        assert_ok(&held);
        assert_eq!(held["result"], Value::Null);
        let close = client.send("session.close", params).await;
        {
            let closed = client.response(close);
            tokio::pin!(closed);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut closed)
                    .await
                    .is_err()
            );
            release.write_all(b"settle").await.unwrap();
            assert_ok(&closed.await);
        }
        expected = json!("settled public Session text");
        assert_eq!(
            serde_json::from_slice::<Value>(
                &std::fs::read(home.join("public-files/state.json")).unwrap()
            )
            .unwrap(),
            json!({"version":1,"text":expected})
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_public_plugin_files_without_product_root_stays_optional() {
    let (server, _, _) = session_typescript::start(ServerConfig {
        session_typescript: Some(file_parts()),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        client.rpc("files.read", json!({})).await["error"]["data"]["appError"],
        "service_reloading"
    );
    let session = session_typescript::new_session(&mut client, json!([])).await;
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":session}))
            .await,
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_typescript_rejects_invalid_embedding_before_runtime_start() {
    let process = json!({"id":"example", "name":"/fixture/process.ts", "provides":{
        "managementMethods:example":{"describe":"async","call":"async"}
    }});
    let mut cases = Vec::new();
    for service in [
        "managementMethods:",
        "managementMethods:hello.child",
        "managementMethods:session.child",
        "managementMethods:bad-namespace",
        "managementMethods:daemon.shutdown.child",
        "settings",
        "workflowFiles",
        "treeCommands",
        "toolContribution:unexpected",
        "sessionMethods:session.example",
        "sessionEvents:example",
    ] {
        let mut row = process.clone();
        row["provides"] = json!({});
        row["provides"][service] = json!({"describe":"async","call":"async"});
        cases.push((vec![row], vec![]));
    }
    for service in ["dispatcher", "treeCommands"] {
        let mut row = process.clone();
        row["inject"] = json!([service]);
        cases.push((vec![row], vec![]));
    }
    let mut row = process.clone();
    row["provides"]["managementMethods:example"]["call"] = json!("sync");
    cases.push((vec![row], vec![]));
    cases.push((vec![process.clone(), process.clone()], vec![]));
    for field in ["id", "name"] {
        let mut row = process.clone();
        row[field] = json!("Rpc");
        cases.push((vec![row], vec![]));
    }
    cases.push((
        vec![process.clone()],
        vec![json!({"id":"session", "name":"/fixture/session.ts",
        "provides":{"sessionMethods:example.child":{"describe":"async","call":"async"}}})],
    ));
    cases.push((vec![], vec![process.clone()]));
    let native_collisions =
        ["settings.update", "provider.save", "account.login"].map(|namespace| {
            let mut row = process.clone();
            row["provides"] = json!({});
            row["provides"][format!("managementMethods:{namespace}")] =
                json!({"describe":"async","call":"async"});
            (vec![row], vec![], true)
        });
    for (process_rows, rows, native) in cases
        .into_iter()
        .map(|(process_rows, rows)| (process_rows, rows, false))
        .chain(native_collisions)
    {
        let product_home = native.then(home);
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_hosts(&mut builder);
        let result = serve(
            builder,
            ServerConfig {
                product_home: product_home.clone(),
                target_home: product_home.as_ref().map(|home| home.join("rust")),
                unix_path: Some(
                    std::env::temp_dir()
                        .join(format!("dm-invalid-{}/server.sock", uuid::Uuid::new_v4())),
                ),
                session_typescript: Some(SessionTypeScriptConfig {
                    node: "/unstarted/node".into(),
                    js_runtime: "/unstarted/runtime".into(),
                    anchor: "/unstarted/package.json".into(),
                    rows,
                    process_rows,
                }),
                ..Default::default()
            },
        )
        .await;
        let error = result.err().expect("invalid embedding must fail");
        assert!(matches!(
            error,
            dim_app_server::ServerError::Configuration(_)
        ));
        if let Some(home) = product_home {
            assert!(
                error.to_string().contains("overlaps a native owner"),
                "{error}"
            );
            std::fs::remove_dir_all(home).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_process_typescript_shared_owner_and_local_management_boundary() {
    let home = home();
    let (server, _, _) = session_typescript::start(config(&home, parts())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    let before = available(&mut client, &server).await;
    assert_eq!(before["params"], json!({"original":[1,true]}));
    assert!(!before["callId"].as_str().unwrap().is_empty());
    let features = Client::new_version(&server, false, 3).await.features;
    assert!(
        features
            .as_array()
            .unwrap()
            .contains(&json!("example.status"))
    );
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let created = client
            .rpc("session.create", create(json!(["echo", "ts_echo"])))
            .await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let status = client
            .rpc("session.typescript.status", json!({"sessionId":id}))
            .await;
        assert_ok(&status);
        assert_eq!(
            status["result"]["pid"], before["pid"],
            "one shared Node runtime"
        );
        sessions.push(id);
    }
    for id in sessions {
        assert_ok(&client.rpc("session.close", json!({"sessionId":id})).await);
        assert_eq!(
            available(&mut client, &server).await["generation"],
            before["generation"]
        );
    }
    assert_eq!(
        client.rpc("example.read", json!({})).await["result"]["maxResults"],
        5
    );
    let error = client.rpc("example.error", json!({})).await;
    assert_eq!(error["error"]["code"], -32011);
    assert_eq!(error["error"]["data"]["appError"], "fixture_error");
    let malformed = client.rpc("example.malformed", json!({})).await;
    assert_eq!(malformed["error"]["code"], -32603);
    assert!(!malformed.to_string().contains("extra"));
    let empty = client.rpc("example.undefined", json!({})).await;
    assert_ok(&empty);
    assert_eq!(empty["result"], Value::Null);

    let mut remote = Client::new_version(&server, true, 3).await;
    let mut legacy = Client::new_version(&server, false, 2).await;
    assert_eq!(
        remote.rpc("example.status", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );
    assert_eq!(
        legacy.rpc("example.status", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    assert!(server.restart("process-example").await.is_err());

    let report = client.send("example.report", json!({})).await;
    started(&mut client, report).await;
    let drain = client.send("example.drain", json!({})).await;
    started(&mut client, drain).await;
    server.restart("Rpc").await.unwrap();
    assert_eq!(
        client.response(report).await["error"]["data"]["appError"],
        "operation_interrupted"
    );
    let settled = client.response(drain).await;
    assert_ok(&settled);
    assert_eq!(settled["result"]["generation"], before["generation"]);
    assert_eq!(settled["result"]["alive"], true);
    assert_eq!(settled["result"]["cancelled"], true);
    client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        available(&mut client, &server).await["generation"],
        before["generation"]
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_process_typescript_owner_reload_keeps_native_session_work_running() {
    let home = home();
    let mut parts = parts();
    parts.rows[0]["name"] = json!(
        parts
            .anchor
            .parent()
            .unwrap()
            .join("fixtures/session-process-part.ts")
    );
    parts.rows[0]["config"] = json!({"processOwner":true});
    let (mut builder, _requests, release) = session_typescript::setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.session_plugin(move |scope| session_typescript::ControlledGate(*scope, send.clone()));
    let mut config = config(&home, parts);
    config.unix_path = Some(home.join("server.sock"));
    let server = serve(builder, config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    let process = available(&mut client, &server).await;
    let mut sessions = Vec::new();
    for _ in 0..2 {
        sessions.push(
            session_typescript::new_session(&mut client, json!(["ts_echo", "native_slow", "echo"]))
                .await,
        );
    }
    let (_, _, loader, registry) = controls.recv().await.unwrap();
    let retained = registry.contributed("ts_echo").unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sessions[0],"branchId":"main","text":"batch"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.typescript.wait_fast",
                json!({"sessionId":sessions[0]}),
            )
            .await,
    );
    loader.restart("process-example").await.unwrap();
    let current = available(&mut client, &server).await;
    assert_ne!(current["generation"], process["generation"]);
    let retired_error = "tool provider generation is unloading; this call was not made. The tool is offered again once its provider is active; a later request uses the current provider.";
    assert_eq!(
        retained
            .handler
            .execute(
                ToolCall {
                    id: "retained".into(),
                    name: "ts_echo".into(),
                    input: serde_json::from_value(json!({"text":"must not execute"})).unwrap(),
                },
                ToolExecutionContext {
                    turn_id: "retained".into(),
                    request_id: "retained".into(),
                    branch_id: "main".into(),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string(),
        retired_error,
    );
    let fresh = session_typescript::new_session(&mut client, json!(["ts_echo", "echo"])).await;
    session_typescript::submit(&mut client, &fresh, r#"tool:ts_echo {"text":"new owner"}"#).await;
    let fresh = server.agent().unwrap().get_session(&fresh).unwrap();
    let history = fresh.history("main", None).await.unwrap();
    let result = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(batch) => batch.results.first(),
            _ => None,
        })
        .unwrap();
    assert_ne!(result.result.is_error, Some(true));
    let dim_agent_types::ToolContent::Text(content) = &result.result.content else {
        panic!("text result")
    };
    assert_eq!(
        serde_json::from_str::<Value>(content).unwrap()["processGeneration"],
        current["generation"]
    );
    release.notify_one();
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":sessions[0],"branchId":"main"}),
            )
            .await,
    );
    let session = server.agent().unwrap().get_session(&sessions[0]).unwrap();
    assert!(session.history("main", None).await.unwrap().iter().any(|event| {
        matches!(&event.kind, EventKind::ToolResult(batch) if batch.results.iter().any(|result| {
            result.call_id == "slow"
                && result.result.content == dim_agent_types::ToolContent::Text("native slow completed".into())
                && result.result.is_error != Some(true)
        }))
    }), "the admitted native tool must finish across the optional owner's reload");
    // The existing Session adopts the replacement owner's tool for its next
    // request; only the retained old handler stays retired (#241).
    session_typescript::submit(
        &mut client,
        &sessions[0],
        r#"tool:ts_echo {"text":"existing session"}"#,
    )
    .await;
    let history = session.history("main", None).await.unwrap();
    let result = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(batch) => batch.results.first(),
            _ => None,
        })
        .unwrap();
    assert_ne!(result.result.is_error, Some(true), "{result:?}");
    let dim_agent_types::ToolContent::Text(content) = &result.result.content else {
        panic!("text result")
    };
    assert_eq!(
        serde_json::from_str::<Value>(content).unwrap()["processGeneration"],
        current["generation"]
    );
    for id in sessions {
        session_typescript::submit(
            &mut client,
            &id,
            r#"tool:echo {"text":"native survives owner reload"}"#,
        )
        .await;
        let session = server.agent().unwrap().get_session(&id).unwrap();
        assert!(session.history("main", None).await.unwrap().iter().any(|event| {
            matches!(&event.kind, EventKind::ToolResult(batch) if batch.results.iter().any(|result| {
                result.result.content == dim_agent_types::ToolContent::Text("native survives owner reload".into())
                    && result.result.is_error != Some(true)
            }))
        }));
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_process_typescript_dependency_reload_settles_calls_and_keeps_failures_local() {
    async fn unavailable(client: &mut Client) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = client.rpc("example.status", json!({})).await;
                let code = &reply["error"]["data"]["appError"];
                if code == "service_unavailable" {
                    break;
                }
                assert_eq!(code, "service_reloading", "{reply}");
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let home = home();
    let mut parts = parts();
    parts.process_rows[0]["config"] = json!({"failAfter":2});
    let (mut builder, _, _) = session_typescript::setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.session_plugin(move |scope| session_typescript::ControlledGate(*scope, send.clone()));
    let mut config = config(&home, parts);
    config.unix_path = Some(home.join("server.sock"));
    let server = serve(builder, config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    let before = available(&mut client, &server).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let report = client.send("example.report", json!({})).await;
    started(&mut client, report).await;
    let drain = client.send("example.drain", json!({})).await;
    started(&mut client, drain).await;
    let disconnected = {
        let mut disconnected = Client::new_version(&server, false, 3).await;
        let call = disconnected.send("example.drain", json!({})).await;
        started(&mut disconnected, call).await;
        disconnected
    };
    drop(disconnected);
    let settings = loader
        .entries()
        .into_iter()
        .find(|entry| entry.options["name"] == "Settings")
        .expect("Settings row");
    loader.restart(&settings.id).await.unwrap();
    assert_eq!(
        client.response(report).await["error"]["data"]["appError"],
        "operation_interrupted"
    );
    let settled = client.response(drain).await;
    assert_ok(&settled);
    assert_eq!(settled["result"]["generation"], before["generation"]);
    assert_eq!(settled["result"]["alive"], true);
    // Rpc publishes namespace status independently of the loader's failure.
    unavailable(&mut client).await;
    assert!(matches!(loader.get("process-example").unwrap().status,
        rutis_loader::EntryStatus::Running(ref state) if state.state == rutis::FiberState::Failed));
    for plugin in ["Rpc", "Agents"] {
        tokio::time::timeout(Duration::from_secs(10), server.restart(plugin))
            .await
            .unwrap()
            .unwrap();
        client = Client::new_version(&server, false, 3).await;
        unavailable(&mut client).await;
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_process_typescript_optional_pending_failed_unresolved_and_disabled_owners() {
    for mode in ["pending", "failed", "unresolved", "disabled", "bad_methods"] {
        let home = home();
        let mut parts = parts();
        parts.rows[0]["name"] = json!(
            parts
                .anchor
                .parent()
                .unwrap()
                .join("fixtures/session-process-part.ts")
        );
        parts.rows[0]["config"] = json!({"processOwner":true});
        let row = &mut parts.process_rows[0];
        match mode {
            "pending" => row["inject"] = json!(["missingProcessDependency"]),
            "failed" => row["config"] = json!({"failAfter":1}),
            "unresolved" => row["provides"]["extra"] = json!({"read":"sync"}),
            "disabled" => row["disabled"] = json!(true),
            "bad_methods" => row["config"] = json!({"badMethods":true}),
            _ => unreachable!(),
        }
        // A disabled child must not fall through to its active parent namespace.
        parts.process_rows.push(json!({"id":"disabled-child", "name":parts.anchor.parent().unwrap().join("fixtures/disabled-child.ts"),
            "disabled":true,"provides":{"managementMethods:example.child":{"describe":"async","call":"async"}}}));
        let (server, mut requests, _) = session_typescript::start(config(&home, parts)).await;
        let mut client = Client::new_version(&server, false, 3).await;
        select_model(&mut client).await;
        let expected = match mode {
            "pending" => "service_reloading",
            "disabled" => "unknown_method",
            _ => "service_unavailable",
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = client.rpc("example.status", json!({})).await;
                if reply["error"]["data"]["appError"] == expected {
                    break;
                }
                assert_eq!(
                    reply["error"]["data"]["appError"], "service_reloading",
                    "{mode}: {reply}"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client.rpc("example.child.status", json!({})).await["error"]["data"]["appError"],
            "unknown_method"
        );
        for plugin in ["Rpc", "Agents"] {
            server.restart(plugin).await.unwrap();
            client = Client::new_version(&server, false, 3).await;
            assert_eq!(
                client.rpc("example.status", json!({})).await["error"]["data"]["appError"],
                expected
            );
        }
        let created = client
            .rpc("session.create", create(json!(["echo", "ts_echo"])))
            .await;
        assert_ok(&created);
        let session = &created["result"]["sessionId"];
        assert_ok(&client.rpc("session.submit", json!({"sessionId":session,"branchId":"main","text":"native remains available"})).await);
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":session,"branchId":"main"}),
                )
                .await,
        );
        let tools = requests.recv().await.unwrap();
        assert!(tools.contains(&"echo".into()));
        assert!(!tools.contains(&"ts_echo".into()), "{mode}: {tools:?}");
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_process_typescript_held_apply_and_describe_do_not_hold_native_startup() {
    for hold in ["holdApply", "hangDescribe"] {
        let home = home();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut parts = parts();
        parts.process_rows[0]["config"] = json!({});
        parts.process_rows[0]["config"][hold] =
            json!({"host":"127.0.0.1","port":listener.local_addr().unwrap().port()});
        let (server, _, _) = tokio::time::timeout(
            Duration::from_secs(10),
            session_typescript::start(config(&home, parts)),
        )
        .await
        .unwrap();
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        select_model(&mut client).await;
        assert_eq!(
            client.rpc("example.status", json!({})).await["error"]["data"]["appError"],
            "service_reloading"
        );
        assert_ok(&client.rpc("session.create", create(json!(["echo"]))).await);
        socket.write_all(b"release").await.unwrap();
        assert_eq!(available(&mut client, &server).await["alive"], true);
        server.shutdown();
        server.join().await.unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio::io::AsyncReadExt::read(&mut socket, &mut byte)
            )
            .await
            .unwrap()
            .unwrap(),
            0
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

async fn workflow_file_call(client: &mut Client, method: &str, params: Value) -> Value {
    let reply = client.rpc(method, params).await;
    assert_ok(&reply);
    reply["result"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_files_enforces_project_and_original_row_package_authority() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let store = super::deletion::Store::new();
    let home = &store.0;
    let project = home.join("project");
    let package = home.join("package");
    let other = home.join("other");
    for path in [
        &project,
        &package.join("workflows"),
        &other.join("workflows"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    let package_bytes = b"name: package-flow\nstages: []\n";
    std::fs::write(package.join("workflows/flow.yaml"), package_bytes).unwrap();
    std::fs::write(other.join("workflows/private.yaml"), b"other row").unwrap();
    std::fs::write(
        home.join("workflow.json"),
        b"{\"definitions\":{},\"projectEnabled\":{}}",
    )
    .unwrap();
    let mut parts = session_typescript::fixture();
    parts.rows.clear();
    let row = |id: &str, module: &str, namespace: &str, id_root: &str, root: &std::path::Path| {
        json!({
            "id":id, "name":parts.anchor.parent().unwrap().join(format!("fixtures/{module}.ts")),
            "provides":{format!("managementMethods:{namespace}"):{"describe":"async","call":"async"}},
            "config":{"packages":[{"id":id_root,"root":root}]},
        })
    };
    let mut first = row(
        "workflow-reader",
        "workflow-files",
        "workflow_files_test",
        "installed",
        &package,
    );
    first["settings"] = json!([{
        "source":"workflow", "userFile":"workflow.json", "projectFile":null,
        "schema":{"type":"object","properties":{
            "definitions":{"type":"object","default":{}},
            "projectEnabled":{"type":"object","default":{}}
        },"additionalProperties":false},
        "userOnly":["/projectEnabled"]
    }]);
    parts.process_rows = vec![
        first,
        row(
            "other-reader",
            "workflow-files-other",
            "workflow_files_other",
            "other",
            &other,
        ),
    ];
    let mut config = config(home, parts);
    config.file_roots = vec![package.clone(), other.clone()];
    let (mut builder, _, _) = session_typescript::setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.session_plugin(move |scope| session_typescript::ControlledGate(*scope, send.clone()));
    config.unix_path = Some(PathBuf::from("/tmp").join(format!(
        "wf-{}/server.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )));
    let server = serve(builder, config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    for namespace in ["workflow_files_test", "workflow_files_other"] {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = client.rpc(&format!("{namespace}.status"), json!({})).await;
                if reply.get("result").is_some() {
                    break;
                }
                assert_eq!(
                    reply["error"]["data"]["appError"], "service_reloading",
                    "{reply}"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let project_root = json!({"project":project});
    // A host root alone does not authorize a project. Session creation does.
    let denied = workflow_file_call(
        &mut client,
        "workflow_files_test.list",
        json!({"root":project_root}),
    )
    .await;
    let error: Value = serde_json::from_str(denied["error"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "invalid_path", "{error}");
    assert_ok(
        &client
            .rpc("session.create", json!({"cwd":project,"tools":[]}))
            .await,
    );
    let missing = workflow_file_call(
        &mut client,
        "workflow_files_test.list",
        json!({"root":project_root}),
    )
    .await;
    let error: Value = serde_json::from_str(missing["error"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "not_found", "{error}");
    assert_eq!(
        workflow_file_call(
            &mut client,
            "workflow_files_test.create_directory",
            json!({"root":project_root})
        )
        .await,
        true
    );
    assert!(project.join(".dimcode/workflows").is_dir());
    assert!(!project.join("workflows").exists());
    let definition = vec![0xff_u8, 0, 0x80, b'\n'];
    let target = project.join(".dimcode/workflows/flow.yaml");
    std::fs::write(&target, b"old").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        workflow_file_call(
            &mut client,
            "workflow_files_test.replace",
            json!({"root":project_root,"path":"flow.yaml","bytes":definition})
        )
        .await,
        true
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        definition,
        "raw bytes without S10 wrapping"
    );
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let read = workflow_file_call(
        &mut client,
        "workflow_files_test.read",
        json!({"root":project_root,"path":"flow.yaml"}),
    )
    .await;
    assert_eq!(read, json!({"bytes":definition,"typed":true}));
    std::fs::create_dir(project.join(".dimcode/workflows/directory")).unwrap();
    symlink(
        home.join("workflow.json"),
        project.join(".dimcode/workflows/link.yaml"),
    )
    .unwrap();
    let entries = workflow_file_call(
        &mut client,
        "workflow_files_test.list",
        json!({"root":project_root}),
    )
    .await;
    let entries: Value = serde_json::from_str(entries.as_str().unwrap()).unwrap();
    assert_eq!(
        entries,
        json!([
            {"name":"directory","kind":"directory"},{"name":"flow.yaml","kind":"file"},{"name":"link.yaml","kind":"other"}
        ])
    );
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        let bad_name = project
            .join(".dimcode/workflows")
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        std::fs::write(&bad_name, b"untouched").unwrap();
        let bad = workflow_file_call(
            &mut client,
            "workflow_files_test.list",
            json!({"root":project_root}),
        )
        .await;
        let error: Value = serde_json::from_str(bad["error"].as_str().unwrap()).unwrap();
        assert_eq!(error["code"], "invalid_data");
        assert_eq!(std::fs::read(&bad_name).unwrap(), b"untouched");
        std::fs::remove_file(bad_name).unwrap();
    }
    let package_read = workflow_file_call(
        &mut client,
        "workflow_files_test.read",
        json!({"root":{"package":"installed"},"path":"flow.yaml"}),
    )
    .await;
    assert_eq!(
        package_read,
        json!({"bytes":package_bytes.to_vec(),"typed":true})
    );
    for (method, request) in [
        (
            "workflow_files_test.read",
            json!({"root":{"package":"other"},"path":"private.yaml"}),
        ),
        (
            "workflow_files_other.read",
            json!({"root":{"package":"installed"},"path":"flow.yaml"}),
        ),
        (
            "workflow_files_test.replace",
            json!({"root":{"package":"installed"},"path":"flow.yaml","bytes":[]}),
        ),
        (
            "workflow_files_test.create_directory",
            json!({"root":{"package":"installed"}}),
        ),
        (
            "workflow_files_test.read",
            json!({"root":{"project":home},"path":"workflow.yaml"}),
        ),
    ] {
        let denied = workflow_file_call(&mut client, method, request).await;
        let error: Value = serde_json::from_str(denied["error"].as_str().unwrap()).unwrap();
        assert_eq!(error["name"], "WorkflowFilesError", "{error}");
        assert_eq!(error["code"], "invalid_path", "{error}");
        assert_eq!(error["data"]["code"], "invalid_path");
        assert_eq!(error["data"]["message"], error["message"]);
    }
    for path in [
        "../workflow.json",
        "../../dimcode.sqlite",
        "/outside.yaml",
        "bad\\path.yaml",
        "workflow.json",
        ".mcp.json",
        "bad.yml",
    ] {
        for method in ["workflow_files_test.read", "workflow_files_test.replace"] {
            let mut request = json!({"root":project_root,"path":path});
            if method.ends_with("replace") {
                request["bytes"] = json!([]);
            }
            let denied = workflow_file_call(&mut client, method, request).await;
            let error: Value = serde_json::from_str(denied["error"].as_str().unwrap()).unwrap();
            assert_eq!(error["code"], "invalid_path", "{path}: {error}");
        }
    }
    let forged = workflow_file_call(
        &mut client,
        "workflow_files_test.read",
        json!({"root":0,"path":"flow.yaml"}),
    )
    .await;
    let error: Value = serde_json::from_str(forged["error"].as_str().unwrap()).unwrap();
    assert_ne!(
        error["name"], "WorkflowFilesError",
        "the generated decoder rejects raw indices"
    );
    let linked = workflow_file_call(
        &mut client,
        "workflow_files_test.read",
        json!({"root":project_root,"path":"link.yaml"}),
    )
    .await;
    let error: Value = serde_json::from_str(linked["error"].as_str().unwrap()).unwrap();
    assert_eq!(error["code"], "io", "a link must not expose home bytes");
    assert_eq!(
        std::fs::read(package.join("workflows/flow.yaml")).unwrap(),
        package_bytes
    );
    let settings = client
        .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
        .await;
    assert_ok(&settings);
    let mut document = settings["result"]["document"].clone();
    document["definitions"] = json!({"user-flow":{"name":"user-flow"}});
    document["projectEnabled"] = json!({project.to_str().unwrap():{"flow":true}});
    let denied = workflow_file_call(
        &mut client,
        "workflow_files_test.settings_update",
        document.clone(),
    )
    .await;
    let denied: Value = serde_json::from_str(denied.as_str().unwrap()).unwrap();
    assert_eq!(denied["error"]["code"], "user_only_field");
    assert_ok(&client.rpc("settings.update", json!({"source":"workflow","scope":"user","expectedRevision":settings["result"]["revision"],"document":document})).await);
    let read =
        workflow_file_call(&mut client, "workflow_files_test.settings_read", json!({})).await;
    let read: Value = serde_json::from_str(read.as_str().unwrap()).unwrap();
    assert_eq!(read["value"]["document"], document);
    let (_, _, loader, _) = controls.recv().await.unwrap();
    loader.restart("Settings").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let restored = client
                .rpc(
                    "workflow_files_test.read",
                    json!({"root":{"package":"installed"},"path":"flow.yaml"}),
                )
                .await;
            if restored.get("result").is_some() {
                assert_eq!(restored["result"], package_read);
                break;
            }
            assert_eq!(
                restored["error"]["data"]["appError"], "service_reloading",
                "{restored}"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // A package under an authorized parent still needs its own explicit host root.
    let unlisted = home.join("unlisted");
    std::fs::create_dir_all(unlisted.join("workflows")).unwrap();
    std::fs::write(unlisted.join("workflows/secret.yaml"), b"unlisted").unwrap();
    let patch = serde_json::from_value(
        json!({"id":"workflow-reader","config":{"packages":[{"id":"unlisted","root":unlisted}]}}),
    )
    .unwrap();
    let report = loader
        .set_overlay("workflow-package-roots", Some(vec![patch]))
        .await
        .unwrap();
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.id == "workflow-reader"
                && failure.error.contains("canonical authorized host root")),
        "{report:?}"
    );
    assert_ok(&client.rpc("session.list", json!({})).await);
    assert_eq!(
        std::fs::read(unlisted.join("workflows/secret.yaml")).unwrap(),
        b"unlisted"
    );
    server.shutdown();
    server.join().await.unwrap();
}
