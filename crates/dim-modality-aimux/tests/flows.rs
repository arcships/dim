//! The built-in adapter behind `modality.task.*`: the real product install,
//! an HTTP stub in place of the provider, and the record each flow leaves.
#![cfg(unix)]
use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use dim_agent_capabilities::adapters::MockTools;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use futures_util::{SinkExt, StreamExt};
use http::StatusCode;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsRequest, Response as WsResponse,
};

const CREDENTIAL: &str = "test-credential";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR-stub-image";
const MP3: &[u8] = b"ID3\x04\x00\x00\x00\x00\x00\x00-stub-audio";
const VIDEO: &[u8] = b"stub-mp4-bytes-for-the-video-download";

// ── Provider stub ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
struct StubState {
    requests: Arc<Mutex<Vec<Recorded>>>,
    polls: Arc<AtomicUsize>,
    /// The status polls answered `pending` before the job settles.
    pending: usize,
    base: String,
}

struct Stub {
    base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    polls: Arc<AtomicUsize>,
}

impl Stub {
    /// The requests that started work, one per task.
    fn starts(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "POST" && request.path == path)
            .count()
    }
    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn stub(pending: usize) -> Stub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base_url = format!("http://{address}/v1");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let polls = Arc::new(AtomicUsize::new(0));
    let state = StubState {
        requests: requests.clone(),
        polls: polls.clone(),
        pending,
        base: format!("http://{address}"),
    };
    let app = Router::new().fallback(handle).with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Stub {
        base_url,
        requests,
        polls,
    }
}

async fn handle(State(state): State<StubState>, request: Request) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let body = axum::body::to_bytes(request.into_body(), 1 << 20)
        .await
        .unwrap()
        .to_vec();
    state.requests.lock().unwrap().push(Recorded {
        method: method.clone(),
        path: path.clone(),
        query,
        headers,
        body: body.clone(),
    });
    // A prompt naming "slow" holds the request open, so a test can stop the
    // server or cancel while the call is in flight.
    if String::from_utf8_lossy(&body).contains("slow") {
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
    match (method.as_str(), path.as_str()) {
        ("POST", "/v1/images/generations") | ("POST", "/v1/images/edits") => json_reply(
            StatusCode::OK,
            json!({"created": 1, "data": [{"b64_json": b64(PNG)}]}),
        ),
        ("POST", "/v1/audio/speech") => {
            (StatusCode::OK, [("content-type", "audio/mpeg")], MP3).into_response()
        }
        ("POST", "/v1/audio/transcriptions") => {
            json_reply(StatusCode::OK, json!({"text": "hello there"}))
        }
        ("POST", "/v1/videos/generations") => {
            json_reply(StatusCode::OK, json!({"request_id": "req_1"}))
        }
        ("GET", "/v1/videos/req_1") => {
            let poll = state.polls.fetch_add(1, Ordering::SeqCst) + 1;
            if poll <= state.pending {
                json_reply(StatusCode::ACCEPTED, json!({"status": "pending"}))
            } else {
                // A google package appends its API key to a same-origin video
                // URL (`google/video_model.rs`); the stub hands back that
                // shape, so the download URL carries a secret in its query.
                json_reply(
                    StatusCode::OK,
                    json!({"status": "done",
                        "video": {"url": format!("{}/video.mp4?key={CREDENTIAL}", state.base), "duration": 5}}),
                )
            }
        }
        ("GET", "/video.mp4") => {
            (StatusCode::OK, [("content-type", "video/mp4")], VIDEO).into_response()
        }
        _ => json_reply(
            StatusCode::NOT_FOUND,
            json!({"error": {"message": "no route"}}),
        ),
    }
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn json_reply(status: StatusCode, value: Value) -> Response {
    (
        status,
        [("content-type", "application/json")],
        value.to_string(),
    )
        .into_response()
}

// ── The product host ────────────────────────────────────────────────────────

fn home(prefix: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("dim-c5b-flow-{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

async fn start_in(home: PathBuf) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        unix_path: Some(
            std::path::PathBuf::from("/tmp")
                .join(format!("dim-c5b-flow-{}/server.sock", uuid::Uuid::new_v4())),
        ),
        ..Default::default()
    };
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    dim_agent_capabilities::defaults::install(&mut builder);
    dim_agent_capabilities::defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    // The product's order: the adapter provides the service before Modality
    // reads it.
    dim_modality_aimux::install(&mut builder);
    dim_modality::install(&mut builder);
    serve(builder, config).await.unwrap()
}

struct Client {
    stream: BufReader<UnixStream>,
    id: u64,
    notices: Vec<Value>,
}

impl Client {
    async fn new(server: &RunningServer) -> Self {
        let mut stream = BufReader::new(
            UnixStream::connect(server.unix_path.as_ref().unwrap())
                .await
                .unwrap(),
        );
        let hello = json!({"jsonrpc":"2.0","id":0,"method":"hello","params":{"protocolVersion":3,"clientKind":"other"}});
        stream
            .get_mut()
            .write_all(format!("{hello}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        Self {
            stream,
            id: 0,
            notices: Vec::new(),
        }
    }
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        let value = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        self.stream
            .get_mut()
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.id
    }
    async fn response(&mut self, id: u64) -> Value {
        if let Some(index) = self.notices.iter().position(|v| v["id"] == id) {
            return self.notices.remove(index);
        }
        loop {
            let mut line = String::new();
            assert_ne!(
                self.stream.read_line(&mut line).await.unwrap(),
                0,
                "connection closed"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
            self.notices.push(value);
        }
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.response(id).await
    }
    /// The next notification `method` names, buffering every other message.
    async fn notice(&mut self, method: &str) -> Value {
        if let Some(index) = self
            .notices
            .iter()
            .position(|value| value["method"] == method)
        {
            return self.notices.remove(index);
        }
        loop {
            let mut line = String::new();
            assert_ne!(
                self.stream.read_line(&mut line).await.unwrap(),
                0,
                "connection closed"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["method"] == method {
                return value;
            }
            self.notices.push(value);
        }
    }
}

fn assert_ok(value: &Value) {
    assert!(value.get("error").is_none(), "{value}");
}

fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_default()
}

fn state_of(task: &Value) -> &str {
    task["state"].as_str().unwrap()
}

/// Points the bundled provider at the stub and enables it with a credential,
/// as a user does from Provider settings.
async fn point_at_stub(client: &mut Client, provider_id: &str, base_url: &str) {
    let list = client.rpc("provider.list", json!({})).await;
    assert_ok(&list);
    let provider = list["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == provider_id)
        .unwrap_or_else(|| panic!("no {provider_id} provider: {list}"))
        .clone();
    let version = provider["version"].as_u64().unwrap();
    let reply = client
        .rpc(
            "provider.update",
            json!({
                "providerId": provider_id,
                "expectedVersion": version,
                "patch": {
                    "baseUrl": base_url,
                    "enabled": true,
                    "credential": {"action": "set", "value": CREDENTIAL},
                }
            }),
        )
        .await;
    assert_ok(&reply);
}

fn model(provider_id: &str, model_id: &str) -> Value {
    json!({"providerId": provider_id, "modelId": model_id})
}

async fn submit(
    client: &mut Client,
    category: &str,
    operation: &str,
    params: Value,
    files: Value,
    model: Value,
) -> String {
    let reply = client
        .rpc(
            "modality.task.submit",
            json!({"category": category, "operation": operation, "params": params, "files": files, "model": model}),
        )
        .await;
    assert_ok(&reply);
    reply["result"]["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn status(client: &mut Client, task_id: &str) -> Value {
    let reply = client
        .rpc("modality.task.status", json!({"task_id": task_id}))
        .await;
    assert_ok(&reply);
    reply["result"]["task"].clone()
}

/// The record once `wanted` accepts it, polling the way a client does.
async fn wait_for(client: &mut Client, task_id: &str, wanted: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let task = status(client, task_id).await;
        if wanted(&task) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "task stayed {task} past the deadline"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The record once it reaches `wanted`, polling the way a client does.
async fn wait_state(client: &mut Client, task_id: &str, wanted: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let task = status(client, task_id).await;
        if wanted.contains(&state_of(&task)) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "task stayed {} waiting for {wanted:?}: {task}",
            state_of(&task)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_result(client: &mut Client, task_id: &str, name: &str) -> Vec<u8> {
    let reply = client
        .rpc(
            "modality.task.result",
            json!({"task_id": task_id, "name": name, "offset": 0, "length": 1 << 20}),
        )
        .await;
    assert_ok(&reply);
    base64::engine::general_purpose::STANDARD
        .decode(reply["result"]["data_base64"].as_str().unwrap())
        .unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn assert_no_credential(value: &Value) {
    assert!(
        !value.to_string().contains(CREDENTIAL),
        "the credential reached a client reply: {value}"
    );
}

// ── Flows ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_generation_flow_reaches_the_provider_and_stores_the_result() {
    let stub = stub(2).await;
    let home = home("image");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "openai", &stub.base_url).await;

    let task_id = submit(
        &mut client,
        "image",
        "generate",
        json!({"prompt": "a cat"}),
        json!([]),
        model("openai", "gpt-image-2"),
    )
    .await;
    let task = wait_state(&mut client, &task_id, &["succeeded"]).await;
    assert_no_credential(&task);
    let files = task["result"]["files"].as_array().unwrap();
    assert_eq!(files[0]["name"], "image-1.png");
    assert_eq!(files[0]["media_type"], "image/png");
    assert_eq!(files[0]["sha256"], sha256(PNG));
    assert_eq!(read_result(&mut client, &task_id, "image-1.png").await, PNG);

    let requests = stub.requests();
    let generation = requests
        .iter()
        .find(|request| request.path == "/v1/images/generations")
        .expect("the stub saw the generation");
    assert_eq!(
        generation.header("authorization"),
        Some("Bearer test-credential")
    );
    assert!(generation.body.contains(&b'a'));

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_speech_and_transcription_flows_store_their_files() {
    let stub = stub(2).await;
    let home = home("media");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "openai", &stub.base_url).await;

    let edit = submit(
        &mut client,
        "image",
        "edit",
        json!({"prompt": "add a hat"}),
        json!([{"name": "cat.png", "media_type": "image/png",
            "data_base64": base64::engine::general_purpose::STANDARD.encode(PNG)}]),
        model("openai", "gpt-image-2"),
    )
    .await;
    let task = wait_state(&mut client, &edit, &["succeeded"]).await;
    assert_eq!(task["result"]["files"][0]["name"], "image-1.png");

    let speech = submit(
        &mut client,
        "speech",
        "synthesize",
        json!({"text": "hello", "outputFormat": "wav"}),
        json!([]),
        model("openai", "tts-1"),
    )
    .await;
    let task = wait_state(&mut client, &speech, &["succeeded"]).await;
    assert_eq!(task["result"]["files"][0]["name"], "speech.mp3");
    assert_eq!(task["result"]["files"][0]["media_type"], "audio/mpeg");
    assert_eq!(read_result(&mut client, &speech, "speech.mp3").await, MP3);

    let transcription = submit(
        &mut client,
        "speech",
        "transcribe",
        json!({"language": "en"}),
        json!([{"name": "clip.mp3", "media_type": "audio/mpeg",
            "data_base64": base64::engine::general_purpose::STANDARD.encode(MP3)}]),
        model("openai", "whisper-1"),
    )
    .await;
    let task = wait_state(&mut client, &transcription, &["succeeded"]).await;
    assert_eq!(task["result"]["files"][0]["name"], "transcript.txt");
    assert_eq!(task["result"]["data"]["text"], "hello there");
    assert_eq!(
        read_result(&mut client, &transcription, "transcript.txt").await,
        b"hello there"
    );

    let requests = stub.requests();
    for path in [
        "/v1/images/edits",
        "/v1/audio/speech",
        "/v1/audio/transcriptions",
    ] {
        let request = requests
            .iter()
            .find(|request| request.path == path)
            .unwrap_or_else(|| panic!("the stub saw no {path}"));
        assert_eq!(
            request.header("authorization"),
            Some("Bearer test-credential")
        );
    }

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn video_polls_the_stub_and_resumes_the_handle_after_a_restart() {
    let stub = stub(3).await;
    let home = home("video");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "xai", &stub.base_url).await;

    let task_id = submit(
        &mut client,
        "video",
        "generate",
        json!({"prompt": "a cat"}),
        json!([]),
        model("xai", "grok-video-1"),
    )
    .await;
    // The record turns `running` before `submit` returns the handle, so the
    // restart waits for the handle itself.
    let running = wait_for(&mut client, &task_id, |task| {
        state_of(task) == "running" && task["remote_handle"].is_string()
    })
    .await;
    let handle = running["remote_handle"].as_str().unwrap().to_owned();
    assert!(
        handle.contains("req_1"),
        "the record keeps the start operation"
    );
    assert_no_credential(&running);
    assert!(
        !running.to_string().contains("key="),
        "the record holds the video URL's key query"
    );

    // Stop the server while the job is still polling, then start a new
    // generation from the same home.
    server.shutdown();
    server.join().await.unwrap();
    let restarted = start_in(home.clone()).await;
    let mut client = Client::new(&restarted).await;
    let task = wait_state(&mut client, &task_id, &["succeeded"]).await;
    assert_eq!(task["remote_handle"].as_str(), Some(handle.as_str()));
    assert_eq!(task["result"]["files"][0]["name"], "video-1.mp4");
    assert_eq!(task["result"]["files"][0]["media_type"], "video/mp4");
    assert_eq!(
        read_result(&mut client, &task_id, "video-1.mp4").await,
        VIDEO
    );
    // The completion's URL carried the key in its query; no reply or record
    // keeps the URL or the key.
    assert_no_credential(&task);
    assert!(
        !task.to_string().contains("key="),
        "the record holds the video URL's key query"
    );
    assert_eq!(
        stub.starts("/v1/videos/generations"),
        1,
        "the restart resumed the stored job instead of starting another"
    );
    assert!(
        stub.polls.load(Ordering::SeqCst) >= 3,
        "the status endpoint was polled before and after the restart"
    );
    let downloads: Vec<Recorded> = stub
        .requests()
        .into_iter()
        .filter(|request| request.path == "/video.mp4")
        .collect();
    assert_eq!(downloads.len(), 1);
    assert_eq!(
        downloads[0].query,
        format!("key={CREDENTIAL}"),
        "the download used the completion's URL, key query included"
    );
    // The download shares the provider's origin, so it carries the credential
    // exactly as any other request to that connection does.
    assert_eq!(
        downloads[0].header("authorization"),
        Some("Bearer test-credential")
    );

    restarted.shutdown();
    restarted.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_interrupts_a_blocking_task() {
    let stub = stub(2).await;
    let home = home("interrupted");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "openai", &stub.base_url).await;

    let task_id = submit(
        &mut client,
        "image",
        "generate",
        json!({"prompt": "slow cat"}),
        json!([]),
        model("openai", "gpt-image-2"),
    )
    .await;
    wait_state(&mut client, &task_id, &["running"]).await;
    server.shutdown();
    server.join().await.unwrap();

    let restarted = start_in(home.clone()).await;
    let mut client = Client::new(&restarted).await;
    let task = status(&mut client, &task_id).await;
    assert_eq!(state_of(&task), "interrupted");

    restarted.shutdown();
    restarted.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_is_local_for_an_image_and_unsupported_for_a_video() {
    let stub = stub(2).await;
    let home = home("cancel");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "openai", &stub.base_url).await;
    point_at_stub(&mut client, "xai", &stub.base_url).await;

    let image = submit(
        &mut client,
        "image",
        "generate",
        json!({"prompt": "slow cat"}),
        json!([]),
        model("openai", "gpt-image-2"),
    )
    .await;
    wait_state(&mut client, &image, &["running"]).await;
    let reply = client
        .rpc("modality.task.cancel", json!({"task_id": image}))
        .await;
    assert_ok(&reply);
    let task = wait_state(&mut client, &image, &["cancelled"]).await;
    assert!(
        task["remote_cancel"].is_null(),
        "a blocking task has no remote cancel: {task}"
    );

    let video = submit(
        &mut client,
        "video",
        "generate",
        json!({"prompt": "a cat"}),
        json!([]),
        model("xai", "grok-video-1"),
    )
    .await;
    wait_state(&mut client, &video, &["running"]).await;
    let reply = client
        .rpc("modality.task.cancel", json!({"task_id": video}))
        .await;
    assert_ok(&reply);
    let task = wait_state(&mut client, &video, &["cancelled"]).await;
    assert_eq!(
        task["remote_cancel"], "unsupported",
        "aimux exposes no cancel for a started video job: {task}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_realtime_session_relays_its_frames_and_events_over_the_socket() {
    // The provider side of one realtime session: the catalog entry's
    // declaration says `/realtime`, and the connection adds the model id.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stub = tokio::spawn(async move {
        // tungstenite's own `ErrorResponse` size is not ours to box.
        #[allow(clippy::result_large_err)]
        fn checked(
            request: &WsRequest,
            response: WsResponse,
        ) -> Result<WsResponse, http::Response<Option<String>>> {
            assert_eq!(
                request.uri().path_and_query().unwrap().as_str(),
                "/v1/realtime?model=stepaudio-2.5-realtime"
            );
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                format!("Bearer {CREDENTIAL}").as_str()
            );
            Ok(response)
        }
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, checked)
            .await
            .unwrap();
        let text = socket.next().await.unwrap().unwrap();
        let Message::Text(text) = text else {
            panic!("the client's first frame is a text frame")
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "session.update");
        socket
            .send(Message::Text(
                r#"{"type":"session.created","session":{"id":"s"}}"#.into(),
            ))
            .await
            .unwrap();
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    });

    let home = home("realtime");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "step-plan", &format!("http://{address}/v1")).await;
    let opened = client
        .rpc(
            "modality.realtime.open",
            json!({"operation": "voice", "model": model("step-plan", "stepaudio-2.5-realtime"), "params": {}}),
        )
        .await;
    assert_ok(&opened);
    let id = opened["result"]["connectionId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(opened["result"]["transport"], "relay");
    let sent = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "session.update", "session": {"instructions": "hi"}}}}),
        )
        .await;
    assert_ok(&sent);
    let event = client.notice("modality.realtime.event").await;
    assert_eq!(event["params"]["connectionId"], id);
    assert_eq!(event["params"]["event"]["type"], "session.created");
    assert_no_credential(&event);
    let closed = client
        .rpc("modality.realtime.close", json!({"connectionId": id}))
        .await;
    assert_ok(&closed);
    let ended = client.notice("modality.realtime.ended").await;
    assert_eq!(ended["params"]["reason"], "closed");
    stub.await.unwrap();
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capabilities_show_the_mapped_provider_and_refuse_a_created_one() {
    let stub = stub(2).await;
    let home = home("capabilities");
    let server = start_in(home.clone()).await;
    let mut client = Client::new(&server).await;
    point_at_stub(&mut client, "openai", &stub.base_url).await;

    let capabilities = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&capabilities);
    let categories = capabilities["result"]["categories"].as_array().unwrap();
    let image = categories
        .iter()
        .find(|category| category["category"] == "image")
        .unwrap();
    let generate = image["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["operation"] == "generate")
        .unwrap();
    assert_eq!(generate["available"], true);
    assert!(
        generate["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["providerId"] == "openai"
                && option["modelId"] == "gpt-image-2"
                && option["available"] == true),
        "the bundled image model is an option: {generate}"
    );
    assert_no_credential(&capabilities);

    // A created provider's id is a UUID, which no row names.
    let created = "018ec73f-0000-7000-8000-00000000c5b0";
    let reply = client
        .rpc(
            "provider.create",
            json!({
                "providerId": created,
                "displayName": "Created",
                "connection": {"driverKind": "openai-compatible", "baseUrl": stub.base_url, "auth": {"type": "anonymous"}},
                "models": [{
                    "modelId": "custom-image",
                    "displayName": "custom-image",
                    "status": "active",
                    "capabilities": {"modalities":["image"],"toolCalling":false,"streaming":true,
                        "structuredOutput":false,"vision":false,"audio":false,"reasoning":false},
                    "metadata": {"raw": {"type":"image-generation","modalityCapabilities":["image.generate"]}}
                }],
                "enabled": true
            }),
        )
        .await;
    assert_ok(&reply);
    let revision = capabilities["result"]["revision"].as_str().unwrap();
    let refused = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "generate",
                "model": model(created, "custom-image")}),
        )
        .await;
    assert_eq!(app_error(&refused), "modality_invalid_selection");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("adapter"),
        "the refusal names the adapter as the reason: {refused}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
