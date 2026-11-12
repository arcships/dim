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

/// The fake Gemini provider: records every request body and streams a reply.
async fn google(
    State(capture): State<Arc<Capture>>,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    capture.requests.lock().unwrap().push(body);
    let reply = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},
        "finishReason":"STOP","index":0}],
        "usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}});
    if uri.path().contains(":streamGenerateContent") {
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {}\n\n", reply),
        )
    } else {
        (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            reply.to_string(),
        )
    }
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

/// A model descriptor whose reasoning declaration carries whether reasoning is
/// on by default, the `default` effort beside its offered efforts, and `off`
/// when the model declares one. `reasoning` is the model's reasoning
/// capability: `false` denies the control the declaration would otherwise
/// describe, and the projection derives the capability from it.
fn model_with_off(
    model_id: &str,
    default_enabled: Option<bool>,
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
    if let Some(enabled) = default_enabled {
        declaration["defaultEnabled"] = json!(enabled);
    }
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
        None,
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

/// The effort the last request carried, if any: the Anthropic effort field, or
/// the effort field the compatible models write themselves.
fn sent_effort(capture: &Capture) -> Option<String> {
    let body = capture.requests.lock().unwrap().last().cloned()?;
    body.get("output_config")
        .and_then(|output| output.get("effort"))
        .or_else(|| body.get("reasoning_effort"))
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
                    None,
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
                model_with_off("native", None, Some("none"), &["low", "none"], None, true),
                model("off", Some("none"), &["low"]),
                model_with_off(
                    "unexpressible",
                    None,
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

/// #1376: a model whose declaration says reasoning is off by default adopts
/// no reasoning at all when its declaration carries no way to say off, while a
/// model that declares reasoning on, and one that leaves the field undeclared,
/// keep taking the declared effort.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_that_does_not_reason_by_default_adopts_no_declared_effort() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("default-enabled-off");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000110";
        create_provider(
            &mut client,
            provider,
            "anthropic",
            &format!("http://{endpoint}"),
            vec![
                model_with_off(
                    "off-by-default",
                    Some(false),
                    Some("high"),
                    &["low", "high"],
                    None,
                    true,
                ),
                model_with_off(
                    "on-by-default",
                    Some(true),
                    Some("high"),
                    &["low", "high"],
                    Some(json!({"kind":"thinking_type","value":"disabled"})),
                    true,
                ),
                model_with_off(
                    "undeclared",
                    None,
                    Some("high"),
                    &["low", "high"],
                    Some(json!({"kind":"thinking_type","value":"disabled"})),
                    true,
                ),
            ],
        )
        .await;

        for (model_id, expected) in [
            ("off-by-default", None),
            ("on-by-default", Some(json!({"reasoningEffort":"high"}))),
            ("undeclared", Some(json!({"reasoningEffort":"high"}))),
        ] {
            let created = client
                .rpc(
                    "session.create",
                    json!({"cwd":directory,"tools":[],
                        "model":{"provider":provider,"modelId":model_id}}),
                )
                .await;
            assert_ok(&created);
            let session_id = created["result"]["sessionId"].clone();
            assert_eq!(
                created["result"].get("modelPreferences"),
                expected.as_ref(),
                "{model_id}: {created}"
            );
            turn(&mut client, &session_id, "hello").await;
            assert_eq!(
                configured(&mut client, &session_id)
                    .await
                    .get("model_preferences"),
                expected.as_ref(),
                "{model_id}"
            );
            let wire = last_body(&capture);
            let effort = expected.as_ref().map(|_| "high");
            assert_eq!(
                sent_effort(&capture).as_deref(),
                effort,
                "{model_id}: {wire}"
            );
            assert!(wire.get("thinking").is_none(), "{model_id}: {wire}");
        }

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// #1376: a model that declares reasoning off by default and declares how to
/// say so adopts that declaration instead of the effort beside it, in each
/// protocol that spells off its own way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_that_does_not_reason_by_default_adopts_its_off_declaration() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let capture = Arc::new(Capture::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/messages", post(messages))
            .route("/v1/chat/completions", post(completions))
            .fallback(google)
            .with_state(capture.clone());
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = directory("default-off-mapped");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;

        for (provider, driver, base_url, off, wire_off, wire_off_value) in [
            (
                "018ec73f-0000-7000-8000-000000000111",
                "anthropic",
                format!("http://{endpoint}"),
                json!({"kind":"thinking_type","value":"disabled"}),
                "/thinking/type",
                json!("disabled"),
            ),
            (
                "018ec73f-0000-7000-8000-000000000112",
                "openai-compatible",
                format!("http://{endpoint}/v1"),
                json!({"kind":"thinking_type","value":"disabled"}),
                "/thinking/type",
                json!("disabled"),
            ),
            (
                "018ec73f-0000-7000-8000-000000000113",
                "gemini",
                format!("http://{endpoint}/v1"),
                json!({"kind":"thinking_budget","value":0}),
                "/generationConfig/thinkingConfig/thinkingBudget",
                json!(0),
            ),
        ] {
            create_provider(
                &mut client,
                provider,
                driver,
                &base_url,
                vec![
                    model_with_off(
                        "off-mapped",
                        Some(false),
                        Some("medium"),
                        &["low", "medium"],
                        Some(off.clone()),
                        true,
                    ),
                    model_with_off(
                        "off-mapped-without-effort",
                        Some(false),
                        None,
                        &["low", "medium"],
                        Some(off.clone()),
                        true,
                    ),
                ],
            )
            .await;

            for model_id in ["off-mapped", "off-mapped-without-effort"] {
                let created = client
                    .rpc(
                        "session.create",
                        json!({"cwd":directory,"tools":[],
                            "model":{"provider":provider,"modelId":model_id}}),
                    )
                    .await;
                assert_ok(&created);
                assert_eq!(
                    created["result"]["modelPreferences"],
                    json!({"reasoningEffort":"none"}),
                    "{driver} {model_id}: {created}"
                );
                let session_id = created["result"]["sessionId"].clone();
                turn(&mut client, &session_id, "hello").await;
                assert_eq!(
                    configured(&mut client, &session_id).await["model_preferences"],
                    json!({"reasoningEffort":"none"}),
                    "{driver} {model_id}"
                );
                let wire = last_body(&capture);
                assert_eq!(
                    wire.pointer(wire_off),
                    Some(&wire_off_value),
                    "{driver} {model_id}: {wire}"
                );
                assert_eq!(sent_effort(&capture), None, "{driver} {model_id}: {wire}");
            }
        }

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// The fake OpenAI Responses provider: records every request body and streams
/// a short reply.
async fn responses(
    State(capture): State<Arc<Capture>>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    capture.requests.lock().unwrap().push(body);
    let payload: String = [
        json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"fixture"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}),
        json!({"type":"response.content_part.added","item_id":"msg-1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
        json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":0,"content_index":0,"delta":"ok"}),
        json!({"type":"response.output_text.done","item_id":"msg-1","output_index":0,"content_index":0,"text":"ok"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}),
        json!({"type":"response.completed","response":{"id":"resp-1","created_at":1,"model":"fixture","status":"completed","usage":{"input_tokens":5,"output_tokens":3,"total_tokens":8}}}),
    ]
    .iter()
    .map(|data| format!("data: {data}\n\n"))
    .collect();
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        payload,
    )
}

/// A model descriptor declaring `summaries` beside its effort, on the OpenAI
/// Responses protocol: the reasoning-off control there is a native `none`
/// effort, so `none` is always offered.
fn summarising(model_id: &str, default: Option<&str>, options: &[&str]) -> Value {
    let mut offered = options.to_vec();
    if !offered.contains(&"none") {
        offered.push("none");
    }
    let mut value = model_with_off(model_id, None, default, &offered, None, true);
    value["metadata"]["reasoning"]["summaries"] = json!(true);
    value["metadata"]["reasoning"]["visibility"] = json!("summary");
    value
}

/// The request body with the prompt replaced by its message roles. Every
/// fixture sends the same session prompt and none of these tests is about it,
/// but every other field is compared exactly as sent.
fn wire_shape(body: &Value) -> Value {
    let mut shape = body.clone();
    shape["input"] = json!(
        body["input"]
            .as_array()
            .map(|input| input
                .iter()
                .map(|message| message["role"].clone())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    shape
}

/// The fake endpoint the summary tests share: Responses, Chat and Messages on
/// one port, all recording into one capture.
async fn summary_endpoint() -> (Arc<Capture>, String, tokio::task::JoinHandle<()>) {
    let capture = Arc::new(Capture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(completions))
        .route("/v1/messages", post(messages))
        .fallback(google)
        .with_state(capture.clone());
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (capture, format!("http://{endpoint}"), http)
}

/// The summary style the last request carried, if any.
fn sent_summary(capture: &Capture) -> Option<String> {
    let body = capture.requests.lock().unwrap().last().cloned()?;
    body.get("reasoning")
        .and_then(|reasoning| reasoning.get("summary"))
        .or_else(|| body.get("reasoningSummary"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The effort the last request carried on any protocol this file drives,
/// including the OpenAI Responses `reasoning` object.
fn sent_reasoning_effort(capture: &Capture) -> Option<String> {
    let body = capture.requests.lock().unwrap().last().cloned()?;
    body.get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| sent_effort(capture))
}

/// 1. A declared summary reaches the request beside the effort it summarizes,
/// and 2(a). an explicit `none` effort sends none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_summary_reaches_the_request_beside_the_effort() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let (capture, base, http) = summary_endpoint().await;
        let directory = directory("declared-summary");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000301";
        create_provider(
            &mut client,
            provider,
            "openai-responses",
            &format!("{base}/v1"),
            vec![summarising(
                "summarised",
                Some("medium"),
                &["low", "medium", "high"],
            )],
        )
        .await;

        // The declared default effort carries the declared summary.
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"summarised"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        assert_eq!(sent_reasoning_effort(&capture).as_deref(), Some("medium"));
        assert_eq!(sent_summary(&capture).as_deref(), Some("detailed"));

        // An explicit `none` suppresses it: asking a provider to summarize the
        // reasoning the same request just turned off is not a request dim sends.
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"summarised"},
                    "modelPreferences":{"reasoningEffort":"none"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        let wire = last_body(&capture);
        // The effort itself still reaches the wire as a native `none`; what
        // must not follow it is a summary.
        assert_eq!(
            sent_reasoning_effort(&capture).as_deref(),
            Some("none"),
            "{wire}"
        );
        assert_eq!(sent_summary(&capture), None, "{wire}");

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 2(b) and 3. An update to `none` drops the summary, an update back to a
/// concrete effort sends it again, and a clear returns to the declared default
/// with the summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_update_keeps_the_summary_in_step_with_the_effort() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let (capture, base, http) = summary_endpoint().await;
        let directory = directory("summary-update");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000302";
        create_provider(
            &mut client,
            provider,
            "openai-responses",
            &format!("{base}/v1"),
            vec![summarising(
                "summarised",
                Some("medium"),
                &["low", "medium", "high"],
            )],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"summarised"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        assert_eq!(sent_summary(&capture).as_deref(), Some("detailed"));

        // Off: nothing a later update could carry back in.
        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":session_id,"branchId":"main",
                        "modelPreferences":{"reasoningEffort":"none"}}),
                )
                .await,
        );
        turn(&mut client, &session_id, "off").await;
        let wire = last_body(&capture);
        assert_eq!(
            sent_reasoning_effort(&capture).as_deref(),
            Some("none"),
            "{wire}"
        );
        assert_eq!(sent_summary(&capture), None, "{wire}");

        // Back on: the summary returns with the effort it summarizes.
        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":session_id,"branchId":"main",
                        "modelPreferences":{"reasoningEffort":"high"}}),
                )
                .await,
        );
        turn(&mut client, &session_id, "on").await;
        assert_eq!(sent_reasoning_effort(&capture).as_deref(), Some("high"));
        assert_eq!(sent_summary(&capture).as_deref(), Some("detailed"));

        // A clear returns to the declared default, summary included.
        assert_ok(
            &client
                .rpc(
                    "session.model.update",
                    json!({"sessionId":session_id,"branchId":"main",
                        "modelPreferences":{"reasoningEffort":{"action":"clear"}}}),
                )
                .await,
        );
        turn(&mut client, &session_id, "cleared").await;
        assert_eq!(sent_reasoning_effort(&capture).as_deref(), Some("medium"));
        assert_eq!(sent_summary(&capture).as_deref(), Some("detailed"));

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 4. A declaration with no effort adds nothing: the request is byte-identical
/// to the one `1efbd75df` sends, which reaches the model as a plain system
/// prompt with no reasoning block. The expectation is the captured baseline,
/// not a hand-written one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_summary_declaration_with_no_effort_sends_nothing() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let (capture, base, http) = summary_endpoint().await;
        let directory = directory("summary-no-effort");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000303";
        create_provider(
            &mut client,
            provider,
            "openai-responses",
            &format!("{base}/v1"),
            vec![summarising("longcat", None, &[])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"longcat"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        let wire = last_body(&capture);
        assert!(wire.get("reasoning").is_none(), "{wire}");
        // The baseline body captured at 1efbd75df for this fixture.
        assert_eq!(
            wire_shape(&wire),
            json!({
                "model": "longcat",
                "input": ["system", "user", "user", "user", "user", "user"],
                "max_output_tokens": 4096,
                "include": ["reasoning.encrypted_content"],
                "store": false,
                "stream": true,
            }),
            "{wire}"
        );

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 5. The same declaration on a protocol with no place for a summary style
/// adopts successfully and sends no summary field anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_summary_declaration_is_not_sent_where_it_is_not_expressible() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let (capture, base, http) = summary_endpoint().await;
        let directory = directory("summary-not-expressible");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;

        for (provider, driver, base_url, off) in [
            (
                "018ec73f-0000-7000-8000-000000000304",
                "openai-compatible",
                format!("{base}/v1"),
                json!({"kind":"thinking_type","value":"disabled"}),
            ),
            (
                "018ec73f-0000-7000-8000-000000000305",
                "anthropic",
                base.clone(),
                json!({"kind":"thinking_type","value":"disabled"}),
            ),
            (
                "018ec73f-0000-7000-8000-000000000306",
                "gemini",
                format!("{base}/v1"),
                json!({"kind":"thinking_budget","value":0}),
            ),
        ] {
            // Not a Responses route: the declaration spells its own off
            // mapping and offers no native `none`.
            let mut declared = summarising("declared", Some("medium"), &["low", "medium"]);
            declared["metadata"]["reasoning"]["effortOptions"] = json!(["low", "medium"]);
            declared["metadata"]["reasoning"]["off"] = off;
            create_provider(&mut client, provider, driver, &base_url, vec![declared]).await;

            let created = client
                .rpc(
                    "session.create",
                    json!({"cwd":directory,"tools":[],
                        "model":{"provider":provider,"modelId":"declared"}}),
                )
                .await;
            assert_ok(&created);
            let session_id = created["result"]["sessionId"].clone();
            turn(&mut client, &session_id, "hello").await;
            let wire = last_body(&capture);
            assert_eq!(sent_summary(&capture), None, "{driver}: {wire}");
            assert!(wire.get("reasoningSummary").is_none(), "{driver}: {wire}");
            assert!(
                wire.get("reasoning")
                    .and_then(|reasoning| reasoning.get("summary"))
                    .is_none(),
                "{driver}: {wire}"
            );
        }

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// 7. A model that declares nothing sends nothing new: the whole body matches
/// the baseline captured at `1efbd75df`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_that_declares_no_summary_sends_the_baseline_body() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let (capture, base, http) = summary_endpoint().await;
        let directory = directory("summary-silent");
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        let provider = "018ec73f-0000-7000-8000-000000000307";
        create_provider(
            &mut client,
            provider,
            "openai-responses",
            &format!("{base}/v1"),
            vec![model("silent", Some("medium"), &["low", "medium", "high"])],
        )
        .await;

        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory,"tools":[],
                    "model":{"provider":provider,"modelId":"silent"}}),
            )
            .await;
        assert_ok(&created);
        let session_id = created["result"]["sessionId"].clone();
        turn(&mut client, &session_id, "hello").await;
        let wire = last_body(&capture);
        // Baseline at 1efbd75df: the adapter's own default already sends
        // `detailed` beside a concrete effort, so the body is unchanged.
        assert_eq!(
            wire_shape(&wire),
            json!({
                "model": "silent",
                "input": ["developer", "user", "user", "user", "user", "user"],
                "max_output_tokens": 4096,
                "include": ["reasoning.encrypted_content"],
                "store": false,
                "reasoning": {"effort": "medium", "summary": "detailed"},
                "stream": true,
            }),
            "{wire}"
        );

        server.child.kill().await.unwrap();
        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// #1377 r3: a configuration stored under a `--model-config` profile lands in a
/// store a product daemon later opens, and `session.model.update` starts from
/// that stored configuration. The profile's model file can put a style in the
/// generic `reasoning.summary` field, so the rule sees a style it did not write:
/// it keeps a `concise`, and re-derives a `detailed` that has no effort to follow.
/// Both shapes of that are pinned: an update that moves the effort to `none`, and
/// a configuration carrying no effort at all on a model that declares none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stored_generic_summary_keeps_its_style_and_follows_the_effort() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let (capture, base, http) = summary_endpoint().await;
        let directory = directory("stored-generic-summary");
        let provider = "018ec73f-0000-7000-8000-000000000308";

        // The product daemon creates the Provider the profile names, so the
        // product catalog can resolve it after the profile wrote the store.
        let mut server = launch_product(&directory).await;
        let mut client = connect(&server).await;
        create_provider(
            &mut client,
            provider,
            "openai-responses",
            &format!("{base}/v1"),
            vec![
                summarising("summarised", Some("medium"), &["low", "medium", "high"]),
                summarising("silent", None, &[]),
            ],
        )
        .await;
        server.child.kill().await.unwrap();

        // The profile's own configuration: an effort beside the style, or — for
        // `silent` — no effort at all, so the stored configuration carries only
        // the style. `sent` is the summary the profile's own first turn carries,
        // which it passes through untouched; `expected` is what the product
        // daemon's rule keeps once it finalizes the configuration again.
        for (model, style, effort, update, sent, expected) in [
            (
                "summarised",
                "concise",
                "low",
                Some(json!({"reasoningEffort":"high"})),
                Some("concise"),
                Some("concise"),
            ),
            (
                "summarised",
                "detailed",
                "low",
                Some(json!({"reasoningEffort":"none"})),
                Some("detailed"),
                None,
            ),
            (
                "silent",
                "detailed",
                "",
                Some(json!({"reasoningEffort":{"action":"clear"}})),
                Some("detailed"),
                None,
            ),
        ] {
            let configuration = directory.join(format!("{model}-{style}.json"));
            let mut reasoning = json!({"summary":style});
            if !effort.is_empty() {
                reasoning["effort"] = json!(effort);
            }
            std::fs::write(
                &configuration,
                serde_json::to_vec(&json!({
                    "defaultModel":{"providerId":provider,"modelId":model},
                    "providers":[{"id":provider,"kind":"openai-responses",
                        "baseUrl":format!("{base}/v1"),
                        "models":[{
                            "id":model,"contextWindow":32_000,
                            "maxOutputTokens":4_096,
                            "config":{"maxTokens":4096,"reasoning":reasoning}
                        }]}]
                }))
                .unwrap(),
            )
            .unwrap();
            let mut configured = launch_configured(&directory, &configuration).await;
            let mut profile = connect(&configured).await;
            let created = profile
                .rpc(
                    "session.create",
                    json!({"cwd":directory,"tools":[],
                        "model":{"provider":provider,"modelId":model}}),
                )
                .await;
            assert_ok(&created);
            let session_id = created["result"]["sessionId"].clone();
            turn(&mut profile, &session_id, "hello").await;
            assert_eq!(
                sent_summary(&capture).as_deref(),
                sent,
                "{model} {style}: the summary the first turn carries"
            );
            configured.child.kill().await.unwrap();

            // The product daemon reopens the store and finalizes the stored
            // configuration again, so the rule sees what the profile wrote.
            let mut server = launch_product(&directory).await;
            let mut client = connect(&server).await;
            assert_ok(
                &client
                    .rpc(
                        "session.model.update",
                        json!({"sessionId":session_id,"branchId":"main",
                            "modelPreferences":update}),
                    )
                    .await,
            );
            let stored = stored_configuration(&directory, &session_id).await;
            let generic = stored
                .pointer("/config/reasoning/summary")
                .and_then(Value::as_str);
            let provider = stored
                .pointer("/config/providerOptions/openai/summary")
                .and_then(Value::as_str);
            assert_eq!(
                provider.or(generic),
                expected,
                "{model} {style}: the style the body carries: {stored}"
            );
            if expected.is_some() {
                // A kept style stays in the field it was configured in.
                assert_eq!(generic, Some(style), "{model} {style}: {stored}");
            } else {
                assert_eq!(generic, None, "{model} {style}: {stored}");
            }
            server.child.kill().await.unwrap();
        }

        http.abort();
        std::fs::remove_dir_all(directory).unwrap();
    })
    .await
    .unwrap();
}

/// The newest stored `context/configured` settings of `session`, read after the
/// daemon that wrote them exited.
async fn stored_configuration(directory: &std::path::Path, session_id: &Value) -> Value {
    let database = directory.join("state/dimcode.sqlite");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let read = (|| -> Option<Value> {
            let connection = rusqlite::Connection::open(&database).ok()?;
            let body: String = connection
                .query_row(
                    "SELECT body FROM timeline_events WHERE session_id=?1
                     AND branch_id='main' AND kind='context/configured'
                     ORDER BY sequence DESC LIMIT 1",
                    [session_id.as_str()?],
                    |row| row.get(0),
                )
                .ok()?;
            let record: Value = serde_json::from_str(&body).ok()?;
            Some(record.pointer("/record/event/data/settings")?.clone())
        })();
        if let Some(settings) = read {
            return settings;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no stored configuration"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
