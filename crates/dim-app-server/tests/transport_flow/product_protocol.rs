use super::*;
use rutis::host::CtxHost;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_management_preserves_live_intent_and_separates_save_acknowledgement() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("dc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = home.join("rust");
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
    let mut legacy = Client::new_version(&server, false, 2).await;
    let mut remote = Client::new_version(&server, true, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("mcp.save"))
    );
    assert!(
        !remote
            .features
            .as_array()
            .unwrap()
            .contains(&json!("mcp.save"))
    );
    assert_eq!(
        legacy.rpc("mcp.catalog", json!({})).await["error"]["code"],
        -32601
    );
    assert_eq!(
        remote.rpc("mcp.catalog", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );
    let before = client.rpc("mcp.catalog", json!({"cwd":home})).await;
    assert_ok(&before);
    let mut edit = json!({"source":"user", "id":"private", "expectedVersion":before["result"]["userRevision"],
        "configuration":{"command":"never-start-this-fixture","enabled":false,"env":{"TOKEN":"secret-never-return"}}});
    let mut wrong_source = edit.clone();
    wrong_source["expectedVersion"] = before["result"]["projectRevision"].clone();
    assert_eq!(
        client.rpc("mcp.save", wrong_source).await["error"]["data"]["appError"],
        "mcp_settings_conflict"
    );
    assert_ok(&client.rpc("mcp.save", edit.clone()).await);
    let saved = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&saved);
    assert_ne!(
        saved["result"]["userRevision"],
        before["result"]["userRevision"]
    );
    assert_eq!(saved["result"]["servers"][0]["id"], "private");
    assert_eq!(saved["result"]["servers"][0]["enabled"], false);
    assert!(!saved.to_string().contains("secret-never-return"));
    assert_eq!(
        client.rpc("mcp.save", edit.clone()).await["error"]["data"]["appError"],
        "mcp_settings_conflict"
    );
    edit["expectedVersion"] = saved["result"]["userRevision"].clone();
    edit["configuration"]["enabled"] = json!(true);
    let fault = rusqlite::Connection::open(native.join("dimcode.sqlite")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE ON settings_documents BEGIN SELECT RAISE(ABORT, 'secret-never-return'); END").unwrap();
    let failed = client.rpc("mcp.save", edit.clone()).await;
    assert_eq!(failed["error"]["data"]["appError"], "mcp_settings_read");
    assert!(!failed.to_string().contains("secret-never-return"));
    let unchanged = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&unchanged);
    assert_eq!(
        unchanged["result"]["userRevision"],
        saved["result"]["userRevision"]
    );
    assert_eq!(unchanged["result"]["servers"], saved["result"]["servers"]);
    fault.execute_batch("DROP TRIGGER fail_save").unwrap();
    drop(fault);
    server.shutdown();
    server.join().await.unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let restored = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&restored);
    assert_eq!(
        restored["result"]["userRevision"],
        saved["result"]["userRevision"]
    );
    assert_eq!(restored["result"]["servers"], saved["result"]["servers"]);
    assert_ok(&client.rpc("mcp.save", edit).await);
    let retried = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&retried);
    assert_ne!(
        retried["result"]["userRevision"],
        saved["result"]["userRevision"]
    );
    assert_eq!(retried["result"]["servers"][0]["enabled"], true);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_editor_preserves_redacts_and_removes_credentials_with_source_cas() {
    let home = std::path::PathBuf::from("/tmp").join(format!("dm-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let native = home.join("native");
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
    let mut remote = Client::new_version(&server, true, 3).await;
    for method in ["mcp.configuration", "mcp.update"] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            remote.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    for (id, configuration, patch, removed) in [
        (
            "command",
            json!({"command":"never-start-this-fixture","args":["private-argument-fixture"],"enabled":false,"env":{"KEEP":"preserved-fixture-value","REPLACE":"old-fixture-value","DELETE":"deleted-fixture-value"},"extension":"preserved"}),
            json!({"env":{"REPLACE":"replacement-fixture-value","DELETE":null}}),
            json!({"args":[],"env":{"KEEP":null,"REPLACE":null}}),
        ),
        (
            "remote",
            json!({"type":"http","url":"https://url-user-fixture:url-password-fixture@example.invalid/url-path-fixture?access_token=url-query-fixture#url-fragment-fixture","enabled":false,"headers":{"KEEP":"preserved-fixture-value","REPLACE":"old-fixture-value","DELETE":"deleted-fixture-value"},"auth":{"type":"bearer","token":"bearer-fixture-value"},"extension":"preserved"}),
            json!({"headers":{"REPLACE":"replacement-fixture-value","DELETE":null}}),
            json!({"headers":{"KEEP":null,"REPLACE":null},"bearer":{"action":"remove"}}),
        ),
    ] {
        let before = client.rpc("mcp.catalog", json!({})).await;
        assert_ok(&client.rpc("mcp.save", json!({"source":"user","id":id,"expectedVersion":before["result"]["userRevision"],"configuration":configuration})).await);
        let before = client.rpc("mcp.catalog", json!({})).await;
        let view = client
            .rpc("mcp.configuration", json!({"source":"user","id":id}))
            .await;
        assert_ok(&view);
        assert_eq!(
            view["result"]["revision"], before["result"]["userRevision"],
            "the edit read carries the source revision the catalog reports"
        );
        let inspection = client
            .rpc("mcp.inspect", json!({"source":"user","id":id}))
            .await;
        assert_ok(&inspection);
        for secret in [
            "private-argument-fixture",
            "preserved-fixture-value",
            "old-fixture-value",
            "deleted-fixture-value",
            "bearer-fixture-value",
            "url-user-fixture",
            "url-password-fixture",
            "url-path-fixture",
            "url-query-fixture",
            "url-fragment-fixture",
        ] {
            assert!(
                !view.to_string().contains(secret),
                "configuration must not expose stored values"
            );
            assert!(
                !inspection.to_string().contains(secret),
                "inspection must not expose stored values"
            );
        }
        let request = json!({"source":"user","id":id,"expectedVersion":before["result"]["userRevision"],"patch":patch});
        assert_ok(&client.rpc("mcp.update", request.clone()).await);
        assert_eq!(
            client.rpc("mcp.update", request).await["error"]["data"]["appError"],
            "mcp_settings_conflict"
        );
        let db = rusqlite::Connection::open(native.join("dimcode.sqlite")).unwrap();
        let document: String = db
            .query_row(
                "SELECT document FROM settings_documents WHERE source='user/mcp'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let document: Value = serde_json::from_str(&document).unwrap();
        let stored = &document["mcpServers"][id];
        let values = if id == "command" {
            &stored["env"]
        } else {
            &stored["headers"]
        };
        assert_eq!(values["KEEP"], "preserved-fixture-value");
        assert_eq!(values["REPLACE"], "replacement-fixture-value");
        assert!(values.get("DELETE").is_none());
        assert_eq!(stored["enabled"], false);
        assert_eq!(stored["extension"], "preserved");
        if id == "command" {
            assert_eq!(stored["args"], configuration["args"]);
            assert_eq!(view["result"]["configuration"]["argsCount"], 1);
            assert!(view["result"]["configuration"].get("args").is_none());
        }
        if id == "remote" {
            assert_eq!(stored["auth"], configuration["auth"]);
            assert_eq!(
                stored["url"], configuration["url"],
                "an omitted URL must preserve hidden credentials"
            );
            assert_eq!(
                view["result"]["configuration"]["endpoint"],
                "https://example.invalid"
            );
            assert!(view["result"]["configuration"].get("url").is_none());
            let current = client.rpc("mcp.catalog", json!({})).await;
            assert_ok(&client.rpc("mcp.update", json!({"source":"user","id":id,"expectedVersion":current["result"]["userRevision"],"patch":{"bearer":{"action":"set","token":"replacement-bearer-fixture"}}})).await);
            let view = client
                .rpc("mcp.configuration", json!({"source":"user","id":id}))
                .await;
            assert_ok(&view);
            assert_eq!(view["result"]["configuration"]["authConfigured"], true);
            assert!(!view.to_string().contains("replacement-bearer-fixture"));
            let document: String = db
                .query_row(
                    "SELECT document FROM settings_documents WHERE source='user/mcp'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let document: Value = serde_json::from_str(&document).unwrap();
            assert_eq!(
                document["mcpServers"][id]["auth"]["token"],
                "replacement-bearer-fixture"
            );
        }
        let after = client.rpc("mcp.catalog", json!({})).await;
        let invalid_patch = if id == "command" {
            json!({"url":"https://example.invalid"})
        } else {
            json!({"command":"invalid-transport-field"})
        };
        let invalid = client.rpc("mcp.update", json!({"source":"user","id":id,"expectedVersion":after["result"]["userRevision"],"patch":invalid_patch})).await;
        assert_eq!(invalid["error"]["data"]["appError"], "invalid_params");
        assert_ok(&client.rpc("mcp.update", json!({"source":"user","id":id,"expectedVersion":after["result"]["userRevision"],"patch":removed})).await);
        let view = client
            .rpc("mcp.configuration", json!({"source":"user","id":id}))
            .await;
        assert_ok(&view);
        let names = if id == "command" {
            &view["result"]["configuration"]["envNames"]
        } else {
            &view["result"]["configuration"]["headerNames"]
        };
        assert_eq!(names, &json!([]));
        if id == "command" {
            assert_eq!(view["result"]["configuration"]["argsCount"], 0);
        }
        if id == "remote" {
            assert_eq!(view["result"]["configuration"]["authConfigured"], false);
            let document: String = db
                .query_row(
                    "SELECT document FROM settings_documents WHERE source='user/mcp'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let document: Value = serde_json::from_str(&document).unwrap();
            assert!(document["mcpServers"][id].get("auth").is_none());
        }
    }
    let current = client.rpc("mcp.catalog", json!({})).await;
    let refused = client
        .rpc(
            "mcp.update",
            json!({"source":"plugin","id":"kit:docs","expectedVersion":current["result"]["userRevision"],"patch":{"command":"other"}}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "invalid_params");
    assert_eq!(
        refused["error"]["message"],
        "plugin MCP entries change through their plugin"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_entry_edits_keep_the_declared_configuration() {
    let home = std::path::PathBuf::from("/tmp").join(format!("dp-{}", uuid::Uuid::new_v4()));
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = std::fs::canonicalize(&project).unwrap();
    std::fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{"local":{"command":"never-start-this-fixture","args":["--flag"]}}})
            .to_string(),
    )
    .unwrap();
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
    let catalog = client.rpc("mcp.catalog", json!({"cwd":project})).await;
    assert_ok(&catalog);
    assert_eq!(catalog["result"]["servers"][0]["id"], "local");
    assert_eq!(catalog["result"]["servers"][0]["enabled"], false);
    let before = client
        .rpc(
            "mcp.configuration",
            json!({"source":"project","id":"local","cwd":project}),
        )
        .await;
    assert_ok(&before);
    assert_eq!(
        before["result"]["revision"], catalog["result"]["projectRevision"],
        "the edit read and the catalog report the same source revision"
    );
    assert_eq!(before["result"]["enabled"], false);
    assert!(
        before["result"]["configuration"]["cwd"].is_null(),
        "the edit read keeps the declared configuration"
    );
    let inspection = client
        .rpc(
            "mcp.inspect",
            json!({"source":"project","id":"local","cwd":project}),
        )
        .await;
    assert_ok(&inspection);
    assert_eq!(
        inspection["result"]["configuration"]["cwd"],
        project.to_str().unwrap(),
        "a running entry resolves the workspace working directory"
    );

    assert_ok(&client.rpc("mcp.update", json!({"source":"project","id":"local","cwd":project,"expectedVersion":before["result"]["revision"],"patch":{"command":"never-start-this-fixture-2"}})).await);
    let stored: Value =
        serde_json::from_slice(&std::fs::read(project.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        stored["mcpServers"]["local"],
        json!({"command":"never-start-this-fixture-2","args":["--flag"]}),
        "an edit writes the patched field and leaves the declared entry otherwise unchanged"
    );
    assert!(
        stored["mcpServers"]["local"].get("cwd").is_none(),
        "an edit must not write the resolved workspace path into the project file"
    );
    assert!(
        stored["mcpServers"]["local"].get("enabled").is_none(),
        "an edit must not write the user record's enablement into the project file"
    );
    assert!(
        !stored.to_string().contains(project.to_str().unwrap()),
        "the project file names no workspace path"
    );
    let after = client
        .rpc(
            "mcp.configuration",
            json!({"source":"project","id":"local","cwd":project}),
        )
        .await;
    assert_ok(&after);
    assert_ne!(after["result"]["revision"], before["result"]["revision"]);
    assert_eq!(
        client.rpc("mcp.update", json!({"source":"project","id":"local","cwd":project,"expectedVersion":before["result"]["revision"],"patch":{"command":"stale"}})).await["error"]["data"]["appError"],
        "mcp_settings_conflict"
    );

    assert_ok(&client.rpc("mcp.set_enabled", json!({"source":"project","id":"local","cwd":project,"expectedVersion":after["result"]["revision"],"enabled":true})).await);
    let view = client
        .rpc(
            "mcp.configuration",
            json!({"source":"project","id":"local","cwd":project}),
        )
        .await;
    assert_ok(&view);
    assert_eq!(view["result"]["enabled"], true);
    assert!(
        view["result"]["configuration"].get("enabled").is_none(),
        "enablement is reported beside the configuration, never inside it"
    );
    let stored: Value =
        serde_json::from_slice(&std::fs::read(project.join(".mcp.json")).unwrap()).unwrap();
    assert!(stored["mcpServers"]["local"].get("enabled").is_none());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_connection_tests_are_disposable_preserve_drafts_and_respect_project_authority() {
    let home = std::path::PathBuf::from("/tmp").join(format!("dm-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("project")).unwrap();
    let marker = home.join("probe");
    let script = r#"import sys, json, pathlib
marker = pathlib.Path(sys.argv[1])
marker.with_suffix('.started').write_text('started')
try:
    for line in sys.stdin:
        message = json.loads(line)
        if 'id' not in message:
            continue
        if message['method'] == 'initialize':
            result = {'protocolVersion': message['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'probe', 'version': '1'}}
        elif len(sys.argv) > 2 and sys.argv[2] == 'bad-schema':
            result = {'tools': [{'name': 'broken', 'description': 'Broken', 'inputSchema': {'type': 'object', 'properties': {'x': {'type': ['string', 'file']}}}}]}
        elif len(sys.argv) > 2:
            print(json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'error': {'code': -32603, 'message': 'discovery-secret-failure'}}), flush=True)
            continue
        else:
            result = {'tools': [{'name': 'inspect', 'description': 'Inspect a file', 'inputSchema': {'type': 'object'}}]}
        print(json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'result': result}), flush=True)
finally:
    marker.with_suffix('.closed').write_text('closed')
"#;
    let configuration = json!({"command":"python3","args":["-u","-c",script,marker],"enabled":false,"env":{"TOKEN":"probe-secret-value"}});
    std::fs::write(
        home.join("project/.mcp.json"),
        json!({"mcpServers":{"probe":configuration}}).to_string(),
    )
    .unwrap();
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
    for method in ["mcp.inspect", "mcp.test", "mcp.auth.cancel"] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            remote.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let before = client.rpc("mcp.catalog", json!({})).await;
    let tested = client
        .rpc(
            "mcp.test",
            json!({"source":"user","id":"unsaved","configuration":configuration}),
        )
        .await;
    assert_ok(&tested);
    assert_eq!(tested["result"]["status"], "connected", "{tested}");
    assert!(
        tested["result"]["latencyMs"].as_u64().is_some(),
        "a probe reports the latency of its attempt: {tested}"
    );
    assert_eq!(tested["result"]["tools"][0]["name"], "inspect");
    assert_eq!(
        tested["result"]["tools"][0]["description"],
        "Inspect a file"
    );
    assert!(
        marker.with_extension("closed").exists(),
        "test returns only after its child has closed"
    );
    assert!(!tested.to_string().contains("probe-secret-value"));
    let after = client.rpc("mcp.catalog", json!({})).await;
    assert_eq!(
        before["result"]["userRevision"],
        after["result"]["userRevision"]
    );
    assert!(
        !after["result"]["servers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == "unsaved")
    );
    assert_ok(&client.rpc("mcp.save", json!({"source":"user","id":"probe","expectedVersion":after["result"]["userRevision"],"configuration":configuration})).await);
    let before = client.rpc("mcp.catalog", json!({})).await;
    let inspected = client
        .rpc("mcp.inspect", json!({"source":"user","id":"probe"}))
        .await;
    assert_ok(&inspected);
    assert_eq!(inspected["result"]["enabled"], false);
    assert_eq!(inspected["result"]["connections"], json!([]));
    assert!(!inspected.to_string().contains("probe-secret-value"));
    let failed = client.rpc("mcp.test", json!({"source":"user","id":"probe","patch":{"command":"missing-probe-secret-command"}})).await;
    assert_ok(&failed);
    assert_eq!(failed["result"]["status"], "failed", "{failed}");
    assert_eq!(failed["result"]["reason"], "unavailable", "{failed}");
    assert!(failed["result"]["tools"].is_null(), "{failed}");
    assert!(
        failed["result"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "a failed probe explains itself: {failed}"
    );
    assert!(
        failed["result"]["latencyMs"].as_u64().is_some(),
        "a failed probe reports its latency: {failed}"
    );
    assert!(!failed.to_string().contains("missing-probe-secret-command"));
    std::fs::remove_file(marker.with_extension("closed")).unwrap();
    let failed = client.rpc("mcp.test", json!({"source":"user","id":"probe","patch":{"args":["-u","-c",script,marker,"fail-discovery"]}})).await;
    assert_ok(&failed);
    assert_eq!(failed["result"]["status"], "failed", "{failed}");
    assert!(failed["result"]["tools"].is_null(), "{failed}");
    assert!(!failed.to_string().contains("discovery-secret-failure"));
    assert!(
        marker.with_extension("closed").exists(),
        "failed discovery closes its spawned child before returning"
    );
    std::fs::remove_file(marker.with_extension("closed")).unwrap();
    let refused = client.rpc("mcp.test", json!({"source":"user","id":"probe","patch":{"args":["-u","-c",script,marker,"bad-schema"]}})).await;
    assert_ok(&refused);
    assert_eq!(refused["result"]["status"], "failed", "{refused}");
    assert_eq!(refused["result"]["reason"], "response", "{refused}");
    assert_eq!(
        refused["result"]["message"],
        "invalid MCP server response: tool broken: inputSchema.properties.x.type[1]: unknown variant `file`, expected one of `object`, `array`, `string`, `number`, `integer`, `boolean`, `null`"
    );
    assert!(
        marker.with_extension("closed").exists(),
        "a refused tool list closes its spawned child before returning"
    );
    let after = client.rpc("mcp.catalog", json!({})).await;
    assert_eq!(
        before["result"]["userRevision"],
        after["result"]["userRevision"]
    );
    let tested = client.rpc("mcp.test", json!({"source":"user","id":"probe","patch":{"env":{"TOKEN":"replacement-probe-secret"}}})).await;
    assert_ok(&tested);
    assert!(!tested.to_string().contains("replacement-probe-secret"));
    std::fs::remove_file(marker.with_extension("started")).unwrap();
    let project = json!({"source":"project","id":"probe","cwd":home.join("project")});
    let refused = client.rpc("mcp.test", project.clone()).await;
    assert_eq!(refused["error"]["data"]["appError"], "mcp_unavailable");
    assert!(
        !marker.with_extension("started").exists(),
        "disabled project commands cannot run as tests"
    );
    let refused = client.rpc("mcp.test", json!({"source":"project","id":"new","cwd":home.join("project"),"configuration":configuration})).await;
    assert_eq!(refused["error"]["data"]["appError"], "invalid_params");
    for (request, expected) in [
        (
            json!({"source":"user","id":" ","configuration":configuration}),
            "invalid_params",
        ),
        (json!({"source":"user","id":"absent"}), "mcp_unknown_server"),
        (
            json!({"source":"plugin","id":"kit:docs","patch":{"command":"other"}}),
            "invalid_params",
        ),
        (
            json!({"source":"user","id":"probe","patch":{"url":"https://example.invalid"}}),
            "invalid_params",
        ),
    ] {
        let refused = client.rpc("mcp.test", request).await;
        assert_eq!(
            refused["error"]["data"]["appError"], expected,
            "a request refused before contact stays an error: {refused}"
        );
    }
    let catalog = client
        .rpc("mcp.catalog", json!({"cwd":home.join("project")}))
        .await;
    assert_ok(&client.rpc("mcp.set_enabled", json!({"source":"project","id":"probe","cwd":home.join("project"),"expectedVersion":catalog["result"]["projectRevision"],"enabled":true})).await);
    assert_ok(&client.rpc("mcp.test", project).await);
    assert!(marker.with_extension("started").exists());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_locates_a_published_submission_without_resubmitting() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, ws, 3).await;
        let created = client.rpc("session.create", create(json!([]))).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();
        let submission = uuid::Uuid::new_v4().to_string();
        let mut dispatched = session.dispatch_watch("main").unwrap();
        client
            .send(
                "session.submit",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":submission,"text":"recover me"
                }),
            )
            .await;
        let admitted = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(event) = session
                    .history("main", None)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|event| {
                        matches!(&event.kind, EventKind::InputMessage(data)
                        if data.submission_id.as_deref() == Some(&submission))
                    })
                {
                    break event;
                }
                dispatched.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        drop(client);
        let mut reconnect = Client::new_version(&server, !ws, 3).await;
        let lookup = json!({"sessionId":sid,"branchId":"main","submissionId":submission});
        let recovered = reconnect.rpc("session.input_lookup", lookup.clone()).await;
        assert_ok(&recovered);
        assert_eq!(recovered["result"]["status"], "accepted");
        assert_eq!(
            recovered["result"]["event"],
            serde_json::to_value(&admitted).unwrap()
        );
        let absent = reconnect
            .rpc(
                "session.input_lookup",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":uuid::Uuid::new_v4().to_string()
                }),
            )
            .await;
        assert_eq!(absent["result"], json!({"status":"unknown"}));
        let inputs = || async {
            session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
                .count()
        };
        let before = inputs().await;
        let resubmitted = reconnect.rpc("session.submit", json!({
            "sessionId":sid,"branchId":"main","submissionId":submission,"text":"another input"
        })).await;
        assert_ok(&resubmitted);
        assert_eq!(
            resubmitted["result"]["event"],
            serde_json::to_value(&admitted).unwrap()
        );
        for _ in 0..2 {
            assert_ok(
                &reconnect
                    .rpc(
                        "session.submit",
                        json!({"sessionId":sid,"branchId":"main","text":"same text"}),
                    )
                    .await,
            );
        }
        assert_eq!(inputs().await, before + 2);
        assert_eq!(
            reconnect.rpc("session.input_lookup", lookup).await["result"]["event"],
            serde_json::to_value(&admitted).unwrap()
        );
        for identity in [json!("invalid"), json!(null), json!(5)] {
            let rejected = reconnect.rpc("session.submit", json!({
                "sessionId":sid,"branchId":"main","submissionId":identity,"text":"invalid identity"
            })).await;
            assert_eq!(rejected["error"]["code"], -32602);
        }
        let mut legacy = Client::new_version(&server, ws, 2).await;
        let rejected = legacy
            .rpc(
                "session.history",
                json!({
                    "sessionId":sid,"branchId":"main","after":0,"limit":1
                }),
            )
            .await;
        assert_eq!(rejected["error"]["code"], -32601);
        let rejected = legacy
            .rpc(
                "session.input_lookup",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":submission
                }),
            )
            .await;
        assert_eq!(rejected["error"]["code"], -32601);
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixed_history_pages_share_live_projection_and_advance_across_filtered_records() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, ws, 3).await;
        let mut params = create(json!([]));
        params["systemPrompt"] = json!("private system prompt");
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();
        let initial = session.sequence("main").unwrap();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({
                        "sessionId":sid,"branchId":"main","text":"first"
                    }),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                .await,
        );
        let first = client
            .rpc(
                "session.history",
                json!({
                    "sessionId":sid,"branchId":"main","after":0,"limit":1
                }),
            )
            .await;
        assert_ok(&first);
        let through = first["result"]["through"].as_u64().unwrap();
        let expected: Vec<_> = client
            .notices
            .iter()
            .filter(|notice| {
                notice["method"] == "session.event"
                    && notice["params"]["branchId"] == "main"
                    && notice["params"]["event"]["sequence"].as_u64().unwrap() <= through
            })
            .map(|notice| notice["params"]["event"].clone())
            .collect();
        let second = client
            .rpc(
                "session.submit",
                json!({
                    "sessionId":sid,"branchId":"main","text":"later"
                }),
            )
            .await;
        assert_ok(&second);
        assert_ok(
            &client
                .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                .await,
        );
        assert!(session.sequence("main").unwrap() > through);
        let mut page = first["result"].clone();
        let mut all = Vec::new();
        let mut filtered_page = false;
        loop {
            assert_eq!(page["through"], through);
            let events = page["events"].as_array().unwrap();
            filtered_page |= events.is_empty();
            all.extend(events.iter().cloned());
            if page["done"] == true {
                break;
            }
            let after = page["nextAfter"].as_u64().unwrap();
            let response = client
                .rpc(
                    "session.history",
                    json!({
                        "sessionId":sid,"branchId":"main","after":after,"limit":1,"through":through
                    }),
                )
                .await;
            assert_ok(&response);
            page = response["result"].clone();
            assert!(page["nextAfter"].as_u64().unwrap() > after);
        }
        assert!(
            filtered_page,
            "filtered pages must advance without visible events"
        );
        assert_eq!(page["nextAfter"], through);
        let live_part: Vec<_> = all
            .iter()
            .filter(|event| event["sequence"].as_u64().unwrap() > initial)
            .cloned()
            .collect();
        assert_eq!(live_part, expected);
        assert!(
            all.windows(2)
                .all(|pair| pair[0]["sequence"].as_u64().unwrap()
                    < pair[1]["sequence"].as_u64().unwrap())
        );
        assert!(
            !serde_json::to_string(&all)
                .unwrap()
                .contains("private system prompt")
        );
        assert!(
            !all.iter()
                .any(|event| event["id"] == second["result"]["event"]["id"])
        );
        let complete = client.rpc("session.history", json!({
            "sessionId":sid,"branchId":"main","after":through,"limit":9007199254740991u64,"through":through
        })).await;
        assert_eq!(
            complete["result"],
            json!({"events":[],"through":through,"nextAfter":through,"done":true})
        );
        for invalid in [
            json!({"after":-1,"limit":1}),
            json!({"after":0.5,"limit":1}),
            json!({"after":9007199254740992u64,"limit":1}),
            json!({"after":0,"limit":0}),
            json!({"after":0,"limit":1.5}),
            json!({"after":0,"limit":9007199254740992u64}),
            json!({"after":0,"limit":1,"through":null}),
            json!({"after":through+1,"limit":1,"through":through}),
            json!({"after":0,"limit":1,"through":session.sequence("main").unwrap()+1}),
        ] {
            let mut params = invalid;
            params["sessionId"] = json!(sid);
            params["branchId"] = json!("main");
            let rejected = client.rpc("session.history", params).await;
            assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
        }
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_product_management_starts_without_configuration_and_isolates_invalid_sources() {
    let home = std::env::temp_dir().join(format!(
        "dim-p-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let home = std::fs::canonicalize(home).unwrap();
    let broken = b"{private-configuration-secret";
    std::fs::write(home.join("mcp.json"), broken).unwrap();
    std::fs::write(home.join("skills.json"), broken).unwrap();
    let native = home.join("rust");
    std::fs::create_dir_all(&native).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = serve(
        AgentBuilder::new(AgentConfig::default()),
        ServerConfig {
            unix_path: Some(native.join("daemon.sock")),
            product_home: Some(home.clone()),
            target_home: Some(native),
            session_execution: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(server.agent().is_none());
    assert!(
        !home.join("auth.json").exists(),
        "startup must not read or generate account state"
    );
    let mut local = Client::new_version(&server, false, 3).await;
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("account.providers"))
    );
    assert!(
        !local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.history"))
    );
    let empty = local.rpc("account.providers", json!({})).await;
    assert_ok(&empty);
    assert_eq!(empty["result"], json!([]));
    assert_eq!(
        local
            .rpc("account.status", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    let managed_empty = json!({"authenticated":false,"recoveryRequired":false,"expiresAt":null,"accountIdPreview":null});
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("account.usage"))
    );
    assert_eq!(
        local
            .rpc("account.usage", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    assert_eq!(
        local.rpc("account.codex.status", json!({})).await["result"],
        managed_empty
    );
    assert_eq!(
        local.rpc("account.grok.status", json!({})).await["result"],
        managed_empty
    );
    let codex = local
        .rpc("account.codex.login.browser.begin", json!({}))
        .await;
    assert_ok(&codex);
    let attempt = codex["result"]["attemptId"].as_str().unwrap();
    assert_eq!(
        local
            .rpc("account.codex.login.status", json!({"attemptId":attempt}))
            .await["result"]["status"],
        "pending"
    );
    assert_ok(
        &local
            .rpc("account.codex.login.cancel", json!({"attemptId":attempt}))
            .await,
    );
    assert_eq!(
        local
            .rpc("account.codex.login.status", json!({"attemptId":attempt}))
            .await["result"]["status"],
        "cancelled"
    );
    assert_eq!(
        local
            .rpc(
                "account.codex.login.browser.complete",
                json!({"attemptId":attempt,"callbackUrl":"https://outside.test/?code=private"})
            )
            .await["error"]["data"]["appError"],
        "login_attempt_closed"
    );
    assert_eq!(
        local
            .rpc(
                "account.grok.login.status",
                json!({"attemptId":uuid::Uuid::new_v4()})
            )
            .await["error"]["data"]["appError"],
        "login_attempt_unknown"
    );
    assert_eq!(
        local
            .rpc("account.recover", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    assert_eq!(
        local
            .rpc("provider.catalog", json!({"providerId":"missing"}))
            .await["result"],
        json!({"catalog":null})
    );
    assert_eq!(
        local.rpc("provider.selection", json!({"cwd":home})).await["result"],
        json!({"selection":null})
    );
    assert_eq!(
        local
            .rpc("provider.selection", json!({"cwd":"relative"}))
            .await["error"]["data"]["appError"],
        "invalid_params"
    );
    assert_eq!(
        local.rpc("session.create", create(json!([]))).await["error"]["data"]["appError"],
        "product_integration_pending"
    );
    assert_eq!(
        local
            .rpc(
                "account.login.status",
                json!({"providerId":"managed-fixture","attemptId":"invalid"})
            )
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(remote.features, json!([]));
    assert_eq!(
        remote
            .rpc(
                "provider.import_legacy",
                json!({"sourcePath":"/unused/legacy.sqlite"})
            )
            .await["error"]["data"]["appError"],
        "local_transport_required"
    );
    for method in [
        "account.status",
        "account.logout",
        "account.usage",
        "account.codex.status",
        "account.codex.logout",
        "account.grok.status",
        "account.grok.logout",
        "provider.refresh",
    ] {
        assert_eq!(
            remote
                .rpc(
                    method,
                    if method.starts_with("account.codex") || method.starts_with("account.grok") {
                        json!({})
                    } else {
                        json!({"providerId":"managed-fixture"})
                    }
                )
                .await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let mut old = Client::new_version(&server, false, 2).await;
    for (method, params) in [
        (
            "provider.import_legacy",
            json!({"sourcePath":"/unused/legacy.sqlite"}),
        ),
        ("account.recover", json!({"providerId":"managed-fixture"})),
        (
            "account.login.browser.begin",
            json!({"providerId":"managed-fixture","directory":"https://issuer.example","redirectUri":"http://localhost:54321/auth/callback"}),
        ),
        (
            "account.login.browser.complete",
            json!({"providerId":"managed-fixture","attemptId":uuid::Uuid::new_v4().to_string(),"callbackUrl":"http://localhost:54321/auth/callback?code=private&state=state"}),
        ),
        (
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":0,"modelIds":[]}),
        ),
        ("account.codex.status", json!({})),
        ("account.codex.recover", json!({})),
        ("account.codex.login.browser.begin", json!({})),
        (
            "account.codex.login.browser.complete",
            json!({"attemptId":uuid::Uuid::new_v4(),"callbackUrl":"http://localhost:1455/auth/callback?code=private&state=state"}),
        ),
        (
            "account.codex.login.status",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        (
            "account.codex.login.cancel",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        ("account.codex.logout", json!({})),
        ("account.grok.status", json!({})),
        ("account.grok.recover", json!({})),
        ("account.grok.login.begin", json!({})),
        (
            "account.grok.login.status",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        (
            "account.grok.login.cancel",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        ("account.grok.logout", json!({})),
    ] {
        assert!(local.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            remote.rpc(method, params.clone()).await["error"]["data"]["appError"],
            "local_transport_required"
        );
        assert_eq!(
            old.rpc(method, params).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    for version in [1, 2, 3] {
        let mut remote = Client::new_version(&server, true, version).await;
        for force in [false, true] {
            assert_eq!(
                remote.rpc("daemon.shutdown", json!({"force":force})).await["error"]["data"]["appError"],
                "local_transport_required"
            );
        }
    }
    assert_eq!(
        old.rpc("account.status", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "unknown_method"
    );
    for method in ["account.codex.status", "account.grok.status"] {
        assert_eq!(
            old.rpc(method, json!({})).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    std::fs::write(home.join("auth.json"), broken).unwrap();
    for method in ["account.codex.status", "account.grok.status"] {
        let invalid = local.rpc(method, json!({})).await;
        assert_eq!(
            invalid["error"]["data"]["appError"],
            "account_invalid_store"
        );
        assert!(!invalid.to_string().contains("private-configuration-secret"));
    }
    assert_ok(&local.rpc("daemon.ping", json!({})).await);
    assert_eq!(
        local.rpc("session.list", json!({})).await["result"],
        json!([])
    );
    for file in ["auth.json", "mcp.json", "skills.json"] {
        assert_eq!(std::fs::read(home.join(file)).unwrap(), broken);
    }
    assert_ok(&local.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_product_operations_use_real_tools_and_survive_rpc_reload() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, tool_registry::ToolSourcesView,
        workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset};
    use dim_app_server::mcp::McpSession;
    use std::os::unix::{ffi::OsStringExt, fs::DirBuilderExt};

    async fn skills_posted(session: &dim_agent_runtime::Session, enabled: bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let Ok(view) = session
                    .service::<dim_agent_capabilities::skills::SkillsBranch>("main")
                    .unwrap()
                    .session()
                    .current()
                else {
                    tokio::task::yield_now().await;
                    continue;
                };
                let current = match &view.list {
                    Some(list) => {
                        format!("<dim-state source=\"dim/skills/list\">{list}</dim-state>")
                    }
                    None => {
                        "<dim-state source=\"dim/skills/list\" cleared=\"true\"></dim-state>".into()
                    }
                };
                let events = session.history("main", None).await.unwrap();
                let delivered = events.iter().rev().find_map(|event| match &event.kind {
                    EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| {
                        let dim_agent_types::InputBlock::Raw { text, .. } = block;
                        text.starts_with("<dim-state source=\"dim/skills/list\"")
                            .then(|| text.clone())
                    }),
                    _ => None,
                });
                if view.skills.contains_key("product-check") == enabled
                    && delivered.as_ref() == Some(&current)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    let home = std::path::PathBuf::from("/tmp").join(format!("dp-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("AGENTS.md"), "Product prompt before refresh.\n").unwrap();
    let skill_dir = work
        .join(".agents/skills")
        .join(std::ffi::OsString::from_vec(if cfg!(target_os = "linux") {
            b"product-\xff".to_vec()
        } else {
            b"product-check".to_vec()
        }));
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: product-check\ndescription: Product fixture\n---\nProduct skill body\n",
    )
    .unwrap();
    std::fs::write(home.join("skills.json"), br#"{"disabledSkillIds":[]}"#).unwrap();
    std::fs::write(home.join("mcp.json"), json!({"mcpServers":{"docs":{"type":"http","url":"https://example.invalid/private-secret","headers":{"Authorization":"private-secret"},"enabled":true}}}).to_string()).unwrap();
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
            session_defaults: SessionConfig {
                tools: Some(
                    [
                        "read", "write", "edit", "glob", "grep", "exec", "wait", "skill",
                    ]
                    .map(String::from)
                    .to_vec(),
                ),
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
    std::fs::write(work.join(".mcp.json"), json!({"mcpServers":{"docs":{"command":"missing-mcp-program","args":["private-secret"],"env":{"TOKEN":"private-secret"},"enabled":false}},"extension":"preserved"}).to_string()).unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    for method in [
        "mcp.catalog",
        "mcp.set_enabled",
        "mcp.disconnect",
        "session.mcp",
        "session.mcp.refresh",
    ] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
    }
    // ADR 003: no host-local approval methods or catalog approval state.
    for (method, params) in [
        (
            "mcp.approve",
            json!({"cwd":work,"id":"docs","approvalToken":"any"}),
        ),
        ("mcp.revoke", json!({"cwd":work,"id":"docs"})),
    ] {
        assert!(!client.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            client.rpc(method, params).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    let catalog = client.rpc("mcp.catalog", json!({"cwd":work})).await;
    assert_ok(&catalog);
    assert_eq!(
        catalog["result"]["servers"],
        json!([{"id":"docs","source":"project","enabled":false,"transport":"stdio"}])
    );
    assert!(!catalog.to_string().contains("private-secret"));
    let revision = catalog["result"]["projectRevision"].clone();
    assert_eq!(client.rpc("mcp.set_enabled", json!({"cwd":work,"id":"docs","source":"project","expectedVersion":"stale","enabled":true})).await["error"]["data"]["appError"], "mcp_settings_conflict");
    assert_eq!(
        client
            .rpc(
                "mcp.set_enabled",
                json!({"id":"docs","source":"project","expectedVersion":revision,"enabled":true})
            )
            .await["error"]["data"]["appError"],
        "invalid_params"
    );
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.exec.stop"))
    );
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(remote.features, json!([]));
    for (method, params) in [
        ("session.create", json!({"cwd":work})),
        ("session.list", json!({})),
        ("skills.catalog", json!({})),
        ("mcp.catalog", json!({})),
        ("session.mcp", json!({"sessionId":"remote"})),
        (
            "session.permission.get",
            json!({"sessionId":"remote","branchId":"main"}),
        ),
    ] {
        assert_eq!(
            remote.rpc(method, params).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let entry = |id: &str, max: u64| {
        let caps = dim_agent_types::CatalogCapabilities {
            reasoning: true,
            ..dim_agent_types::CatalogCapabilities::with_limits(32000, max)
        };
        json!({"modelId":id,"displayName":id,"status":"active","capabilities":caps,
            "metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"],"off":{"kind":"thinking_type","value":"disabled"}},
                "serviceTier":{"supported":true,"options":["priority","default"]}}})
    };
    let fixture_provider = "018ec73f-0000-7000-8000-000000000001";
    let definition = json!({"providerId":"018ec73f-0000-7000-8000-000000000001","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},"models":[entry("alpha",2048),entry("beta",4096)],"enabled":true});
    assert_ok(&client.rpc("provider.create", definition.clone()).await);
    assert_eq!(
        client.rpc("provider.create", definition.clone()).await["error"]["data"]["appError"],
        "provider_conflict"
    );
    assert_eq!(
        client
            .rpc(
                "provider.update",
                json!({"providerId":fixture_provider,"expectedVersion":1,"patch":{}})
            )
            .await["error"]["data"]["appError"],
        "provider_invalid_config"
    );
    let mut invalid_id = definition.clone();
    invalid_id["providerId"] = json!("friendly-name");
    assert_eq!(
        client.rpc("provider.create", invalid_id).await["error"]["data"]["appError"],
        "provider_invalid_config"
    );
    assert!(
        !client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("provider.save"))
    );
    assert_eq!(
        client.rpc("provider.save", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    let uncertain_id = "018ec73f-4d19-7f61-bc79-7130618bb454";
    let mut creator = Client::new_version(&server, false, 3).await;
    let mut uncertain = definition.clone();
    uncertain["providerId"] = json!(uncertain_id);
    uncertain["enabled"] = json!(false);
    let _reply_not_consumed = creator.send("provider.create", uncertain.clone()).await;
    let recovered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = client
                .rpc("provider.management", json!({"providerId":uncertain_id}))
                .await;
            if result.get("result").is_some() {
                break result;
            }
            assert_eq!(result["error"]["data"]["appError"], "provider_not_found");
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(creator);
    assert_eq!(
        recovered["result"]["providers"].as_array().unwrap().len(),
        1
    );
    assert_eq!(recovered["result"]["providers"][0]["version"], 1);
    assert_eq!(
        client.rpc("provider.create", uncertain).await["error"]["data"]["appError"],
        "provider_conflict"
    );
    assert_ok(
        &client
            .rpc(
                "provider.delete",
                json!({"providerId":uncertain_id,"expectedVersion":1}),
            )
            .await,
    );
    let managed = client
        .rpc(
            "provider.management",
            json!({"providerId":fixture_provider}),
        )
        .await;
    assert_ok(&managed);
    assert_eq!(
        managed["result"]["providers"][0]["models"][0]["model"]["modelId"],
        "alpha"
    );
    assert_eq!(
        managed["result"]["providers"][0]["models"][0]["source"],
        "default"
    );
    assert!(!managed.to_string().contains("fixture-access"));
    for (method, params, expected) in [
        (
            "provider.delete",
            json!({"providerId":fixture_provider,"expectedVersion":0}),
            "provider_conflict",
        ),
        (
            "provider.models.edit",
            json!({"providerId":fixture_provider,"expectedVersion":0,"modelId":"alpha","edit":{}}),
            "provider_conflict",
        ),
        (
            "provider.models.reset",
            json!({"providerId":fixture_provider,"expectedVersion":1,"modelId":"alpha"}),
            "provider_not_found",
        ),
        (
            "provider.models.set_enabled",
            json!({"providerId":fixture_provider,"expectedVersion":1,"modelIds":["missing"]}),
            "provider_invalid_config",
        ),
        (
            "provider.select",
            json!({"providerId":"missing-provider","modelId":"alpha"}),
            "provider_not_found",
        ),
    ] {
        let result = client.rpc(method, params).await;
        assert_eq!(result["error"]["code"], -32000, "{method}: {result}");
        assert_eq!(
            result["error"]["data"]["appError"], expected,
            "{method}: {result}"
        );
    }
    let mut second = Client::new_version(&server, false, 3).await;
    let preferred = client.rpc("provider.preferences.update", json!({"providerId":fixture_provider,"expectedVersion":1,"modelId":"alpha","patch":{"reasoningEffort":{"action":"set","value":"high"}}})).await;
    assert_ok(&preferred);
    assert_eq!(preferred["result"]["version"], 2);
    let stale = second.rpc("provider.update", json!({"providerId":fixture_provider,"expectedVersion":1,"patch":{"displayName":"Must not overwrite"}})).await;
    assert_eq!(stale["error"]["data"]["appError"], "provider_conflict");
    let cleared = client.rpc("provider.preferences.update", json!({"providerId":fixture_provider,"expectedVersion":2,"modelId":"alpha","patch":{"reasoningEffort":{"action":"clear"}}})).await;
    assert_ok(&cleared);
    assert_eq!(cleared["result"]["preferences"], json!({}));
    let added = client
        .rpc(
            "provider.models.create",
            json!({"providerId":fixture_provider,"expectedVersion":3,"model":entry("manual",512)}),
        )
        .await;
    assert_ok(&added);
    assert_eq!(added["result"]["version"], 4);
    assert_eq!(
        client
            .rpc(
                "provider.models.reset",
                json!({"providerId":fixture_provider,"expectedVersion":4,"modelId":"manual"})
            )
            .await["error"]["data"]["appError"],
        "provider_unsupported"
    );
    assert_ok(
        &client
            .rpc(
                "provider.models.delete",
                json!({"providerId":fixture_provider,"expectedVersion":4,"modelId":"manual"}),
            )
            .await,
    );
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
    assert_eq!(created["result"]["model"]["modelId"], "alpha");
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let branch = json!({"sessionId":sid,"branchId":"main"});
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    async fn mcp_disabled(client: &mut Client) {
        loop {
            let notice = client.notice("session.mcp.state").await;
            if notice["params"]["servers"][0]["state"] == "disabled" {
                assert!(notice["params"]["servers"][0]["adoptedVersion"].is_null());
                break;
            }
        }
    }
    mcp_disabled(&mut client).await;
    let state = client.rpc("session.mcp", json!({"sessionId":sid})).await;
    assert_ok(&state);
    assert_eq!(state["result"]["servers"][0]["state"], "disabled");
    assert_ok(
        &client
            .rpc("session.mcp.refresh", json!({"sessionId":sid}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "mcp.disconnect",
                json!({"cwd":work,"id":"docs","source":"project"}),
            )
            .await,
    );
    let old_mcp = session.session_service::<McpSession>().unwrap();
    let old_tools = session.session_service::<ToolSourcesView>().unwrap();
    client
        .notices
        .retain(|notice| notice["method"] != "session.mcp.state");
    server.restart("Mcp").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(mcp) = session.session_service::<McpSession>()
                && !Arc::ptr_eq(&mcp, &old_mcp)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Session MCP owner replaced after process reload");
    assert!(Arc::ptr_eq(
        &session.session_service::<ToolSourcesView>().unwrap(),
        &old_tools
    ));
    mcp_disabled(&mut client).await;
    assert_ok(&client.rpc("session.mcp", json!({"sessionId":sid})).await);
    assert_ok(&client.rpc("mcp.set_enabled", json!({"cwd":work,"id":"docs","source":"project","expectedVersion":revision,"enabled":true})).await);
    let enabled = client.rpc("mcp.catalog", json!({"cwd":work})).await;
    assert_eq!(enabled["result"]["servers"][0]["enabled"], true);
    loop {
        let notice = client.notice("session.mcp.state").await;
        if notice["params"]["servers"][0]["state"] == "failed" {
            assert_eq!(notice["params"]["servers"][0]["id"], "docs");
            assert_eq!(notice["params"]["closed"], false);
            break;
        }
    }
    assert_eq!(enabled["result"]["projectRevision"], revision);
    assert_ok(&client.rpc("mcp.set_enabled", json!({"cwd":work,"id":"docs","source":"project","expectedVersion":revision,"enabled":false})).await);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(work.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(saved["extension"], "preserved");
    assert_eq!(saved["mcpServers"]["docs"]["enabled"], false);
    mcp_disabled(&mut client).await;
    std::fs::write(work.join(".mcp.json"), b"{broken-private-secret").unwrap();
    let broken = client.rpc("mcp.catalog", json!({"cwd":work})).await;
    assert!(broken.get("error").is_some());
    assert!(!broken.to_string().contains("private-secret"));
    assert_ok(
        &client
            .rpc("session.mcp.refresh", json!({"sessionId":sid}))
            .await,
    );
    std::fs::write(home.join("skills.json"), b"{broken").unwrap();
    assert_ok(&client.rpc("session.skills", branch.clone()).await);
    assert_ok(&client.rpc("skills.refresh", json!({"cwd":work})).await);
    let skills = client.rpc("skills.catalog", json!({"cwd":work})).await;
    assert_ok(&skills);
    let skill = skills["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "product-check")
        .unwrap();
    if cfg!(target_os = "linux") {
        assert!(skill["filePath"].as_str().unwrap().contains('\u{fffd}'));
    }
    let skill_id = skill["id"].clone();
    assert_ok(
        &client
            .rpc(
                "skills.set_enabled",
                json!({"cwd":work,"id":skill_id,"enabled":false}),
            )
            .await,
    );
    skills_posted(&session, false).await;
    assert_ok(
        &client
            .rpc(
                "skills.set_enabled",
                json!({"cwd":work,"id":skill_id,"enabled":true}),
            )
            .await,
    );
    skills_posted(&session, true).await;
    assert_ok(&client.rpc("session.skills", branch.clone()).await);

    assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":"tool:write {\"path\":\"result.txt\",\"content\":\"product output\\n\"}"})).await);
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    assert_eq!(
        std::fs::read_to_string(work.join("result.txt")).unwrap(),
        "product output\n"
    );
    assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":"tool:exec {\"command\":\"printf started; sleep 60\",\"runInBackground\":true,\"timeoutMs\":-1}"})).await);
    let approval = client.notice("approval.request").await;
    assert_ok(&client.rpc("approval.respond", json!({"sessionId":sid,"branchId":"main","requestId":approval["params"]["requestId"],"decision":"allow"})).await);
    let task = loop {
        let event = client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "started"
        {
            break event["params"]["event"]["data"]["task_id"].clone();
        }
    };
    let ended = client
        .rpc(
            "session.exec.stop",
            json!({"sessionId":sid,"branchId":"main","taskId":task}),
        )
        .await;
    assert_ok(&ended);
    assert_eq!(ended["result"]["status"], "cancelled");
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    let policy = client.rpc("session.permission.get", branch.clone()).await;
    assert_ok(&policy);
    let changed = client.rpc("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":policy["result"]["version"],"policy":{"preset":"read-only","allowedTools":[],"disallowedTools":[]}})).await;
    assert_ok(&changed);
    assert_eq!(client.rpc("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":policy["result"]["version"],"policy":policy["result"]["policy"]})).await["error"]["data"]["appError"], "permission_conflict");
    async fn project_rule(session: &dim_agent_runtime::Session) -> (String, String) {
        let context = session
            .context("main", session.sequence("main").unwrap())
            .await
            .unwrap();
        let system = serde_json::to_string(
            &dim_agent_capabilities::history::context_configuration(&context)
                .unwrap()
                .system,
        )
        .unwrap();
        let rule = context
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    text.starts_with("<dim-rule source=\"dim/instructions/project\"")
                        .then(|| text.clone())
                }),
                _ => None,
            })
            .unwrap();
        (system, rule)
    }
    let (system, rule) = project_rule(&session).await;
    assert!(rule.contains("Product prompt before refresh."));
    assert!(!system.contains("Product prompt before refresh."));
    std::fs::write(work.join("AGENTS.md"), "Product prompt after refresh.\n").unwrap();
    assert_ok(&client.rpc("session.prompt.refresh", branch.clone()).await);
    let (system, rule) = project_rule(&session).await;
    assert!(rule.contains("op=\"replace\"") && rule.contains("Product prompt after refresh."));
    assert!(!system.contains("Product prompt"));
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main","providerId":"018ec73f-0000-7000-8000-000000000001","modelId":"beta"}),
            )
            .await,
    );
    let context = session
        .context("main", session.sequence("main").unwrap())
        .await
        .unwrap();
    assert_eq!(
        dim_agent_capabilities::history::context_configuration(&context)
            .unwrap()
            .settings
            .model_id,
        "beta"
    );

    use dim_agent_capabilities::prompt_state::PromptSnapshot;
    let adopted = || {
        session
            .get::<PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .configuration
            .unwrap()
    };
    let original = adopted();
    for preferences in [
        json!({"reasoningEffort":"high"}),
        json!({"serviceTier":"priority"}),
        json!({"reasoningEffort":"none"}),
        json!({"reasoningEffort":"low"}),
    ] {
        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":sid,"branchId":"main","modelPreferences":preferences}),
                )
                .await,
        );
        assert_eq!(adopted().tools, original.tools);
        if preferences["reasoningEffort"] == "none" {
            assert!(adopted().config.unwrap().reasoning.is_none());
        }
    }
    let selected = adopted();
    let config = selected.config.as_ref().unwrap();
    assert_eq!(config.max_tokens, Some(4096.));
    assert_eq!(
        config.reasoning.as_ref().unwrap().effort.as_deref(),
        Some("low")
    );
    let options = config
        .provider_options
        .as_ref()
        .unwrap()
        .openai_compatible
        .as_ref()
        .unwrap();
    assert_eq!(options.service_tier.as_deref(), Some("priority"));
    assert!(options.thinking.is_none());
    assert_eq!(client.rpc("session.model.update", json!({"sessionId":sid,"branchId":"main","modelPreferences":{"reasoningEffort":"low"}})).await["result"]["outcome"], "unchanged");
    assert_eq!(adopted(), selected);
    for fields in [
        json!({}),
        json!({"modelPreferences":{}}),
        json!({"providerId":"018ec73f-0000-7000-8000-000000000001"}),
        json!({"modelId":"beta"}),
        json!({"providerId":null,"modelId":null}),
        json!({"modelPreferences":{"reasoningEffort":"unsupported"}}),
        json!({"modelPreferences":{"reasoningEffort":null}}),
        json!({"modelPreferences":{"reasoningEffort":{"action":"set"}}}),
        json!({"modelPreferences":{"reasoningEffort":{"action":"clear","value":"high"}}}),
        json!({"modelPreferences":{"reasoningEffort":{"action":"nope"}}}),
    ] {
        let mut request = branch.clone();
        request
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(
            client
                .rpc("session.model.update", request)
                .await
                .get("error")
                .is_some()
        );
        assert_eq!(adopted(), selected);
    }
    let invalid_profile = client.rpc("agent.create", json!({"profile":{"name":"Unavailable model","description":"",
        "systemPrompt":"Must not be adopted.","tools":[],"model":{"providerId":"018ec73f-0000-7000-8000-000000000001","modelId":"missing"}}})).await;
    assert_ok(&invalid_profile);
    assert!(client.rpc("session.agent.update", json!({"sessionId":sid,"branchId":"main","agentId":invalid_profile["result"]["agentId"]})).await.get("error").is_some());
    assert_eq!(adopted(), selected);
    assert!(
        !session
            .get::<PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .system
            .contains("Must not be adopted.")
    );
    let profile = client.rpc("agent.create", json!({"profile":{"name":"Reviewer","description":"F3 fixture",
        "systemPrompt":"Profile review instructions.","tools":[],"model":{"providerId":"018ec73f-0000-7000-8000-000000000001","modelId":"alpha"}}})).await;
    assert_ok(&profile);
    let profile_id = profile["result"]["agentId"].clone();
    let update = json!({"sessionId":sid,"branchId":"main","agentId":profile_id});
    assert_eq!(
        client.rpc("session.agent.update", update.clone()).await["result"]["outcome"],
        "adopted"
    );
    assert_eq!(adopted().model_id, "alpha");
    assert_eq!(adopted().config.as_ref().unwrap().max_tokens, Some(2048.));
    assert_eq!(adopted().tools, original.tools);
    assert_eq!(
        client.rpc("session.permission.get", branch.clone()).await["result"],
        changed["result"]
    );
    assert!(server.agent().unwrap().get_session(&sid).is_some());
    assert_eq!(
        client.rpc("session.agent.update", update).await["result"]["outcome"],
        "unchanged"
    );
    let before = adopted();
    let prompt_only = client
        .rpc(
            "agent.create",
            json!({"profile":{"name":"Prompt only","description":"",
        "systemPrompt":"Prompt-only instructions.","tools":[]}}),
        )
        .await;
    assert_ok(&prompt_only);
    assert_ok(&client.rpc("session.agent.update", json!({"sessionId":sid,"branchId":"main","agentId":prompt_only["result"]["agentId"]})).await);
    assert_eq!(adopted(), before);
    assert_ok(&client.rpc("session.prompt.refresh", branch.clone()).await);
    assert!(
        session
            .get::<PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .system
            .contains("Prompt-only instructions.")
    );
    let empty = client
        .rpc(
            "agent.create",
            json!({"profile":{"name":"Empty","description":"","systemPrompt":"","tools":[]}}),
        )
        .await;
    assert_ok(&empty);
    assert_ok(
        &client
            .rpc(
                "session.agent.update",
                json!({"sessionId":sid,"branchId":"main","agentId":empty["result"]["agentId"]}),
            )
            .await,
    );
    assert!(
        !session
            .get::<PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .entries
            .keys()
            .any(|(source, _)| source == dim_agent_capabilities::prompt_sources::SESSION)
    );
    let before = adopted();
    assert_eq!(
        client
            .rpc(
                "session.agent.update",
                json!({"sessionId":sid,"branchId":"main","agentId":"unknown"})
            )
            .await["error"]["data"]["appError"],
        "agent_unknown"
    );
    assert_eq!(
        client
            .rpc(
                "session.agent.update",
                json!({"sessionId":sid,"branchId":"other","agentId":profile_id})
            )
            .await["error"]["code"],
        -32602
    );
    assert_eq!(adopted(), before);

    let lock = rusqlite::Connection::open(native.join("dimcode.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let pending = client.send("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":changed["result"]["version"],"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}})).await;
    let publication = session.publication("main").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !publication.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), server.restart("Rpc"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        client.response(pending).await["error"]["data"]["appError"],
        "operation_interrupted"
    );
    lock.execute_batch("COMMIT").unwrap();
    let mut dispatched = session.dispatch_watch("main").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session
                .get::<dim_agent_capabilities::permission::PermissionState>("main")
                .unwrap()
                .unwrap()
                .policy
                .preset
                == PermissionPreset::WorkspaceWrite
            {
                break;
            }
            dispatched.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.rpc("session.permission.get", branch).await["result"]["policy"]["preset"],
        "workspace-write"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The public `context/configured` view carries the effective preferences of
/// the adopted configuration: what a client can draw and send back, the same
/// after the Session is reopened, and absent when the configuration carries
/// none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_configured_view_reads_back_the_adopted_model_preferences() {
    let home = std::env::temp_dir().join(format!("dim-effort-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let provider = "018ec73f-0000-7000-8000-000000000001";
    let entry = |id: &str, reasoning: bool| {
        let capabilities = dim_agent_types::CatalogCapabilities {
            reasoning,
            ..dim_agent_types::CatalogCapabilities::with_limits(32_000, 4_096)
        };
        let mut entry =
            json!({"modelId":id,"displayName":id,"status":"active","capabilities":capabilities});
        if reasoning {
            entry["metadata"] = json!({
                "reasoning":{"supported":true,"effortOptions":["low","high"],
                    "off":{"kind":"thinking_type","value":"disabled"}},
                "serviceTier":{"supported":true,"options":["priority","default"]}
            });
        }
        entry
    };
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":provider,"displayName":"Fixture",
                    "connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1",
                        "auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},
                    "models":[entry("reasoning",true),entry("plain",false)],"enabled":true}),
            )
            .await,
    );

    /// The `context/configured` payload of the newest public history read.
    async fn configured(client: &mut Client, session_id: &str) -> Value {
        let attached = client
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        let through = attached["result"]["sequence"].as_u64().unwrap();
        let history = client
            .rpc(
                "session.history",
                json!({"sessionId":session_id,"branchId":"main","after":0,
                    "limit":9007199254740991u64,"through":through}),
            )
            .await;
        history["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|event| event["type"] == "context" && event["name"] == "configured")
            .expect("the branch adopted a configuration")["data"]
            .clone()
    }

    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"provider":provider,"modelId":"reasoning"}}),
        )
        .await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();

    // The first adoption through the public flow: the explicit target starts
    // from the catalog defaults, and nothing names a preference of its own.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "providerId":provider,"modelId":"reasoning"}),
            )
            .await,
    );
    let data = configured(&mut client, &session_id).await;
    assert_eq!(data["model_id"], "reasoning");
    assert!(data.get("model_preferences").is_none(), "{data}");

    // `/effort high` reports the value the configuration will send.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "modelPreferences":{"reasoningEffort":"high"}}),
            )
            .await,
    );
    assert_eq!(
        configured(&mut client, &session_id).await["model_preferences"],
        json!({"reasoningEffort":"high"})
    );

    // `/fast on` keeps the effort and adds the tier.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "modelPreferences":{"serviceTier":"priority"}}),
            )
            .await,
    );
    let adopted = json!({"reasoningEffort":"high","serviceTier":"priority"});
    assert_eq!(
        configured(&mut client, &session_id).await["model_preferences"],
        adopted
    );

    // A Session reopened on another connection reads the same values.
    let mut reopened = Client::new_version(&server, false, 3).await;
    assert_eq!(
        configured(&mut reopened, &session_id).await["model_preferences"],
        adopted
    );

    // The attach reply names the adopted model and values, not the creation
    // configuration.
    let attached = client
        .rpc("session.attach", json!({"sessionId":session_id}))
        .await;
    assert_eq!(attached["result"]["model"]["modelId"], "reasoning");
    assert_eq!(
        attached["result"]["modelPreferences"], adopted,
        "{attached}"
    );

    // Clearing one field keeps the other.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "modelPreferences":{"reasoningEffort":{"action":"clear"}}}),
            )
            .await,
    );
    let tier_only = json!({"serviceTier":"priority"});
    assert_eq!(
        configured(&mut client, &session_id).await["model_preferences"],
        tier_only
    );
    let attached = client
        .rpc("session.attach", json!({"sessionId":session_id}))
        .await;
    assert_eq!(
        attached["result"]["modelPreferences"], tier_only,
        "{attached}"
    );

    // Repeating the clear changes nothing and publishes no new configured fact.
    let revision = configured(&mut client, &session_id).await["revision"].clone();
    let repeated = client
        .rpc(
            "session.model.update",
            json!({"sessionId":session_id,"branchId":"main",
                "modelPreferences":{"reasoningEffort":{"action":"clear"}}}),
        )
        .await;
    assert_eq!(repeated["result"]["outcome"], "unchanged", "{repeated}");
    let data = configured(&mut client, &session_id).await;
    assert_eq!(data["revision"], revision);
    assert_eq!(data["model_preferences"], tier_only);

    // A set beside a clear in one request applies both.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "modelPreferences":{"reasoningEffort":"low",
                        "serviceTier":{"action":"clear"}}}),
            )
            .await,
    );
    assert_eq!(
        configured(&mut client, &session_id).await["model_preferences"],
        json!({"reasoningEffort":"low"})
    );

    // Clearing the other field leaves no value, and the attach reply agrees.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "modelPreferences":{"reasoningEffort":{"action":"clear"}}}),
            )
            .await,
    );
    let data = configured(&mut client, &session_id).await;
    assert!(data.get("model_preferences").is_none(), "{data}");
    let attached = client
        .rpc("session.attach", json!({"sessionId":session_id}))
        .await;
    assert!(
        attached["result"].get("modelPreferences").is_none(),
        "{attached}"
    );
    assert_eq!(attached["result"]["model"]["modelId"], "reasoning");

    // A model whose configuration carries no preference shows no field.
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main",
                    "providerId":provider,"modelId":"plain"}),
            )
            .await,
    );
    let data = configured(&mut client, &session_id).await;
    assert_eq!(data["model_id"], "plain");
    assert!(data.get("model_preferences").is_none(), "{data}");
    let attached = client
        .rpc("session.attach", json!({"sessionId":session_id}))
        .await;
    assert_eq!(
        attached["result"]["model"]["modelId"], "plain",
        "{attached}"
    );
    assert!(
        attached["result"].get("modelPreferences").is_none(),
        "{attached}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The creation and attach replies name the preferences the Session's adopted
/// configuration sends, so a client can label a Session that has not run yet;
/// a configuration with no preference replies without the field.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_creation_replies_name_the_adopted_model_preferences() {
    let home = std::env::temp_dir().join(format!("dim-effort-create-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let provider = "018ec73f-0000-7000-8000-000000000002";
    let entry = json!({"modelId":"reasoning","displayName":"reasoning","status":"active",
        "capabilities":dim_agent_types::CatalogCapabilities {
            reasoning: true,
            ..dim_agent_types::CatalogCapabilities::with_limits(32_000, 4_096)
        },
        "metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"],
            "off":{"kind":"thinking_type","value":"disabled"}}}});
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":provider,"displayName":"Fixture",
                    "connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1",
                        "auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},
                    "models":[entry],"enabled":true}),
            )
            .await,
    );
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"provider":provider,"modelId":"reasoning"},
                "modelPreferences":{"reasoningEffort":"high"}}),
        )
        .await;
    assert_ok(&created);
    let adopted = json!({"reasoningEffort":"high"});
    assert_eq!(created["result"]["modelPreferences"], adopted, "{created}");
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let attached = client
        .rpc("session.attach", json!({"sessionId":session_id}))
        .await;
    assert_ok(&attached);
    assert_eq!(
        attached["result"]["modelPreferences"], adopted,
        "{attached}"
    );

    // The same live Session lists without the field: only the create and
    // attach replies name the preferences.
    let listed = client.rpc("session.list", json!({})).await;
    assert_ok(&listed);
    let item = listed["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["sessionId"] == session_id.as_str())
        .expect("the Session is listed");
    assert!(item.get("modelPreferences").is_none(), "{listed}");

    let plain = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"provider":provider,"modelId":"reasoning"}}),
        )
        .await;
    assert_ok(&plain);
    assert!(plain["result"].get("modelPreferences").is_none(), "{plain}");
    let plain_id = plain["result"]["sessionId"].as_str().unwrap().to_owned();
    let attached = client
        .rpc("session.attach", json!({"sessionId":plain_id}))
        .await;
    assert_ok(&attached);
    assert!(
        attached["result"].get("modelPreferences").is_none(),
        "{attached}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Each Session's ModelProvider progress cell. The writer lands separately;
/// the test sets the cell as ModelProvider would.
struct ProgressCapture {
    cells: ProgressCells,
    cell: rutis::host::Key<dim_agent_runtime::StateCell<dim_agent_runtime::ModelProgress>>,
    identity: rutis::host::Key<dim_agent_runtime::SessionIdentity>,
}
impl ProgressCapture {
    /// The branch's progress cell is a state cell, keyed by type there.
    fn new(names: &dim_agent_runtime::Names<'_>, cells: ProgressCells) -> rutis::Result<Self> {
        Ok(Self {
            cells,
            cell: names.state_cell()?,
            identity: names.key()?,
        })
    }
}
type ProgressCells = Arc<
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            Arc<dim_agent_runtime::StateCell<dim_agent_runtime::ModelProgress>>,
        >,
    >,
>;
impl rutis::host::DynPlugin for ProgressCapture {
    fn name(&self) -> &str {
        "product-test.progress-capture"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.cell.erase(), self.identity.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let cell = ctx.require_at(&self.cell)?;
            let identity = ctx.require_at(&self.identity)?;
            let session = identity.session_id.clone();
            self.cells.lock().unwrap().insert(session, cell);
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// `session.model.progress` carries each branch's current snapshot to v3
/// subscribers: on attach, on change (coalesced) and again on re-attach.
/// It is not a Timeline event and v2 subscribers never receive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_progress_reaches_v3_subscribers_without_persisting() {
    use dim_agent_runtime::{ModelProgress, ToolProgress};
    let cells = ProgressCells::default();
    let captured = cells.clone();
    let (server, agent) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        builder.require_feature(
            Feature::new("product-test.progress-capture")
                .branch("product-test.progress-capture", move |names| {
                    Ok(Box::new(ProgressCapture::new(names, captured.clone())?))
                }),
        );
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.model.progress"))
    );
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&sid).unwrap();
    let head = session.sequence("main").unwrap();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    let empty = json!({"sessionId":sid,"branchId":"main","turnId":null,"requestId":null,
        "text":"","textOffset":0,"thinking":"","thinkingOffset":0,"tools":[]});
    assert_eq!(
        client.notice("session.model.progress").await["params"],
        empty
    );

    let cell = cells.lock().unwrap().get(&sid).unwrap().clone();
    let progress = |text: &str| ModelProgress {
        turn_id: Some("turn-1".into()),
        request_id: Some("request-1".into()),
        text_part_id: Some("text-part-1".into()),
        thinking_part_id: Some("thinking-part-1".into()),
        text: text.into(),
        text_offset: 3,
        thinking: "weighing".into(),
        thinking_offset: 5,
        tools: vec![ToolProgress {
            call_id: "call-1".into(),
            name: "echo".into(),
            arguments: "{\"text\":\"he".into(),
        }],
    };
    let updates = 200;
    for count in 1..=updates {
        cell.set(progress(&"x".repeat(count)));
    }
    let mut notices = 0;
    let last = loop {
        let notice = client.notice("session.model.progress").await;
        notices += 1;
        if notice["params"]["text"] == "x".repeat(updates) {
            break notice;
        }
    };
    assert!(notices < updates, "{notices} notices for {updates} changes");
    assert_eq!(
        last["params"],
        json!({"sessionId":sid,"branchId":"main","turnId":"turn-1","requestId":"request-1",
            "textPartId":"text-part-1","thinkingPartId":"thinking-part-1",
            "text":"x".repeat(updates),"textOffset":3,"thinking":"weighing","thinkingOffset":5,
            "tools":[{"callId":"call-1","name":"echo","arguments":"{\"text\":\"he"}]})
    );

    let mut other = Client::new_version(&server, true, 3).await;
    assert_ok(&other.rpc("session.attach", json!({"sessionId":sid})).await);
    assert_eq!(
        other.notice("session.model.progress").await["params"],
        last["params"]
    );
    cell.set(ModelProgress::default());
    assert_eq!(
        client.notice("session.model.progress").await["params"],
        empty
    );
    assert_eq!(
        other.notice("session.model.progress").await["params"],
        empty
    );
    assert_eq!(session.sequence("main").unwrap(), head);

    let mut legacy = Client::new_version(&server, false, 2).await;
    let created = legacy.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let legacy_sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &legacy
            .rpc("session.attach", json!({"sessionId":legacy_sid}))
            .await,
    );
    let legacy_cell = cells.lock().unwrap().get(&legacy_sid).unwrap().clone();
    legacy_cell.set(progress("unseen"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    legacy.rpc("daemon.ping", json!({})).await;
    assert!(
        legacy
            .notices
            .iter()
            .all(|notice| notice["method"] != "session.model.progress"),
        "{:?}",
        legacy.notices
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Provides the Session `PrepareContextHost` from a child plugin; restarting
/// it reloads ContextPreparation and so ModelProvider.
struct ContextHostOwner {
    host: rutis::host::Key<dim_agent_capabilities::host::PrepareContextHost>,
    views: Arc<std::sync::Mutex<Vec<rutis::host::FiberView>>>,
}
impl Plugin for ContextHostOwner {
    type Deps = ();
    fn name(&self) -> &str {
        "ContextHostOwner"
    }
    async fn apply(&self, ctx: &rutis::Ctx, (): Self::Deps) -> rutis::Result {
        let view =
            dim_agent_runtime::mount_active(ctx, ContextHostPlugin(self.host.clone())).await?;
        self.views.lock().unwrap().push(view);
        Ok(())
    }
}
struct ContextHostPlugin(rutis::host::Key<dim_agent_capabilities::host::PrepareContextHost>);
impl Plugin for ContextHostPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ContextHost"
    }
    async fn apply(&self, ctx: &rutis::Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide_at(
            self.0.clone(),
            Arc::new(dim_agent_capabilities::host::PrepareContextHost(Arc::new(
                dim_agent_capabilities::host::IdentityContext,
            ))),
        )?;
        Ok(())
    }
}

/// A ModelProvider reload replaces its progress cell; the subscription follows the new cell
/// without re-attaching. The activation listener is registered before any progress cell is read,
/// so a reload cannot be missed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_progress_follows_a_reloaded_provider() {
    use dim_agent_capabilities::{
        adapters::{DefaultPrepareModelPlugin, EchoModel},
        history::HistoryRenderer,
        host::{CompactionStrategyHost, ContextModelHost, ModelHost},
    };
    let cells = ProgressCells::default();
    let captured = cells.clone();
    let views = Arc::new(std::sync::Mutex::new(vec![]));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    let owners = views.clone();
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        )
        .require_feature(Feature::new("ContextHostOwner").session(
            "ContextHostOwner",
            move |names| {
                Ok(Box::new(ContextHostOwner {
                    host: names.key()?,
                    views: owners.clone(),
                }))
            },
        ));
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder.require_feature(
        Feature::new("product-test.progress-capture")
            .branch("product-test.progress-capture", move |names| {
                Ok(Box::new(ProgressCapture::new(names, captured.clone())?))
            }),
    );
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-progress-reload-{}/s", uuid::Uuid::new_v4())),
            ),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    client.notice("session.model.progress").await;
    let before = cells.lock().unwrap().get(&sid).unwrap().clone();
    let view = views.lock().unwrap().last().cloned().unwrap();
    view.restart().await.unwrap();
    let after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cell = cells.lock().unwrap().get(&sid).unwrap().clone();
            if !Arc::ptr_eq(&cell, &before) {
                return cell;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ModelProvider reloaded");
    after.set(dim_agent_runtime::ModelProgress {
        text: "after reload".into(),
        ..Default::default()
    });
    let notice = client.notice("session.model.progress").await;
    assert_eq!(notice["params"]["text"], "after reload");
    server.shutdown();
    server.join().await.unwrap();
}
