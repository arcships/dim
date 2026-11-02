//! ContextCompaction acceptance against the product profile: the App Server
//! binary with a product home and a mock HTTP provider with a small window.
use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::{process::Stdio, sync::atomic::AtomicBool};

const SUMMARIZER: &str = "You summarize a coding session";

/// An OpenAI-compatible provider: the summarizer gets a fixed summary and
/// can be held open; every other request gets a short answer.
#[derive(Default)]
struct Provider {
    requests: std::sync::Mutex<Vec<Value>>,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Provider {
    fn summaries(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body.to_string().contains(SUMMARIZER))
            .count()
    }
}
async fn completion(State(provider): State<Arc<Provider>>, Json(body): Json<Value>) -> String {
    let summarizer = body.to_string().contains(SUMMARIZER);
    provider.requests.lock().unwrap().push(body);
    if summarizer && provider.hold.swap(false, Ordering::SeqCst) {
        provider.entered.notify_one();
        provider.release.notified().await;
    }
    let text = if summarizer {
        "SUMMARY OF EARLIER TURNS"
    } else {
        "ok"
    };
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"fixture","choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
    )
}

struct Server {
    child: tokio::process::Child,
    socket: std::path::PathBuf,
}
/// Runs the server on a socket below `directory`, with `profile` as its
/// model profile.
async fn launch(directory: &std::path::Path, profile: &[&str]) -> Server {
    let socket = directory.join("ipc/server.sock");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
        .args(profile)
        .args([
            "--unix",
            socket.to_str().unwrap(),
            "--no-ws",
            "--home",
            directory.join("state").to_str().unwrap(),
        ])
        .env("HOME", directory)
        .current_dir(directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "server startup failed");
    Server { child, socket }
}
async fn launch_binary(directory: &std::path::Path) -> Server {
    launch(
        directory,
        &["--product-home", directory.join("home").to_str().unwrap()],
    )
    .await
}
async fn launch_configured(directory: &std::path::Path, configuration: &std::path::Path) -> Server {
    launch(
        directory,
        &["--model-config", configuration.to_str().unwrap()],
    )
    .await
}
async fn connect(server: &Server) -> Client {
    let mut client = Client {
        transport: Transport::Unix(BufReader::new(
            UnixStream::connect(&server.socket).await.unwrap(),
        )),
        id: 0,
        notices: vec![],
        instance: Value::Null,
        features: Value::Null,
    };
    assert_ok(
        &client
            .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await,
    );
    client
}

async fn history(client: &mut Client, sid: &Value) -> Vec<Value> {
    let mut events = Vec::new();
    let mut after = 0;
    loop {
        let page = client
            .rpc(
                "session.history",
                json!({"sessionId":sid,"branchId":"main","after":after,"limit":1000}),
            )
            .await;
        assert_ok(&page);
        events.extend(page["result"]["events"].as_array().unwrap().iter().cloned());
        after = page["result"]["nextAfter"].as_u64().unwrap();
        if page["result"]["done"] == true {
            return events;
        }
    }
}
fn is(event: &Value, kind: &str) -> bool {
    format!(
        "{}/{}",
        event["type"].as_str().unwrap(),
        event["name"].as_str().unwrap()
    ) == kind
}
async fn turn(client: &mut Client, sid: &Value, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":text}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
}
/// A turn of `tokens` estimated tokens (four bytes each).
fn big(index: usize, tokens: f64) -> String {
    format!("turn-{index} {}", "x".repeat((tokens * 4.) as usize))
}

/// Each start has exactly one result with its `start` and ids; no result
/// names a start that does not exist (acceptance 8).
fn assert_paired(events: &[Value]) {
    for start in events
        .iter()
        .filter(|event| is(event, "context/compaction-start"))
    {
        let results: Vec<_> = events
            .iter()
            .filter(|event| {
                is(event, "context/compaction") && event["data"]["start"] == start["sequence"]
            })
            .collect();
        assert_eq!(results.len(), 1, "{start}");
        for field in ["turn_id", "request_id", "command", "through"] {
            assert_eq!(results[0]["data"][field], start["data"][field], "{field}");
        }
    }
    for result in events
        .iter()
        .filter(|event| is(event, "context/compaction"))
    {
        if let Some(start) = result["data"]["start"].as_u64() {
            assert!(
                events.iter().any(
                    |event| event["sequence"] == start && is(event, "context/compaction-start")
                )
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn product_compaction_runs_automatically_on_request_and_settles_after_a_crash() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let provider = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(provider.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = std::path::PathBuf::from("/tmp")
            .join(format!("dim-compaction-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(directory.join("home")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            directory.join("home"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut server = launch_binary(&directory).await;
        let mut client = connect(&server).await;
        let capabilities = dim_agent_types::CatalogCapabilities::with_limits(4_000, 200);
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000004","displayName":"Window","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{endpoint}/v1"),"auth":{"type":"anonymous"}},"models":[{"modelId":"small","displayName":"small","status":"active","capabilities":capabilities}],"enabled":true})).await);
        assert_ok(
            &client
                .rpc("provider.select", json!({"providerId":"018ec73f-0000-7000-8000-000000000004","modelId":"small"}))
                .await,
        );
        let created = client
            .rpc("session.create", json!({"cwd":directory,"tools":[]}))
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].clone();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        let usage = client
            .rpc("session.context.usage", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_ok(&usage);
        assert_eq!(usage["result"]["contextWindow"], 4000.0);
        assert_eq!(usage["result"]["outputTokens"], 200.0);
        assert_eq!(usage["result"]["thresholdTokens"], 3800.0 - 0.2 * 3800.0);
        assert_eq!(usage["result"]["baselineSource"], "full_estimate");
        let threshold = usage["result"]["thresholdTokens"].as_f64().unwrap();
        let overhead = usage["result"]["estimatedInputTokens"].as_f64().unwrap();
        assert!(overhead > 0.);
        // One turn stays below the threshold; two cross it.
        let size = (threshold - overhead) / 2.;
        let missing = client
            .rpc("session.context.compact", json!({"sessionId":sid}))
            .await;
        assert_eq!(missing["error"]["code"], -32602);

        let mut index = 0;
        let events = loop {
            turn(&mut client, &sid, &big(index, size)).await;
            index += 1;
            if index == 1 {
                let after = client
                    .rpc("session.context.usage", json!({"sessionId":sid,"branchId":"main"}))
                    .await;
                assert_ok(&after);
                assert!(
                    after["result"]["estimatedInputTokens"].as_f64().unwrap()
                        < after["result"]["thresholdTokens"].as_f64().unwrap(),
                    "one turn already crosses the threshold: {}",
                    after["result"]
                );
            }
            let events = history(&mut client, &sid).await;
            if events.iter().any(|event| is(event, "context/compaction")
                && event["data"]["status"] == "completed")
            {
                break events;
            }
            assert!(index < 10, "no automatic compaction");
        };
        assert_eq!(provider.summaries(), 1);
        let result = events
            .iter()
            .find(|event| is(event, "context/compaction") && event["data"]["status"] == "completed")
            .unwrap();
        let data = &result["data"];
        assert!(data["turn_id"].is_string() && data["request_id"].is_string());
        assert!(data.get("command").is_none());
        assert_eq!(data["strategy"], "summary");
        assert_eq!(data["replacement"].as_array().unwrap().len(), 1);
        let start = events
            .iter()
            .find(|event| event["sequence"] == data["start"])
            .unwrap();
        assert_eq!(start["data"]["trigger"], "threshold");
        assert_eq!(start["data"]["request_id"], data["request_id"]);
        let budget = events
            .iter()
            .position(|event| is(event, "context/budget"))
            .map(|_| ());
        assert!(budget.is_none(), "context/budget stays hidden from clients");
        let starts: Vec<_> = events
            .iter()
            .filter(|event| is(event, "turn/start"))
            .collect();
        let position = |id: &Value| events.iter().position(|event| &event["id"] == id).unwrap();
        let cut = position(&data["retain_from"]);
        // The bound keeps the last turn's tail only (ContextCompaction §5.3);
        // the request assertions below check what the cut kept.
        assert!(position(&starts[starts.len() - 1]["id"]) <= cut);
        let request = provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|body| !body.to_string().contains(SUMMARIZER))
            .cloned()
            .unwrap();
        let sent = request["messages"].to_string();
        assert!(sent.contains("SUMMARY OF EARLIER TURNS"));
        assert!(sent.contains(&format!("turn-{} ", index - 1)));
        assert!(!sent.contains(&format!("turn-{} ", index - 2)));
        assert!(!sent.contains("turn-0 "));

        turn(&mut client, &sid, "small one").await;
        let before = history(&mut client, &sid).await.len();
        let reply = client
            .rpc("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["status"], "completed");
        let events = history(&mut client, &sid).await;
        let manual: Vec<_> = events[before..].iter().collect();
        assert_eq!(manual.len(), 3, "{manual:?}");
        assert!(is(manual[0], "context/compact"));
        assert!(is(manual[1], "context/compaction-start"));
        assert_eq!(manual[1]["data"]["trigger"], "manual");
        assert_eq!(manual[1]["data"]["command"], manual[0]["sequence"]);
        assert!(manual[1]["data"].get("turn_id").is_none());
        assert!(is(manual[2], "context/compaction"));
        assert_eq!(manual[2]["sequence"], reply["result"]["sequence"]);
        assert_eq!(manual[2]["data"]["start"], manual[1]["sequence"]);
        for kind in ["context/compact", "context/compaction-start", "context/compaction"] {
            let live = loop {
                let notice = client.notice("session.event").await;
                if is(&notice["params"]["event"], kind)
                    && notice["params"]["event"]["sequence"].as_u64() >= manual[0]["sequence"].as_u64()
                {
                    break notice;
                }
            };
            assert_eq!(live["params"]["branchId"], "main");
        }

        turn(&mut client, &sid, "small extra").await;
        let before = history(&mut client, &sid).await.len();
        provider.hold.store(true, Ordering::SeqCst);
        let first = client
            .send("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        provider.entered.notified().await;
        let mut repeated = connect(&server).await;
        let second = repeated
            .send("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        provider.release.notify_one();
        let first = client.response(first).await;
        let second = repeated.response(second).await;
        drop(repeated);
        assert_ok(&first);
        assert_eq!(first["result"]["status"], "completed");
        assert_eq!(second["result"], first["result"]);
        let events = history(&mut client, &sid).await;
        let joined: Vec<_> = events[before..].iter().collect();
        assert_eq!(
            joined.iter().filter(|event| is(event, "context/compact")).count(),
            1,
            "{joined:?}"
        );
        assert_eq!(
            joined
                .iter()
                .filter(|event| is(event, "context/compaction-start"))
                .count(),
            1
        );

        turn(&mut client, &sid, "small two").await;
        provider.hold.store(true, Ordering::SeqCst);
        client
            .send("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        provider.entered.notified().await;
        let mut observer = connect(&server).await;
        let events = loop {
            let events = history(&mut observer, &sid).await;
            if is(events.last().unwrap(), "context/compaction-start") {
                break events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let running = events.last().unwrap();
        assert!(is(running, "context/compaction-start"), "{running}");
        assert_eq!(running["data"]["trigger"], "manual");
        let held = running["sequence"].clone();
        let completed_before = events
            .iter()
            .filter(|event| is(event, "context/compaction") && event["data"]["status"] == "completed")
            .count();
        server.child.kill().await.unwrap();
        drop((client, observer));
        server = launch_binary(&directory).await;
        let mut client = connect(&server).await;
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        let events = history(&mut client, &sid).await;
        let settled: Vec<_> = events
            .iter()
            .filter(|event| is(event, "context/compaction") && event["data"]["start"] == held)
            .collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0]["data"]["status"], "cancelled");
        assert_eq!(settled[0]["data"]["reason"], "capability reloading");
        assert_eq!(
            events
                .iter()
                .filter(|event| is(event, "context/compaction")
                    && event["data"]["status"] == "completed")
                .count(),
            completed_before
        );
        let reply = client
            .rpc("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["status"], "completed");

        provider.hold.store(true, Ordering::SeqCst);
        let mut held = None;
        for _ in 0..10 {
            assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":big(index, size)})).await);
            index += 1;
            let idle = client.send("session.idle", json!({"sessionId":sid,"branchId":"main"})).await;
            tokio::select! {
                _ = provider.entered.notified() => { held = Some(()); break; }
                reply = client.response(idle) => assert_ok(&reply),
            }
        }
        assert!(held.is_some(), "no automatic compaction was held");
        let mut observer = connect(&server).await;
        let running = loop {
            let start = history(&mut observer, &sid)
                .await
                .into_iter()
                .rev()
                .find(|event| is(event, "context/compaction-start"))
                .unwrap();
            if start["data"]["trigger"] == "threshold" {
                break start;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        server.child.kill().await.unwrap();
        drop((client, observer));
        server = launch_binary(&directory).await;
        let mut client = connect(&server).await;
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        let events = history(&mut client, &sid).await;
        let settled = events
            .iter()
            .find(|event| is(event, "context/compaction")
                && event["data"]["start"] == running["sequence"])
            .unwrap();
        assert_eq!(settled["data"]["status"], "cancelled");
        assert_eq!(settled["data"]["reason"], "capability reloading");
        assert_eq!(settled["data"]["request_id"], running["data"]["request_id"]);
        assert_paired(&events);
        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// The model-file profile compacts like the product profile: the window comes
/// from the model entry, automatic and requested compaction complete, and
/// images still go out unchanged with no capability snapshot
/// (ContextCompaction §4, §5.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configured_compaction_uses_the_window_declared_in_the_model_file() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let provider = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(provider.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = std::path::PathBuf::from("/tmp")
            .join(format!("dim-configured-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(directory.join("state")).unwrap();
        let configuration = directory.join("models.json");
        std::fs::write(
            &configuration,
            json!({
                "defaultModel": {"providerId": "local", "modelId": "small"},
                "providers": [{"id": "local", "kind": "openai-compatible",
                    "baseUrl": format!("http://{endpoint}/v1"),
                    "models": [{"id": "small", "contextWindow": 8000, "maxOutputTokens": 200,
                        "config": {"maxTokens": 200}}]}]
            })
            .to_string(),
        )
        .unwrap();
        let mut server = launch_configured(&directory, &configuration).await;
        let mut client = connect(&server).await;
        let created = client
            .rpc("session.create", json!({"cwd":directory,"tools":[]}))
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].clone();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);

        let usage = client
            .rpc(
                "session.context.usage",
                json!({"sessionId":sid,"branchId":"main"}),
            )
            .await;
        assert_ok(&usage);
        assert_eq!(usage["result"]["contextWindow"], 8000.0);
        assert_eq!(usage["result"]["outputTokens"], 200.0);
        assert_eq!(usage["result"]["thresholdTokens"], 7800.0 - 0.2 * 7800.0);

        let size = 1200.;
        let mut index = 0;
        let events = loop {
            turn(&mut client, &sid, &big(index, size)).await;
            index += 1;
            let events = history(&mut client, &sid).await;
            if events.iter().any(|event| {
                is(event, "context/compaction") && event["data"]["status"] == "completed"
            }) {
                break events;
            }
            assert!(index < 10, "no automatic compaction");
        };
        assert_eq!(provider.summaries(), 1);
        let result = events
            .iter()
            .find(|event| is(event, "context/compaction") && event["data"]["status"] == "completed")
            .unwrap();
        assert_eq!(result["data"]["strategy"], "summary");
        let start = events
            .iter()
            .find(|event| event["sequence"] == result["data"]["start"])
            .unwrap();
        assert_eq!(start["data"]["trigger"], "threshold");
        assert_paired(&events);

        turn(&mut client, &sid, "small one").await;
        let reply = client
            .rpc(
                "session.context.compact",
                json!({"sessionId":sid,"branchId":"main"}),
            )
            .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["status"], "completed");
        assert_eq!(provider.summaries(), 2);

        let mut images = connect(&server).await;
        let created = images
            .rpc("session.create", json!({"cwd":directory,"tools":[]}))
            .await;
        assert_ok(&created);
        let image_sid = created["result"]["sessionId"].clone();
        assert_ok(
            &images
                .rpc("session.attach", json!({"sessionId":image_sid}))
                .await,
        );
        assert_ok(
            &images
                .rpc(
                    "session.submit",
                    json!({"sessionId":image_sid,"branchId":"main","user":{"content":[
                        {"type": "text", "text": "look"},
                        {"type": "image", "source": {"type": "base64", "mediaType": "image/png",
                            "data": "aGVsbG8="}}
                    ]}}),
                )
                .await,
        );
        assert_ok(
            &images
                .rpc(
                    "session.idle",
                    json!({"sessionId":image_sid,"branchId":"main"}),
                )
                .await,
        );
        let request = provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|body| !body.to_string().contains(SUMMARIZER))
            .cloned()
            .unwrap();
        assert!(request.to_string().contains("aGVsbG8="), "{request}");

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// The conversation keeps its own reasoning while the summarizer's comes from
/// the strategy's factory defaults, and the diagnostics say which value it ran
/// with (ContextCompaction §13.2, §13.5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summary_reasoning_comes_from_factory_configuration() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let provider = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(provider.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = std::path::PathBuf::from("/tmp")
            .join(format!("dim-compaction-{}", uuid::Uuid::new_v4().simple()));
        let home = directory.join("home");
        std::fs::create_dir_all(&home).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut server = launch_binary(&directory).await;
        let mut client = connect(&server).await;
        let capabilities = dim_agent_types::CatalogCapabilities {
            reasoning: true,
            ..dim_agent_types::CatalogCapabilities::with_limits(4_000, 200)
        };
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000005","displayName":"Reasoning","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{endpoint}/v1"),"auth":{"type":"anonymous"}},"models":[{"modelId":"thinking","displayName":"thinking","status":"active","capabilities":capabilities,"metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"],"off":{"kind":"thinking_type","value":"disabled"}}}}],"enabled":true})).await);
        assert_ok(
            &client
                .rpc("provider.select", json!({"providerId":"018ec73f-0000-7000-8000-000000000005","modelId":"thinking"}))
                .await,
        );
        let created = client
            .rpc("session.create", json!({"cwd":directory,"tools":[]}))
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].clone();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":sid,"branchId":"main","modelPreferences":{"reasoningEffort":"high"}}),
                )
                .await,
        );
        turn(&mut client, &sid, &big(0, 600.0)).await;
        turn(&mut client, &sid, &big(1, 600.0)).await;
        let reply = client
            .rpc("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["status"], "completed");
        let (summarizer, conversation) = {
            let requests = provider.requests.lock().unwrap();
            let summarizer = requests
                .iter()
                .rev()
                .find(|body| body.to_string().contains(SUMMARIZER))
                .cloned()
                .unwrap();
            let conversation = requests
                .iter()
                .rev()
                .find(|body| !body.to_string().contains(SUMMARIZER))
                .cloned()
                .unwrap();
            (summarizer, conversation)
        };
        assert_eq!(summarizer["thinking"]["type"], "disabled", "{summarizer}");
        assert!(summarizer.get("reasoning_effort").is_none(), "{summarizer}");
        assert_eq!(conversation["reasoning_effort"], "high", "{conversation}");
        assert!(conversation.get("thinking").is_none(), "{conversation}");
        let events = history(&mut client, &sid).await;
        let completed = events
            .iter()
            .find(|event| is(event, "context/compaction") && event["data"]["status"] == "completed")
            .unwrap();
        assert_eq!(completed["data"]["diagnostics"]["strategy"]["reasoning"], "off");
        assert!(completed["data"]["diagnostics"]["strategy"].get("targetTokens").is_none());

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// A branch whose fixed request alone crosses the threshold has no cut that
/// removes more than the strategy's planned replacement: the automatic
/// threshold trigger records the existing skip with no summarizer request,
/// while a manual `/compact` on the same history still calls the summarizer
/// and reports its own outcome (ContextCompaction §5.2, §5.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_automatic_compaction_skips_a_cut_that_removes_no_more_than_its_replacement() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let provider = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(provider.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = std::path::PathBuf::from("/tmp")
            .join(format!("dim-compaction-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(directory.join("home")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            directory.join("home"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut server = launch_binary(&directory).await;
        let mut client = connect(&server).await;
        // The fixed request, the shell-input guide entry included, crosses the
        // threshold at 0.8 × (window − output) while the window still leaves
        // room for the requested output, so the turn meets no output-budget
        // recovery and the threshold skip is what it measures.
        let (context_window, max_output_tokens) = (2_900, 200);
        let capabilities = dim_agent_types::CatalogCapabilities::with_limits(
            context_window,
            max_output_tokens,
        );
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000006","displayName":"Tight","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{endpoint}/v1"),"auth":{"type":"anonymous"}},"models":[{"modelId":"small","displayName":"small","status":"active","capabilities":capabilities}],"enabled":true})).await);
        assert_ok(
            &client
                .rpc("provider.select", json!({"providerId":"018ec73f-0000-7000-8000-000000000006","modelId":"small"}))
                .await,
        );
        let created = client
            .rpc("session.create", json!({"cwd":directory,"tools":[]}))
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].clone();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        let usage = client
            .rpc("session.context.usage", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_ok(&usage);
        let threshold = usage["result"]["thresholdTokens"].as_f64().unwrap();
        let budget = (context_window - max_output_tokens) as f64;
        // ContextModel::threshold_tokens (context_model.rs:78-83): the trigger
        // leaves 10% of the input budget, at least 16000 tokens and at most
        // 20% of it.
        let reserve = (0.10 * budget).max(16_000.).min(0.20 * budget);
        assert_eq!(threshold, budget - reserve);
        let overhead = usage["result"]["estimatedInputTokens"].as_f64().unwrap();
        assert!(
            overhead > threshold,
            "the fixed request alone crosses the threshold: {overhead}"
        );

        turn(&mut client, &sid, "first").await;
        let events = history(&mut client, &sid).await;
        let results: Vec<_> = events
            .iter()
            .filter(|event| is(event, "context/compaction"))
            .collect();
        // The threshold trigger records the first request's own estimate. It
        // must leave room for the output the model asks for, or the turn would
        // meet provider-output-budget recovery instead of this skip.
        let request = results
            .first()
            .and_then(|event| event["data"]["diagnostics"]["estimatedInputTokens"].as_f64())
            .expect("the threshold compaction records the request estimate");
        assert!(
            request + max_output_tokens as f64 <= context_window as f64,
            "the request leaves no room for the requested output: {request} + {} > {}",
            max_output_tokens, context_window
        );
        assert_eq!(provider.summaries(), 0);
        let starts: Vec<_> = events
            .iter()
            .filter(|event| is(event, "context/compaction-start"))
            .collect();
        assert_eq!(starts.len(), 1, "{events:?}");
        assert_eq!(starts[0]["data"]["trigger"], "threshold");
        assert_eq!(results.len(), 1, "{events:?}");
        assert_eq!(results[0]["data"]["status"], "skipped");
        assert_eq!(results[0]["data"]["reason"], "nothing_to_compact");
        assert_eq!(results[0]["data"]["start"], starts[0]["sequence"]);
        assert_eq!(results[0]["data"]["diagnostics"]["modelCallCount"], 0);
        assert_eq!(results[0]["data"]["diagnostics"]["entriesReplaced"], 0);

        let reply = client
            .rpc("session.context.compact", json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert_eq!(reply["error"]["data"]["appError"], "compaction_failed");
        assert_eq!(
            reply["error"]["message"],
            "Compacted context does not reduce input."
        );
        assert_eq!(provider.summaries(), 1);
        let events = history(&mut client, &sid).await;
        let manual = events
            .iter()
            .rev()
            .find(|event| is(event, "context/compaction"))
            .unwrap();
        assert_eq!(manual["data"]["status"], "failed");
        assert_eq!(manual["data"]["diagnostics"]["trigger"], "manual");
        assert_eq!(manual["data"]["diagnostics"]["modelCallCount"], 1);
        let diagnostics = &manual["data"]["diagnostics"];
        assert!(
            diagnostics["estimatedOutputTokens"].as_f64().unwrap()
                >= diagnostics["estimatedInputTokens"].as_f64().unwrap()
        );
        assert_eq!(diagnostics["entriesReplaced"], 0);
        assert!(manual["data"]["command"].is_u64());
        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}
