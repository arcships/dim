//! The persisted compaction window end to end (#2235 PR 1b), against the App
//! Server binary the way the host assembles it: the model stacks read the
//! `compaction` settings source on every preparation, so a write compacts a
//! governed session's next turns; a session whose startup adopted an explicit
//! `--autocompact` window keeps it; and the model-config profile reads the
//! same source.

use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::process::Stdio;

const SUMMARIZER: &str = "You summarize a coding session";

/// An OpenAI-compatible provider: the summarizer gets a fixed summary, every
/// other request a short answer.
#[derive(Default)]
struct Provider {
    requests: std::sync::Mutex<Vec<Value>>,
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
    let text = if body.to_string().contains(SUMMARIZER) {
        "SUMMARY OF EARLIER TURNS"
    } else {
        "ok"
    };
    provider.requests.lock().unwrap().push(body);
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"fixture","choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
    )
}

async fn provider() -> (Arc<Provider>, std::net::SocketAddr) {
    let provider = Arc::new(Provider::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(completion))
        .with_state(provider.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (provider, address)
}

struct Server {
    child: tokio::process::Child,
    socket: std::path::PathBuf,
}

/// The product binary on a private home, the compaction cell wired by the
/// host process the way `dim-app-server` serves it.
async fn launch_binary(directory: &std::path::Path) -> Server {
    let socket = directory.join("ipc/server.sock");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
        .args([
            "--product-home",
            directory.join("home").to_str().unwrap(),
            "--unix",
            socket.to_str().unwrap(),
            "--no-ws",
            "--home",
            directory.join("state").to_str().unwrap(),
        ])
        .env("HOME", directory)
        .current_dir(directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // The pipe must drain: a full stderr blocks the child.
    let errors = child.stderr.take().unwrap();
    tokio::spawn(async move {
        let mut lines = BufReader::new(errors).lines();
        while let Ok(Some(_)) = lines.next_line().await {}
    });
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "server startup failed");
    Server { child, socket }
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
    let result = client
        .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
        .await;
    assert_eq!(result["result"]["protocolVersion"], 3);
    client
}

fn directory(name: &str) -> std::path::PathBuf {
    let directory = std::path::PathBuf::from("/tmp")
        .join(format!("dim-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn startup(base: &str, window: Value) -> Value {
    json!({"connection":{"baseUrl":base,"apiFormat":"openai-compatible","apiKey":"startup-credential"},
        "contextWindow":512000,"maxOutputTokens":4096,"autoCompactWindow":window})
}

/// The startup input a `--model`/`--effort` launch carries: no compaction
/// window, so the session is governed by the persisted setting (#2235 PR 1b
/// B2: the exemption must not key on `metadata.startup.is_some()`).
fn startup_without_window(base: &str) -> Value {
    let mut input = startup(base, Value::Null);
    input.as_object_mut().unwrap().remove("autoCompactWindow");
    input
}

/// `tokens` estimated tokens of user content, by the fixture's four bytes a
/// token.
fn big(index: usize, tokens: f64) -> String {
    format!("turn-{index} {}", "x".repeat((tokens * 4.) as usize))
}

async fn turn(client: &mut Client, sid: &str, text: &str) {
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

async fn history(client: &mut Client, sid: &str) -> Vec<Value> {
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

fn compactions(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|event| is(event, "context/compaction") && event["data"]["status"] == "completed")
        .count()
}

async fn create(client: &mut Client, directory: &std::path::Path, startup: Value) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":directory,"model":{"modelId":"startup-fixture"},"startup":startup}),
        )
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    sid
}

async fn write_compaction(client: &mut Client, document: Value) {
    let read = client
        .rpc(
            "settings.read",
            json!({"source":"compaction","scope":"user"}),
        )
        .await;
    assert_ok(&read);
    let revision = read["result"]["revision"].clone();
    let updated = client
        .rpc(
            "settings.update",
            json!({"source":"compaction","scope":"user","expectedRevision":revision,"document":document}),
        )
        .await;
    assert_ok(&updated);
}

/// Feeds `size`-token turns until one compacts, returning how many were fed.
async fn feed_until_compacted(
    client: &mut Client,
    sid: &str,
    from: usize,
    size: f64,
    limit: usize,
) -> usize {
    let mut fed = from;
    loop {
        fed += 1;
        turn(client, sid, &big(fed, size)).await;
        if compactions(&history(client, sid).await) == 1 {
            return fed;
        }
        assert!(fed < limit, "no compaction after the persisted write");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_persisted_window_compacts_the_next_turns_and_explicit_windows_exempt_their_sessions() {
    tokio::time::timeout(Duration::from_secs(240), async {
        let (mock, address) = provider().await;
        let base = format!("http://{address}/v1");
        let directory = directory("compaction-settings-transport");
        let home = directory.join("home");
        std::fs::create_dir_all(&home).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut server = launch_binary(&directory).await;
        let mut client = connect(&server).await;

        // Turn sizing follows the measured values: a tenth of the room the
        // physical threshold leaves over the fixed request, so the governed
        // session stays physical-safe for the turns it takes the shaped 15%
        // threshold to fill.
        let plain = create(&mut client, &directory, startup_without_window(&base)).await;
        // A fixed 8k-token turn: the physical threshold of the 512k window is
        // nowhere near, while the shaped 15% window's threshold is crossed
        // after the fixed overhead plus a couple of them.
        let size = 8_000.;
        for index in 0..3 {
            turn(&mut client, &plain, &big(index, size)).await;
        }
        assert_eq!(
            compactions(&history(&mut client, &plain).await),
            0,
            "the physical budget does not compact three 8k turns"
        );
        write_compaction(&mut client, json!({"default": "15%"})).await;
        let fired_at = feed_until_compacted(&mut client, &plain, 3, size, 15).await;
        assert!(
            mock.summaries() >= 1,
            "the compaction ran the summarizer"
        );

        // Exact startup windows exempt their sessions: fed the same turns
        // past the input the governed session compacted at, they stay under
        // their own shaping — explicit tokens (the "launched auto, resumed
        // with 128k" adoption shape: an integer window), an explicit
        // percentage, and the explicit auto.
        for (window, name) in [
            (json!(128_000u64), "tokens"),
            (json!("90%"), "percent"),
        ] {
            let exempt = create(&mut client, &directory, startup(&base, window.clone())).await;
            for index in 0..=fired_at + 1 {
                turn(&mut client, &exempt, &big(index, size)).await;
            }
            assert_eq!(
                compactions(&history(&mut client, &exempt).await),
                0,
                "{name}: an explicit startup window exempts the session from the persisted 15%, \
                 whose shaped threshold this input crosses"
            );
        }

        // Launched explicit-auto, resumed with a 128k window: both adoptions
        // are explicit, so the session never reads the persisted document.
        let exempt = create(&mut client, &directory, startup(&base, json!("auto"))).await;
        let mut fed = 0;
        for _ in 0..fired_at.div_ceil(2) {
            fed += 1;
            turn(&mut client, &exempt, &big(fed, size)).await;
        }
        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":exempt,"branchId":"main","startup":startup(&base, json!(128_000u64))}),
                )
                .await,
        );
        while fed <= fired_at + 1 {
            fed += 1;
            turn(&mut client, &exempt, &big(fed, size)).await;
        }
        assert_eq!(
            compactions(&history(&mut client, &exempt).await),
            0,
            "auto-then-128k: both explicit adoptions exempt the session from the persisted 15%, \
             whose shaped threshold this input crosses"
        );
        server.child.kill().await.unwrap();
    })
    .await
    .unwrap();
}

/// The model-config profile reads the same source through its own resolver
/// (#2235 PR 1b): a window for the catalog model compacts the session's next
/// turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_model_config_profile_compacts_on_the_persisted_window() {
    tokio::time::timeout(Duration::from_secs(240), async {
        let (mock, address) = provider().await;
        let directory = directory("compaction-settings-configured");
        let models_path = directory.join("models.json");
        std::fs::write(
            &models_path,
            json!({
                "defaultModel": {"providerId": "local", "modelId": "small"},
                "providers": [{"id": "local", "kind": "openai-compatible", "baseUrl": format!("http://{address}/v1"),
                    "models": [{"id": "small", "contextWindow": 512000, "maxOutputTokens": 4096,
                        "config": {"maxTokens": 4096}}]}]
            })
            .to_string(),
        )
        .unwrap();
        let socket = directory.join("ipc/server.sock");
        let mut spawned = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
            .args([
                "--unix",
                socket.to_str().unwrap(),
                "--no-ws",
                "--home",
                directory.join("state").to_str().unwrap(),
                "--model-config",
                models_path.to_str().unwrap(),
            ])
            .env("HOME", &directory)
            .current_dir(&directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // The pipe must drain: a full stderr blocks the child.
        let errors = spawned.stderr.take().unwrap();
        tokio::spawn(async move {
            let mut lines = BufReader::new(errors).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
        let mut output = BufReader::new(spawned.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).await.unwrap();
        assert!(!line.is_empty(), "server startup failed");
        let mut server = Server {
            child: spawned,
            socket,
        };
        let mut client = connect(&server).await;

        let created = client
            .rpc("session.create", json!({"cwd":directory}))
            .await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        let size = 8_000.;
        for index in 0..3 {
            turn(&mut client, &sid, &big(index, size)).await;
        }
        assert_eq!(
            compactions(&history(&mut client, &sid).await),
            0,
            "the catalog model's physical budget does not compact three 8k turns"
        );
        write_compaction(&mut client, json!({"models": {"small": "15%"}})).await;
        feed_until_compacted(&mut client, &sid, 3, size, 15).await;
        assert!(mock.summaries() >= 1, "the compaction ran the summarizer");
        server.child.kill().await.unwrap();
    })
    .await
    .unwrap();
}
