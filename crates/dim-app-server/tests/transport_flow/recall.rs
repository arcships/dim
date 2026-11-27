//! Recall in the tool table of the product and the `--models` profile: both
//! assemble `defaults::install_local_execution`, where Recall is installed
//! beside `wait` (`docs/design/plugins/Recall.md` §13, §14).
use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::{path::Path, process::Stdio, sync::Mutex};

/// An OpenAI-compatible provider: answers every request and keeps the bodies
/// the model saw, so the tool table can be read off them.
#[derive(Default)]
struct Provider {
    requests: Mutex<Vec<Value>>,
}

impl Provider {
    fn tools(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap();
        let body = requests.last().expect("the model was called");
        body["tools"]
            .as_array()
            .expect("a declared tool table")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
            .collect()
    }

    /// The `recall` declaration the model last saw.
    fn recall(&self) -> Value {
        let requests = self.requests.lock().unwrap();
        let body = requests.last().expect("the model was called");
        body["tools"]
            .as_array()
            .expect("a declared tool table")
            .iter()
            .find(|tool| tool["function"]["name"] == "recall")
            .map(|tool| tool["function"].clone())
            .expect("recall is declared")
    }
}

async fn completion(State(provider): State<Arc<Provider>>, Json(body): Json<Value>) -> String {
    provider.requests.lock().unwrap().push(body);
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"fixture","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]})
    )
}

struct Server {
    child: tokio::process::Child,
    socket: std::path::PathBuf,
}

/// Launches the binary for `profile`, which selects the product home or a
/// model configuration.
async fn launch(directory: &Path, profile: &[&str]) -> Server {
    let socket = directory.join("ipc/server.sock");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
        .args([
            "--unix",
            socket.to_str().unwrap(),
            "--no-ws",
            "--home",
            directory.join("state").to_str().unwrap(),
        ])
        .args(profile)
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

async fn declares_recall(client: &mut Client, cwd: &Path) {
    let created = client.rpc("session.create", json!({"cwd": cwd})).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].clone();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main","text":"hello"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":"main"}),
            )
            .await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_profiles_declare_recall() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let provider = Arc::new(Provider::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(provider.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let directory = std::path::PathBuf::from("/tmp")
            .join(format!("dim-recall-{}", uuid::Uuid::new_v4().simple()));
        let home = directory.join("home");
        std::fs::create_dir_all(&home).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir_all(directory.join("state")).unwrap();
        let work = directory.join("work");
        std::fs::create_dir_all(&work).unwrap();

        let capabilities = dim_agent_types::CatalogCapabilities::with_limits(200_000, 4_096);
        let mut product = launch(&directory, &["--product-home", home.to_str().unwrap()]).await;
        let mut client = connect(&product).await;
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000004","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{endpoint}/v1"),"auth":{"type":"anonymous"}},"models":[{"modelId":"fixture","displayName":"fixture","status":"active","capabilities":capabilities}],"enabled":true})).await);
        assert_ok(
            &client
                .rpc(
                    "provider.select",
                    json!({"providerId":"018ec73f-0000-7000-8000-000000000004","modelId":"fixture"}),
                )
                .await,
        );
        declares_recall(&mut client, &work).await;
        let tools = provider.tools();
        assert!(tools.iter().any(|tool| tool == "recall"), "{tools:?}");
        assert!(tools.iter().any(|tool| tool == "wait"), "{tools:?}");
        let recall = provider.recall();
        assert!(
            recall["description"]
                .as_str()
                .unwrap()
                .contains("open any line by its id"),
            "{recall}"
        );
        for (field, declared) in dim_agent_capabilities::narrative::query_properties() {
            assert_eq!(
                recall["parameters"]["properties"][&field]["description"],
                declared["description"],
                "recall declares the shared query: {field}"
            );
        }
        assert_ok(&client.rpc("daemon.shutdown", json!({"force":false})).await);
        assert!(product.child.wait().await.unwrap().success());

        let configuration = directory.join("models.json");
        std::fs::write(
            &configuration,
            json!({"defaultModel":{"providerId":"fixture","modelId":"fixture"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":format!("http://{endpoint}/v1"),"models":[{"id":"fixture","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":4096}}]}]}).to_string(),
        )
        .unwrap();
        let mut models = launch(
            &directory,
            &["--model-config", configuration.to_str().unwrap()],
        )
        .await;
        let mut client = connect(&models).await;
        declares_recall(&mut client, &work).await;
        let tools = provider.tools();
        assert!(tools.iter().any(|tool| tool == "recall"), "{tools:?}");
        assert!(tools.iter().any(|tool| tool == "wait"), "{tools:?}");
        assert_ok(&client.rpc("daemon.shutdown", json!({"force":false})).await);
        assert!(models.child.wait().await.unwrap().success());

        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .expect("the recall declaration flow");
}
