//! Web through actual C4, Settings, Permission and ToolExecutor boundaries.
use super::*;
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

fn rows() -> SessionTypeScriptConfig {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let row: Value =
        serde_json::from_str(include_str!("../../../../runtime/plugins/web/web.json")).unwrap();
    SessionTypeScriptConfig {
        process_rows: Vec::new(),
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: root.join("runtime/package.json"),
        rows: vec![row],
    }
}

async fn configure(client: &mut Client, document: Value) {
    let read = client
        .rpc("settings.read", json!({"source":"web","scope":"user"}))
        .await;
    assert_ok(&read);
    assert_ok(
        &client
            .rpc(
                "settings.update",
                json!({"source":"web","scope":"user",
        "expectedRevision":read["result"]["revision"],"document":document}),
            )
            .await,
    );
}

async fn create_web(client: &mut Client, preset: &str, interactive: bool) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),
        "tools":["web_search","web_fetch"],"interactive":interactive,
        "permissionPolicy":{"preset":preset,"allowedTools":[],"disallowedTools":[]}}),
        )
        .await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

async fn result(session: &dim_agent_runtime::Session) -> ToolResult {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| {
            if let EventKind::ToolResult(batch) = &event.kind {
                batch.results.first().map(|entry| entry.result.clone())
            } else {
                None
            }
        })
        .expect("tool result")
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        dim_agent_types::ToolContent::Text(text) => text,
        _ => panic!("Web returns text"),
    }
}

async fn submit(client: &mut Client, id: &str, name: &str, arguments: Value) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main",
        "text":format!("tool:{name} {arguments}")}),
            )
            .await,
    );
}

async fn idle(client: &mut Client, id: &str) {
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_web_settings_permission_cancellation_and_restore() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let request_count = requests.clone();
    let (slow, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let (closed, mut disconnected) = tokio::sync::mpsc::unbounded_channel();
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let http = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = stopping.cancelled() => break,
                Some(done) = tasks.join_next() => { done.unwrap(); },
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let (count, slow, closed) = (request_count.clone(), slow.clone(), closed.clone());
                    tasks.spawn(async move {
                        let mut header = Vec::new();
                        let mut chunk = [0; 4096];
                        while !header.windows(4).any(|part| part == b"\r\n\r\n") {
                            let n = socket.read(&mut chunk).await.unwrap();
                            if n == 0 { return; }
                            header.extend_from_slice(&chunk[..n]);
                            assert!(header.len() < 16 * 1024);
                        }
                        let request = String::from_utf8(header).unwrap();
                        assert!(request.starts_with("GET "), "{request}");
                        count.fetch_add(1, Ordering::SeqCst);
                        if request.starts_with("GET /slow ") {
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\npending").await.unwrap();
                            slow.send(()).unwrap();
                            assert_eq!(socket.read(&mut chunk).await.unwrap(), 0);
                            closed.send(()).unwrap();
                            return;
                        }
                        let (media, body) = if request.starts_with("GET /search?") {
                            assert!(request.to_lowercase().contains("x-subscription-token: fixture-web-key\r\n"));
                            ("application/json", r#"{"type":"search","web":{"results":[{"title":"fixture","url":"https://example.com/","description":"fixture-web-key snippet"}]}}"#)
                        } else {
                            ("text/html", "<title>Page</title><main><h1>Web flow</h1><a href='/next'>next</a></main>")
                        };
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        socket.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    });
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dw-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        session_typescript: Some(rows()),
        ..Default::default()
    };
    let mut saved = None;
    for restoring in [false, true] {
        let (server, _model_requests, _) = super::session_typescript::start(config.clone()).await;
        let mut client = Client::new_version(&server, false, 3).await;
        if !restoring {
            assert_ok(&client.rpc("provider.save", json!({"provider":{
                "providerId":"web-fixture","displayName":"Fixture","driverKind":"openai-compatible",
                "baseUrl":"http://localhost/v1","auth":{"type":"anonymous"},"enabled":true,
                "models":[{"modelId":"fixture","displayName":"fixture","status":"active",
                    "capabilities":dim_agent_types::ModelCapabilities::default()}]
            }})).await);
            assert_ok(
                &client
                    .rpc(
                        "provider.select",
                        json!({"providerId":"web-fixture","modelId":"fixture"}),
                    )
                    .await,
            );
        }
        let id = if restoring {
            let id: String = saved.clone().unwrap();
            let before = requests.load(Ordering::SeqCst);
            assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before,
                "restore must not repeat network work"
            );
            id
        } else {
            let id = create_web(&mut client, "full-access", false).await;
            let session = server.agent().unwrap().get_session(&id).unwrap();
            submit(&mut client, &id, "web_search", json!({"query":"initial"})).await;
            idle(&mut client, &id).await;
            assert!(text(&result(&session).await).contains("search_unavailable"));
            configure(&mut client, json!({"allowPrivateHosts":["127.0.0.1"],
                "searchService":{"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-web-key"}})).await;
            submit(
                &mut client,
                &id,
                "web_search",
                json!({"query":"configured"}),
            )
            .await;
            idle(&mut client, &id).await;
            let searched = result(&session).await;
            assert_ne!(searched.is_error, Some(true), "{searched:?}");
            assert!(text(&searched).contains("https://example.com/"));
            assert!(
                !serde_json::to_string(&searched)
                    .unwrap()
                    .contains("fixture-web-key")
            );
            configure(&mut client, json!({"allowPrivateHosts":["127.0.0.1"]})).await;
            let before = requests.load(Ordering::SeqCst);
            submit(&mut client, &id, "web_search", json!({"query":"removed"})).await;
            idle(&mut client, &id).await;
            assert!(text(&result(&session).await).contains("search_unavailable"));
            assert_eq!(requests.load(Ordering::SeqCst), before);

            let denied = create_web(&mut client, "read-only", true).await;
            let before = requests.load(Ordering::SeqCst);
            submit(&mut client, &denied, "web_fetch", json!({"url":base})).await;
            idle(&mut client, &denied).await;
            assert_eq!(
                result(&server.agent().unwrap().get_session(&denied).unwrap())
                    .await
                    .is_error,
                Some(true)
            );
            assert_eq!(requests.load(Ordering::SeqCst), before);

            let asking = create_web(&mut client, "workspace-write", true).await;
            assert_ok(
                &client
                    .rpc("session.attach", json!({"sessionId":asking}))
                    .await,
            );
            submit(&mut client, &asking, "web_fetch", json!({"url":base})).await;
            let request = client.notice("approval.request").await;
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before,
                "approval precedes I/O"
            );
            assert_ok(
                &client
                    .rpc(
                        "approval.respond",
                        json!({"sessionId":asking,"branchId":"main",
                "requestId":request["params"]["requestId"],"decision":"allow"}),
                    )
                    .await,
            );
            idle(&mut client, &asking).await;
            assert!(
                text(&result(&server.agent().unwrap().get_session(&asking).unwrap()).await)
                    .contains("# Web flow")
            );
            let cancelling = create_web(&mut client, "workspace-write", true).await;
            assert_ok(
                &client
                    .rpc("session.attach", json!({"sessionId":cancelling}))
                    .await,
            );
            let before = requests.load(Ordering::SeqCst);
            submit(&mut client, &cancelling, "web_fetch", json!({"url":base})).await;
            client.notice("approval.request").await;
            assert_ok(
                &client
                    .rpc(
                        "session.stop",
                        json!({"sessionId":cancelling,"branchId":"main"}),
                    )
                    .await,
            );
            idle(&mut client, &cancelling).await;
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before,
                "cancelled approval must not start I/O"
            );
            let cancelled = server.agent().unwrap().get_session(&cancelling).unwrap();
            assert_eq!(
                cancelled
                    .local("main", ..)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
                    .count(),
                1
            );
            id
        };
        let session = server.agent().unwrap().get_session(&id).unwrap();
        let before = requests.load(Ordering::SeqCst);
        submit(&mut client, &id, "web_fetch", json!({"url":base})).await;
        idle(&mut client, &id).await;
        assert!(text(&result(&session).await).contains("# Web flow"));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            before + 1,
            "restored cache starts empty"
        );
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url":base,"start_char":1000}),
        )
        .await;
        idle(&mut client, &id).await;
        assert_eq!(
            requests.load(Ordering::SeqCst),
            before + 1,
            "pagination uses the Session cache"
        );
        let results_before = session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
            .count();
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url":format!("{base}/slow")}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), entered.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ok(
            &client
                .rpc("session.stop", json!({"sessionId":id,"branchId":"main"}))
                .await,
        );
        tokio::time::timeout(Duration::from_secs(5), disconnected.recv())
            .await
            .unwrap()
            .unwrap();
        idle(&mut client, &id).await;
        let results_after = session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
            .count();
        assert_eq!(
            results_after,
            results_before + 1,
            "one terminal tool result on interrupt"
        );
        saved = Some(id);
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
    stop.cancel();
    http.await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
