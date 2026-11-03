//! #896: an adoption starts at the model's catalog default effort.
//!
//! Each test runs the App Server binary with a product home and a fake
//! Anthropic-compatible endpoint that records the request body, so the adopted
//! configuration, every public read and the wire agree.
use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::{process::Stdio, sync::Mutex as StdMutex};

/// The fake provider: records every request body and streams a short reply.
#[derive(Default)]
struct Capture {
    requests: StdMutex<Vec<Value>>,
}

async fn messages(
    State(capture): State<Arc<Capture>>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    capture.requests.lock().unwrap().push(body);
    let events = [
        (
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_fixture","type":"message","role":"assistant","model":"fixture","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":11,"output_tokens":1}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    let payload: String = events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect();
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        payload,
    )
}

/// The fake OpenAI-compatible provider: records every request body and
/// streams a short reply.
async fn completions(
    State(capture): State<Arc<Capture>>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    capture.requests.lock().unwrap().push(body);
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"id":"fixture","choices":[{"index":0,"delta":{"content":"ok"},
                "finish_reason":"stop"}]})
        ),
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
async fn launch_product(directory: &std::path::Path) -> Server {
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

/// A temporary directory with a product home the server can open.
fn directory(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let directory = std::path::PathBuf::from("/tmp")
        .join(format!("dim-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(directory.join("home")).unwrap();
    std::fs::set_permissions(
        directory.join("home"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    directory
}

/// A model descriptor whose reasoning declaration carries `default` beside
/// its offered efforts, and `off` when the model declares one. `reasoning`
/// is the model's reasoning capability: `false` denies the control the
/// declaration would otherwise describe, and the projection derives the
/// capability from it.
fn model_with_off(
    model_id: &str,
    default: Option<&str>,
    options: &[&str],
    off: Option<Value>,
    reasoning: bool,
) -> Value {
    let capabilities = dim_agent_types::CatalogCapabilities {
        reasoning,
        ..dim_agent_types::CatalogCapabilities::with_limits(32_000, 4_096)
    };
    let mut declaration = json!({"supported":reasoning,"effortOptions":options});
    if let Some(off) = off {
        declaration["off"] = off;
    }
    if let Some(default) = default {
        declaration["effort"] = json!(default);
    }
    json!({"modelId":model_id,"displayName":model_id,"status":"active",
        "capabilities":capabilities,"metadata":{"reasoning":declaration}})
}

/// A model descriptor whose reasoning declaration carries `default` beside
/// its offered efforts.
fn model(model_id: &str, default: Option<&str>, options: &[&str]) -> Value {
    model_with_off(
        model_id,
        default,
        options,
        Some(json!({"kind":"thinking_type","value":"disabled"})),
        true,
    )
}

async fn create_provider(
    client: &mut Client,
    provider: &str,
    driver: &str,
    base_url: &str,
    models: Vec<Value>,
) {
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":provider,"displayName":"Fixture",
                    "connection":{"driverKind":driver,"baseUrl":base_url,
                        "auth":{"type":"anonymous"}},
                    "models":models,"enabled":true}),
            )
            .await,
    );
}

async fn turn(client: &mut Client, session_id: &Value, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session_id,"branchId":"main","text":text}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session_id,"branchId":"main"}),
            )
            .await,
    );
}

/// The `context/configured` payload of the newest public history read.
async fn configured(client: &mut Client, session_id: &Value) -> Value {
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

/// The last request body the fake provider received.
fn last_body(capture: &Capture) -> Value {
    capture
        .requests
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the provider received a request")
}

/// The effort the last request carried, if any.
fn sent_effort(capture: &Capture) -> Option<String> {
    capture
        .requests
        .lock()
        .unwrap()
        .last()
        .and_then(|body| body.get("output_config"))
        .and_then(|output| output.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 1. A model with a declared default carries it in the creation reply, the
/// attach reply and the configured view, and the request sends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_default_reaches_every_read_and_the_request() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("default-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000101";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![model("defaulted", Some("high"), &["low", "high", "max"])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"defaulted"}}),
            )
            .await;
        assert_ok(&created);
        assert_eq!(
            created["result"]["modelPreferences"],
            json!({"reasoningEffort":"high"}),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        let attached = client
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        assert_eq!(
            attached["result"]["modelPreferences"],
            json!({"reasoningEffort":"high"}),
            "{attached}"
        );

        turn(&mut client, &session_id, "hello").await;
        assert_eq!(
            configured(&mut client, &session_id).await["model_preferences"],
            json!({"reasoningEffort":"high"})
        );
        assert_eq!(sent_effort(&capture).as_deref(), Some("high"));

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 2. A model that declares no default sends nothing and reads back nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_with_no_declared_default_carries_none() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("no-default-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000102";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![model("open", None, &["low", "high"])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"open"}}),
            )
            .await;
        assert_ok(&created);
        assert!(
            created["result"].get("modelPreferences").is_none(),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        let attached = client
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        assert!(
            attached["result"].get("modelPreferences").is_none(),
            "{attached}"
        );

        turn(&mut client, &session_id, "hello").await;
        let data = configured(&mut client, &session_id).await;
        assert!(data.get("model_preferences").is_none(), "{data}");
        assert_eq!(sent_effort(&capture), None);

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 3. A default outside the offered efforts, a model whose capabilities deny
/// reasoning, and a protocol that cannot express an effort all adopt
/// successfully with no effort.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inexpressible_default_never_fails_an_adoption() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("inexpressible-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000103";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![
                model("outside", Some("high"), &["low"]),
                model_with_off(
                    "unsupported-reasoning",
                    Some("high"),
                    &["low", "high"],
                    None,
                    false,
                ),
            ],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"outside"}}),
            )
            .await;
        assert_ok(&created);
        assert!(
            created["result"].get("modelPreferences").is_none(),
            "{created}"
        );

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"unsupported-reasoning"}}),
            )
            .await;
        assert_ok(&created);
        assert!(
            created["result"].get("modelPreferences").is_none(),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        let attached = client
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        assert!(
            attached["result"].get("modelPreferences").is_none(),
            "{attached}"
        );
        turn(&mut client, &session_id, "hello").await;
        let data = configured(&mut client, &session_id).await;
        assert!(data.get("model_preferences").is_none(), "{data}");
        assert_eq!(sent_effort(&capture), None);

        // Bedrock rejects every reasoning preference; the declared default is
        // not applied and the adoption still succeeds.
        let bedrock = "018ec73f-0000-7000-8000-000000000104";
        assert_ok(
            &client
                .rpc(
                    "provider.create",
                    json!({"providerId":bedrock,"displayName":"Bedrock fixture",
                        "connection":{"driverKind":"amazon-bedrock",
                            "baseUrl":"https://bedrock-runtime.us-east-1.amazonaws.com/",
                            "auth":{"type":"aws_bedrock"},
                            "credential":{"action":"set_aws","value":{
                                "accessKeyId":"AKIAFIXTURE","secretAccessKey":"fixture-secret"}}},
                        "models":[model("bedrock-model", Some("medium"), &["medium"])],
                        "enabled":true}),
                )
                .await,
        );
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":bedrock,"modelId":"bedrock-model"}}),
            )
            .await;
        assert_ok(&created);
        assert!(
            created["result"].get("modelPreferences").is_none(),
            "{created}"
        );

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 4. An explicit preference wins over the declared default everywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_preference_wins_over_the_declared_default() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("explicit-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000105";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![model("defaulted", Some("high"), &["low", "high"])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"defaulted"},
                    "modelPreferences":{"reasoningEffort":"low"}}),
            )
            .await;
        assert_ok(&created);
        assert_eq!(
            created["result"]["modelPreferences"],
            json!({"reasoningEffort":"low"}),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();

        turn(&mut client, &session_id, "hello").await;
        assert_eq!(
            configured(&mut client, &session_id).await["model_preferences"],
            json!({"reasoningEffort":"low"})
        );
        assert_eq!(sent_effort(&capture).as_deref(), Some("low"));

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 5. A clear removes the Session's value and returns to the declared
/// default, in the reads and on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clear_returns_to_the_declared_default() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("clear-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000106";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![model("defaulted", Some("high"), &["low", "high"])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"defaulted"},
                    "modelPreferences":{"reasoningEffort":"low"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        assert_eq!(sent_effort(&capture).as_deref(), Some("low"));

        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":session_id,"branchId":"main",
                        "modelPreferences":{"reasoningEffort":{"action":"clear"}}}),
                )
                .await,
        );
        assert_eq!(
            configured(&mut client, &session_id).await["model_preferences"],
            json!({"reasoningEffort":"high"})
        );
        let attached = client
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        assert_eq!(
            attached["result"]["modelPreferences"],
            json!({"reasoningEffort":"high"}),
            "{attached}"
        );

        turn(&mut client, &session_id, "again").await;
        assert_eq!(sent_effort(&capture).as_deref(), Some("high"));

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 6. A Session whose stored adopted configuration carries no effort keeps
/// carrying none when another connection reopens it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stored_effortless_configuration_is_unchanged_on_reopen() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("reopen-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000107";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![model("open", None, &["low", "high"])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"open"},
                    "modelPreferences":{"reasoningEffort":"high"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        // A clear on a model that declares no default leaves the stored
        // configuration without an effort.
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

        let mut reopened = connect(&server).await;
        let attached = reopened
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        assert!(
            attached["result"].get("modelPreferences").is_none(),
            "{attached}"
        );
        turn(&mut reopened, &session_id, "hello").await;
        assert_eq!(sent_effort(&capture), None);

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 7. A default `none` adopts the form the model declares: a native `none`
/// reaches the request as the effort, an off declaration as the protocol's
/// off field, and a declaration the protocol cannot express adopts with
/// no effort.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_default_none_needs_an_expressible_off_declaration() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completions))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("none-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000108";
        create_provider(
            &mut client,
            provider,
            "openai-compatible",
            &format!("http://{endpoint}/v1"),
            vec![
                model_with_off("native", Some("none"), &["low", "none"], None, true),
                model("off", Some("none"), &["low"]),
                model_with_off(
                    "unexpressible",
                    Some("none"),
                    &["low"],
                    Some(json!({"kind":"chat_template_kwargs","keys":["thinking"]})),
                    true,
                ),
            ],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"native"}}),
            )
            .await;
        assert_ok(&created);
        assert_eq!(
            created["result"]["modelPreferences"],
            json!({"reasoningEffort":"none"}),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        let wire = last_body(&capture);
        assert_eq!(wire["reasoning_effort"], json!("none"), "{wire}");
        assert!(wire.get("thinking").is_none(), "{wire}");

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"off"}}),
            )
            .await;
        assert_ok(&created);
        assert_eq!(
            created["result"]["modelPreferences"],
            json!({"reasoningEffort":"none"}),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        let wire = last_body(&capture);
        assert_eq!(wire["thinking"], json!({"type":"disabled"}), "{wire}");
        assert!(wire.get("reasoning_effort").is_none(), "{wire}");

        // The declaration the protocol cannot express leaves the adoption
        // without an effort, and it still succeeds.
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"unexpressible"}}),
            )
            .await;
        assert_ok(&created);
        assert!(
            created["result"].get("modelPreferences").is_none(),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        let wire = last_body(&capture);
        assert!(wire.get("reasoning_effort").is_none(), "{wire}");
        assert!(wire.get("thinking").is_none(), "{wire}");

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 8. A model that declares a default service tier adopts it and sends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_default_service_tier_is_applied() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(completions))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("default-tier");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000109";
        let mut tiered = model("tiered", None, &["low"]);
        tiered["metadata"]["serviceTier"] = json!({"supported":true,
            "default":"priority","options":["priority","default"]});
        create_provider(
            &mut client,
            provider,
            "openai-compatible",
            &format!("http://{endpoint}/v1"),
            vec![tiered],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"tiered"}}),
            )
            .await;
        assert_ok(&created);
        assert_eq!(
            created["result"]["modelPreferences"],
            json!({"serviceTier":"priority"}),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        assert_eq!(
            configured(&mut client, &session_id).await["model_preferences"],
            json!({"serviceTier":"priority"})
        );
        let wire = last_body(&capture);
        assert_eq!(wire["service_tier"], json!("priority"), "{wire}");

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// #897: a model-config profile records the protocol it adopts, so the
/// configured effort reaches every read and the request, and a profile with
/// no effort shows nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_configured_profile_shows_the_effort_it_sends() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("configured-effort");
        let configuration = directory.join("model.json");
        std::fs::write(
            &configuration,
            serde_json::to_vec(&json!({
                "defaultModel":{"providerId":"local","modelId":"effortful"},
                "providers":[{"id":"local","kind":"anthropic",
                    "baseUrl":format!("http://{endpoint}"),
                    "models":[
                        {"id":"effortful","contextWindow":32_000,"maxOutputTokens":4_096,
                            "config":{"maxTokens":4096,"reasoning":{"effort":"low"}}},
                        {"id":"plain","contextWindow":32_000,"maxOutputTokens":4_096,
                            "config":{"maxTokens":4096}}
                    ]}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut server = launch_configured(&directory, &configuration).await;
        let mut client = connect(&server).await;

        let rejected = client
            .rpc("session.create", json!({"cwd":directory,"startup":{}}))
            .await;
        assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("product"),
            "{rejected}"
        );
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":"local","modelId":"effortful"}}),
            )
            .await;
        assert_ok(&created);
        assert_eq!(
            created["result"]["modelPreferences"],
            json!({"reasoningEffort":"low"}),
            "{created}"
        );
        let session_id = created["result"]["sessionId"].clone();
        let rejected = client
            .rpc(
                "session.model.update",
                json!({"sessionId":session_id,"branchId":"main","startup":{}}),
            )
            .await;
        assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("product"),
            "{rejected}"
        );
        let attached = client
            .rpc("session.attach", json!({"sessionId":session_id}))
            .await;
        assert_eq!(
            attached["result"]["modelPreferences"],
            json!({"reasoningEffort":"low"}),
            "{attached}"
        );
        turn(&mut client, &session_id, "hello").await;
        assert_eq!(
            configured(&mut client, &session_id).await["model_preferences"],
            json!({"reasoningEffort":"low"})
        );
        assert_eq!(sent_effort(&capture).as_deref(), Some("low"));

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":"local","modelId":"plain"}}),
            )
            .await;
        assert_ok(&created);
        assert!(
            created["result"].get("modelPreferences").is_none(),
            "{created}"
        );
        let plain = created["result"]["sessionId"].clone();
        turn(&mut client, &plain, "hello").await;
        let data = configured(&mut client, &plain).await;
        assert!(data.get("model_preferences").is_none(), "{data}");
        assert_eq!(sent_effort(&capture), None);

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}
