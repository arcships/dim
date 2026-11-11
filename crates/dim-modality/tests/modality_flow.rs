//! Modality image slice through the management methods: the owner's snapshot,
//! resolution order and CAS, with a fixture adapter in the process.
#![cfg(unix)]
use dim_agent_capabilities::host::ModalityConnection;
use dim_agent_capabilities::{adapters::MockTools, defaults, host::ToolDriver};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_types::{CatalogCapabilities, ModelId};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_modality::{
    AdapterError, AdapterFuture, AdapterOperation, AdapterOutput, AdapterProgress, ArtifactHook,
    Clock, InstallOptions, Job, JobFile, ModalityAdapter, ModalityAdapterService, ProgressSink,
    RealtimeConnection, RealtimeEnd, RealtimeEvent, RealtimeFrame, RealtimeLimits, RealtimeOpened,
    RealtimeTransport, RemoteCancel, Submitted,
};
use futures_util::{SinkExt, StreamExt};
use rutis::Disposer;
use rutis::{Ctx, Plugin};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use tokio_util::sync::CancellationToken;

enum Transport {
    Unix(BufReader<UnixStream>),
    Ws(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}
struct Client {
    transport: Transport,
    id: u64,
    notices: Vec<Value>,
}
impl Client {
    async fn new_version(server: &RunningServer, ws: bool, version: u32) -> Self {
        let transport = if ws {
            Transport::Ws(Box::new(
                tokio_tungstenite::connect_async_with_config(
                    format!("ws://{}/ws", server.websocket_address.unwrap()),
                    Some(
                        tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                            .max_frame_size(Some(128 * 1024 * 1024))
                            .max_message_size(Some(128 * 1024 * 1024)),
                    ),
                    false,
                )
                .await
                .unwrap()
                .0,
            ))
        } else {
            Transport::Unix(BufReader::new(
                UnixStream::connect(server.unix_path.as_ref().unwrap())
                    .await
                    .unwrap(),
            ))
        };
        let mut this = Self {
            transport,
            id: 0,
            notices: vec![],
        };
        let result = this
            .rpc(
                "hello",
                json!({"protocolVersion":version,"clientKind":"other"}),
            )
            .await;
        assert_eq!(result["result"]["protocolVersion"], version);
        if version == 3 {
            assert!(result["result"]["features"].is_array());
        }
        this
    }
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        let value = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        self.raw(value).await;
        self.id
    }
    async fn raw(&mut self, value: Value) {
        let text = value.to_string();
        match &mut self.transport {
            Transport::Unix(stream) => {
                stream.get_mut().write_all(text.as_bytes()).await.unwrap();
                stream.get_mut().write_all(b"\n").await.unwrap();
            }
            Transport::Ws(stream) => stream.send(Message::Text(text.into())).await.unwrap(),
        }
    }
    async fn read(&mut self, expected: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            match &mut self.transport {
                Transport::Unix(stream) => {
                    let mut line = String::new();
                    assert_ne!(
                        stream.read_line(&mut line).await.unwrap(),
                        0,
                        "connection unexpectedly closed"
                    );
                    serde_json::from_str(&line).unwrap()
                }
                Transport::Ws(stream) => loop {
                    match stream.next().await.unwrap().unwrap() {
                        Message::Text(text) => break serde_json::from_str(&text).unwrap(),
                        Message::Ping(_) | Message::Pong(_) => {}
                        other => panic!("unexpected websocket: {other:?}"),
                    }
                },
            }
        })
        .await
        .unwrap_or_else(|error| panic!("deadline waiting for {expected}: {error}"))
    }
    async fn response(&mut self, id: u64) -> Value {
        if let Some(index) = self.notices.iter().position(|v| v["id"] == id) {
            return self.notices.remove(index);
        }
        loop {
            let value = self.read(&format!("response {id}")).await;
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
}

/// A product profile with Modality installed, as the product's installer does.
async fn launch_with(
    tools: Arc<dyn ToolDriver>,
    mut config: ServerConfig,
    options: Option<InstallOptions>,
    extra: impl FnOnce(&mut AgentBuilder),
) -> RunningServer {
    config.unix_path = Some(std::path::PathBuf::from("/tmp").join(format!(
        "dim-modality-test-{}/server.sock",
        uuid::Uuid::new_v4()
    )));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, tools);
    dim_app_server::collaboration::install(&mut builder);
    dim_modality::install_with(&mut builder, options.unwrap_or_default());
    extra(&mut builder);
    serve(builder, config).await.unwrap()
}
fn assert_ok(value: &Value) {
    assert!(value.get("error").is_none(), "{value}");
}

/// The fixture `modalityAdapter`: it serves the image operations a test
/// declares per model id, and its task behaviour is set before each
/// submission (handle, delays, gate, progress, outputs, cancel support).
#[derive(Clone, Debug, Default)]
enum Handles {
    /// A distinct handle per task, `handle-<task id>`.
    #[default]
    PerTask,
    /// No handle: the adapter keys its in-flight work by `task_id`.
    None,
}

#[derive(Clone, Debug)]
struct Behavior {
    handles: Handles,
    submit_error: Option<AdapterError>,
    /// When set, `submit` blocks until it fires, then returns its handle.
    submit_gate: Option<CancellationToken>,
    wait_error: Option<AdapterError>,
    /// name, media type, bytes written into the work directory.
    files: Vec<(String, String, Vec<u8>)>,
    /// Returned as the operation's structured data.
    data: Option<Value>,
    /// Progress reports sent before the gate.
    progress: Vec<AdapterProgress>,
    /// When set, `wait` blocks until it fires.
    gate: Option<CancellationToken>,
    /// The output file's bytes are the job's own task id.
    named_output: bool,
    /// Complete later submissions before earlier ones.
    reverse_completion: bool,
    /// A path outside the work directory to return instead of writing.
    outside: Option<std::path::PathBuf>,
    /// A name inside the work directory to leave as a link to the given path.
    link: Option<(String, std::path::PathBuf)>,
    remote_cancel: RemoteCancel,
}
impl Default for Behavior {
    fn default() -> Self {
        Self {
            handles: Handles::PerTask,
            submit_error: None,
            submit_gate: None,
            wait_error: None,
            files: vec![(
                "out.bin".into(),
                "image/png".into(),
                b"fixture result".to_vec(),
            )],
            data: None,
            progress: Vec::new(),
            gate: None,
            named_output: false,
            reverse_completion: false,
            outside: None,
            link: None,
            remote_cancel: RemoteCancel::Cancelled,
        }
    }
}

/// How the fixture serves one `open` (A.2 `realtimeOpen`).
#[derive(Clone, Debug, Default)]
struct RealtimeBehavior {
    /// Serve `direct` with a short-lived connection instead of a relay.
    direct: bool,
    /// The connection's `expires_at`, this far in the future.
    expires_in: Option<Duration>,
    /// Fail the next `open` with this error.
    open_error: Option<AdapterError>,
}

/// One session the fixture opened, and what it was given.
struct RealtimeOpen {
    channel: String,
    params: Value,
    protocol: Option<String>,
    path: Option<String>,
}

/// The fixture's realtime side: what it serves and what reached it.
#[derive(Default)]
struct RealtimeState {
    behavior: RealtimeBehavior,
    opens: Vec<RealtimeOpen>,
    /// The event sender of each open channel, so a test can emit an event.
    events: HashMap<String, tokio::sync::mpsc::Sender<RealtimeEvent>>,
    frames: Vec<(String, RealtimeFrame)>,
    closes: Vec<String>,
}

struct FixtureAdapter {
    /// Model id to the operation ids it serves; `None` serves all three.
    served: Mutex<Option<BTreeMap<String, BTreeSet<String>>>>,
    /// Operation id to its parameter schema.
    schemas: Mutex<BTreeMap<String, Value>>,
    available: AtomicBool,
    behavior: Mutex<Behavior>,
    /// Task id to the job its submission carried.
    jobs: Mutex<HashMap<String, Job>>,
    /// Handle to task id.
    handles: Mutex<HashMap<String, String>>,
    /// Task ids of no-handle submissions, in submission order.
    order: Mutex<Vec<String>>,
    /// Task ids whose `wait` finished, for `reverse_completion`.
    finished: Mutex<std::collections::HashSet<String>>,
    finished_notify: tokio::sync::Notify,
    /// Every task id a `wait` was given.
    wait_ids: Mutex<Vec<String>>,
    submits: AtomicUsize,
    waits: AtomicUsize,
    remote_cancels: AtomicUsize,
    disposer: Mutex<Option<Disposer>>,
    realtime: Mutex<RealtimeState>,
}
impl Default for FixtureAdapter {
    fn default() -> Self {
        Self {
            served: Mutex::new(None),
            schemas: Mutex::new(BTreeMap::new()),
            available: AtomicBool::new(true),
            behavior: Mutex::new(Behavior::default()),
            realtime: Mutex::new(RealtimeState::default()),
            jobs: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            finished: Mutex::new(std::collections::HashSet::new()),
            finished_notify: tokio::sync::Notify::new(),
            wait_ids: Mutex::new(Vec::new()),
            submits: AtomicUsize::new(0),
            waits: AtomicUsize::new(0),
            remote_cancels: AtomicUsize::new(0),
            disposer: Mutex::new(None),
        }
    }
}
impl FixtureAdapter {
    fn serve(&self, served: &[(&str, &[&str])]) {
        *self.served.lock().unwrap() = Some(
            served
                .iter()
                .map(|(model, operations)| {
                    (
                        (*model).to_owned(),
                        operations
                            .iter()
                            .map(|operation| (*operation).to_owned())
                            .collect(),
                    )
                })
                .collect(),
        );
    }
    fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }
    fn set_behavior(&self, behavior: Behavior) {
        *self.behavior.lock().unwrap() = behavior;
    }
    fn behavior(&self) -> Behavior {
        self.behavior.lock().unwrap().clone()
    }
    fn set_schema(&self, operation: &str, schema: Value) {
        self.schemas
            .lock()
            .unwrap()
            .insert(operation.to_owned(), schema);
    }
    fn submits(&self) -> usize {
        self.submits.load(Ordering::SeqCst)
    }
    fn waits(&self) -> usize {
        self.waits.load(Ordering::SeqCst)
    }
    fn wait_ids(&self) -> Vec<String> {
        self.wait_ids.lock().unwrap().clone()
    }
    fn remote_cancels(&self) -> usize {
        self.remote_cancels.load(Ordering::SeqCst)
    }
    fn job(&self, task_id: &str) -> Option<Job> {
        self.jobs.lock().unwrap().get(task_id).cloned()
    }
    /// Removes the adapter service from the process.
    async fn remove(&self) {
        let disposer = self.disposer.lock().unwrap().take();
        if let Some(disposer) = disposer {
            let _ = disposer.dispose().await;
        }
    }
    fn set_realtime_behavior(&self, behavior: RealtimeBehavior) {
        self.realtime.lock().unwrap().behavior = behavior;
    }
    fn realtime_opens(&self) -> usize {
        self.realtime.lock().unwrap().opens.len()
    }
    /// The newest open's channel, params, and the declaration Modality's
    /// connection carried.
    fn realtime_open(&self) -> (String, Value, Option<String>, Option<String>) {
        let state = self.realtime.lock().unwrap();
        let open = state.opens.last().expect("the fixture opened a session");
        (
            open.channel.clone(),
            open.params.clone(),
            open.protocol.clone(),
            open.path.clone(),
        )
    }
    fn realtime_frames(&self) -> Vec<(String, RealtimeFrame)> {
        self.realtime.lock().unwrap().frames.clone()
    }
    fn realtime_closes(&self) -> Vec<String> {
        self.realtime.lock().unwrap().closes.clone()
    }
    /// Emits one provider event on the newest session.
    fn emit(&self, event: Value) {
        let state = self.realtime.lock().unwrap();
        let open = state.opens.last().expect("the fixture opened a session");
        state.events[&open.channel]
            .try_send(RealtimeEvent::Event(event))
            .expect("the fixture's session accepts an event");
    }
    /// Ends the newest session from the provider's side.
    fn end(&self, end: RealtimeEnd) {
        let state = self.realtime.lock().unwrap();
        let open = state.opens.last().expect("the fixture opened a session");
        state.events[&open.channel]
            .try_send(RealtimeEvent::Ended(end))
            .expect("the fixture's session accepts its end");
    }
}
impl ModalityAdapter for FixtureAdapter {
    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }
    fn operations(&self, model: &ModelId) -> Vec<AdapterOperation> {
        let served = self.served.lock().unwrap();
        let operations: BTreeSet<String> = match served.as_ref() {
            Some(served) => served.get(&model.model_id).cloned().unwrap_or_default(),
            None => ["generate", "edit", "enhance"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        let schemas = self.schemas.lock().unwrap();
        operations
            .into_iter()
            .map(|id| {
                // A bare name is an image operation; a qualified id names its
                // category, as `realtime.voice` does.
                let (category, operation) = id.split_once('.').unwrap_or(("image", id.as_str()));
                AdapterOperation {
                    params_schema: schemas.get(&id).cloned().unwrap_or_else(|| json!({})),
                    category: category.to_owned(),
                    operation: operation.to_owned(),
                }
            })
            .collect()
    }
    fn submit(
        &self,
        job: Job,
        _connection: ModalityConnection,
        _cancel: CancellationToken,
    ) -> AdapterFuture<'_, Submitted> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let behavior = self.behavior();
        if let Some(error) = behavior.submit_error {
            return Box::pin(async move { Err(error) });
        }
        let task_id = job.task_id.clone();
        let handle = match &behavior.handles {
            Handles::PerTask => Some(format!("handle-{task_id}")),
            Handles::None => None,
        };
        match &handle {
            Some(handle) => {
                self.handles
                    .lock()
                    .unwrap()
                    .insert(handle.clone(), task_id.clone());
            }
            None => self.order.lock().unwrap().push(task_id.clone()),
        }
        self.jobs.lock().unwrap().insert(task_id, job);
        let submit_gate = behavior.submit_gate.clone();
        Box::pin(async move {
            if let Some(gate) = submit_gate {
                gate.cancelled().await;
            }
            Ok(Submitted {
                remote_handle: handle,
            })
        })
    }
    fn wait(
        &self,
        task_id: &str,
        handle: Option<&str>,
        _connection: ModalityConnection,
        progress: ProgressSink,
        _cancel: CancellationToken,
    ) -> AdapterFuture<'_, AdapterOutput> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        self.wait_ids.lock().unwrap().push(task_id.to_owned());
        let behavior = self.behavior();
        if let Some(handle) = handle {
            let known = self.handles.lock().unwrap().get(handle).cloned();
            if known.as_deref() != Some(task_id) {
                return Box::pin(async {
                    Err(AdapterError::provider_error(
                        "the fixture was given a handle of another job",
                    ))
                });
            }
        }
        let Some(job) = self.job(task_id) else {
            return Box::pin(async {
                Err(AdapterError::provider_error(
                    "the fixture knows no such job",
                ))
            });
        };
        let task_id = task_id.to_owned();
        let reverse = behavior.reverse_completion && handle.is_none();
        let this = self;
        Box::pin(async move {
            for update in behavior.progress {
                progress.send(update);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if let Some(gate) = &behavior.gate {
                gate.cancelled().await;
            }
            if reverse {
                // Complete later submissions before earlier ones.
                loop {
                    let notified = this.finished_notify.notified();
                    let blocked = {
                        let order = this.order.lock().unwrap();
                        let finished = this.finished.lock().unwrap();
                        order
                            .iter()
                            .position(|id| id == &task_id)
                            .is_some_and(|position| {
                                order[position + 1..]
                                    .iter()
                                    .any(|id| !finished.contains(id))
                            })
                    };
                    if !blocked {
                        break;
                    }
                    notified.await;
                }
            }
            if let Some(error) = behavior.wait_error {
                return Err(error);
            }
            if let Some(outside) = behavior.outside {
                return Ok(AdapterOutput {
                    files: vec![JobFile {
                        name: "outside.bin".into(),
                        media_type: "image/png".into(),
                        path: outside,
                    }],
                    data: None,
                });
            }
            if let Some((name, target)) = &behavior.link {
                let path = job.work_dir.join(name);
                let _ = std::fs::remove_file(&path);
                std::os::unix::fs::symlink(target, &path).expect("fixture link");
                return Ok(AdapterOutput {
                    files: vec![JobFile {
                        name: name.clone(),
                        media_type: "image/png".into(),
                        path,
                    }],
                    data: None,
                });
            }
            let files = if behavior.named_output {
                let name = "out.bin".to_owned();
                let bytes = task_id.clone().into_bytes();
                std::fs::write(job.work_dir.join(&name), &bytes).expect("fixture output");
                vec![JobFile {
                    name,
                    media_type: "image/png".into(),
                    path: job.work_dir.join("out.bin"),
                }]
            } else {
                let mut files = Vec::new();
                for (name, media_type, bytes) in behavior.files {
                    let path = job.work_dir.join(&name);
                    std::fs::write(&path, &bytes).expect("fixture output");
                    files.push(JobFile {
                        name,
                        media_type,
                        path,
                    });
                }
                files
            };
            if reverse {
                this.finished.lock().unwrap().insert(task_id.clone());
                this.finished_notify.notify_waiters();
            }
            Ok(AdapterOutput {
                files,
                data: behavior.data,
            })
        })
    }
    fn cancel_remote(
        &self,
        _handle: &str,
        _connection: ModalityConnection,
    ) -> AdapterFuture<'_, RemoteCancel> {
        self.remote_cancels.fetch_add(1, Ordering::SeqCst);
        let outcome = self.behavior().remote_cancel;
        Box::pin(async move { Ok(outcome) })
    }
    fn realtime_open(
        &self,
        connection: ModalityConnection,
        params: Value,
        events: tokio::sync::mpsc::Sender<RealtimeEvent>,
        _cancel: CancellationToken,
    ) -> AdapterFuture<'_, RealtimeOpened> {
        let behavior = self.realtime.lock().unwrap().behavior.clone();
        if let Some(error) = behavior.open_error {
            return Box::pin(async move { Err(error) });
        }
        let mut state = self.realtime.lock().unwrap();
        let channel = format!("chan-{}", state.opens.len() + 1);
        state.opens.push(RealtimeOpen {
            channel: channel.clone(),
            params,
            protocol: connection.realtime.as_ref().map(|r| r.protocol.clone()),
            path: connection.realtime.as_ref().map(|r| r.path.clone()),
        });
        state.events.insert(channel.clone(), events);
        let opened = if behavior.direct {
            RealtimeOpened {
                channel_id: channel,
                transport: RealtimeTransport::Direct,
                connection: Some(RealtimeConnection {
                    kind: "webrtc".into(),
                    url: Some("https://realtime.invalid/session".into()),
                    token: Some("ephemeral-session-token".into()),
                    expires_at: behavior.expires_in.map(|after| {
                        (chrono::Utc::now() + chrono::Duration::from_std(after).unwrap())
                            .to_rfc3339()
                    }),
                    ..Default::default()
                }),
            }
        } else {
            RealtimeOpened {
                channel_id: channel,
                transport: RealtimeTransport::Relay,
                connection: None,
            }
        };
        Box::pin(async move { Ok(opened) })
    }
    fn realtime_send(
        &self,
        channel_id: &str,
        frame: RealtimeFrame,
        _cancel: CancellationToken,
    ) -> AdapterFuture<'_, ()> {
        self.realtime
            .lock()
            .unwrap()
            .frames
            .push((channel_id.to_owned(), frame));
        Box::pin(async { Ok(()) })
    }
    fn realtime_close(&self, channel_id: &str) -> AdapterFuture<'_, ()> {
        self.realtime
            .lock()
            .unwrap()
            .closes
            .push(channel_id.to_owned());
        Box::pin(async { Ok(()) })
    }
}

struct FixturePlugin(Arc<FixtureAdapter>);
impl Plugin for FixturePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ModalityFixture"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let disposer = ctx.provide(ModalityAdapterService(self.0.clone()))?;
        *self.0.disposer.lock().unwrap() = Some(disposer);
        Ok(())
    }
}

const PROVIDER: &str = "018ec73f-0000-7000-8000-0000000000aa";

fn home(prefix: &str) -> std::path::PathBuf {
    let home = std::path::PathBuf::from("/tmp").join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

async fn create_provider(client: &mut Client) {
    let mut vision = CatalogCapabilities::with_limits(128_000, 16_384);
    vision.vision = true;
    let text = CatalogCapabilities::with_limits(128_000, 16_384);
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {"driverKind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "auth": {"type": "anonymous"}},
                    "models": [
                        {"modelId": "alpha", "displayName": "alpha", "status": "active", "capabilities": vision},
                        {"modelId": "beta", "displayName": "beta", "status": "active", "capabilities": text}
                    ],
                    "enabled": true
                }),
            )
            .await,
    );
}
fn model(model_id: &str) -> Value {
    json!({"providerId": PROVIDER, "modelId": model_id})
}
fn operation<'a>(snapshot: &'a Value, operation: &str) -> &'a Value {
    operation_in(snapshot, "image", operation)
}
fn operation_in<'a>(snapshot: &'a Value, category: &str, operation: &str) -> &'a Value {
    snapshot["result"]["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["category"].as_str() == Some(category))
        .unwrap()["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["operation"].as_str() == Some(operation))
        .unwrap()
}

/// One expected A.1 operation and its Settings key.
type ExpectedOperation = (&'static str, Option<&'static str>);

fn assert_operations(categories: &[Value], category: &str, expected: &[ExpectedOperation]) {
    let entry = categories
        .iter()
        .find(|entry| entry["category"].as_str() == Some(category))
        .unwrap();
    let operations: Vec<(String, Option<String>)> = entry["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|operation| {
            (
                operation["operation"].as_str().unwrap().to_owned(),
                operation["key"].as_str().map(str::to_owned),
            )
        })
        .collect();
    let wanted: Vec<(String, Option<String>)> = expected
        .iter()
        .map(|(operation, key)| ((*operation).to_owned(), key.map(str::to_owned)))
        .collect();
    assert_eq!(operations, wanted, "{category}");
}

/// A non-chat media model that declares modality operations.
fn media(model_id: &str, operations: &[&str]) -> Value {
    json!({
        "modelId": model_id, "displayName": model_id, "status": "active",
        "capabilities": {"modalities":["image"],"toolCalling":false,"streaming":true,
            "structuredOutput":false,"vision":false,"audio":false,"reasoning":false},
        "metadata": {"raw": {"type":"image-generation", "modalityCapabilities": operations}}
    })
}
/// A chat-selectable model that also declares modality operations.
fn chat(model_id: &str, operations: &[&str]) -> Value {
    let capabilities = CatalogCapabilities::with_limits(128_000, 16_384);
    json!({
        "modelId": model_id, "displayName": model_id, "status": "active",
        "capabilities": capabilities,
        "metadata": {"raw": {"modalityCapabilities": operations}}
    })
}
async fn create_provider_with(client: &mut Client, models: Vec<Value>) {
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {"driverKind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "auth": {"type": "anonymous"}},
                    "models": models,
                    "enabled": true
                }),
            )
            .await,
    );
}

async fn start(
    fixture: Option<Arc<FixtureAdapter>>,
    prefix: &str,
) -> (RunningServer, std::path::PathBuf) {
    let home = home(prefix);
    let server = start_in(home.clone(), fixture).await;
    (server, home)
}

async fn start_in(home: std::path::PathBuf, fixture: Option<Arc<FixtureAdapter>>) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(Arc::new(MockTools), config, None, |builder| {
        if let Some(fixture) = fixture.clone() {
            builder.plugin(move |_| Ok(Box::new(FixturePlugin(fixture.clone()))));
        }
    })
    .await
}

/// [`start_in`] with the clock a generation reads, for retention.
async fn start_in_with_clock(
    home: std::path::PathBuf,
    fixture: Option<Arc<FixtureAdapter>>,
    clock: Arc<dyn Clock>,
) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(
        Arc::new(MockTools),
        config,
        Some(InstallOptions {
            clock,
            ..Default::default()
        }),
        |builder| {
            if let Some(fixture) = fixture.clone() {
                builder.plugin(move |_| Ok(Box::new(FixturePlugin(fixture.clone()))));
            }
        },
    )
    .await
}

/// [`start_in`] with the realtime bounds a generation reads, so a lifetime
/// rule is observable in milliseconds.
async fn start_in_with_limits(
    home: std::path::PathBuf,
    fixture: Option<Arc<FixtureAdapter>>,
    realtime: RealtimeLimits,
) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(
        Arc::new(MockTools),
        config,
        Some(InstallOptions {
            realtime,
            ..Default::default()
        }),
        |builder| {
            if let Some(fixture) = fixture.clone() {
                builder.plugin(move |_| Ok(Box::new(FixturePlugin(fixture.clone()))));
            }
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capabilities_and_configure_resolve_defaults_through_the_fixture_adapter() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider(&mut client).await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "beta"}),
            )
            .await,
    );

    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let categories = initial["result"]["categories"].as_array().unwrap();
    assert_eq!(categories.len(), 9);
    assert!(
        categories
            .iter()
            .all(|category| category["category"] != "video" || category["available"] == false)
    );
    let video = categories
        .iter()
        .find(|category| category["category"] == "video")
        .unwrap();
    assert_eq!(video["reasons"][0]["dimension"], "build");
    assert!(
        initial["result"]["revision"]
            .as_str()
            .unwrap()
            .contains("modality")
    );

    let generate = operation(&initial, "generate");
    assert_eq!(generate["available"], true);
    assert_eq!(generate["key"], "modality.image.generate");
    assert!(generate["default"].is_null());
    assert_eq!(
        generate["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "alpha", "available": true},
            {"providerId": PROVIDER, "modelId": "beta", "available": true}
        ])
    );
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "beta", "source": "automatic"})
    );
    let understand = operation(&initial, "understand");
    assert_eq!(understand["available"], true);
    assert_eq!(
        understand["options"],
        json!([{"providerId": PROVIDER, "modelId": "alpha", "available": true}])
    );
    assert_eq!(
        understand["effective"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "source": "automatic"})
    );

    let revision = initial["result"]["revision"].as_str().unwrap().to_owned();
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "generate", "model": model("alpha")}),
        )
        .await;
    assert_ok(&configured);
    let generate = operation(&configured, "generate");
    assert_eq!(
        generate["default"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "available": true})
    );
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "source": "default"})
    );
    assert_eq!(
        operation(&configured, "edit")["effective"],
        json!({"providerId": PROVIDER, "modelId": "beta", "source": "automatic"}),
        "configuring one operation leaves the others on automatic"
    );
    let reread = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        configured["result"], reread["result"],
        "reply equals the next read"
    );
    let edit = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": reread["result"]["revision"], "category": "image", "operation": "edit", "model": model("beta")}),
        )
        .await;
    assert_ok(&edit);
    assert_eq!(
        operation(&edit, "edit")["effective"],
        json!({"providerId": PROVIDER, "modelId": "beta", "source": "default"})
    );

    let conflict = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "generate", "model": model("beta")}),
        )
        .await;
    assert_eq!(conflict["error"]["data"]["appError"], "modality_conflict");
    let stale = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        operation(&stale, "generate")["default"]["modelId"],
        "alpha",
        "a conflicted write changes nothing"
    );

    let unseen = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "generate", "model": model("gamma")}),
        )
        .await;
    assert_eq!(
        unseen["error"]["data"]["appError"], "modality_invalid_selection",
        "an unselectable model is rejected"
    );
    let not_vision = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "understand", "model": model("beta")}),
        )
        .await;
    assert_eq!(
        not_vision["error"]["data"]["appError"], "modality_invalid_selection",
        "understanding accepts only selectable vision models"
    );
    let unknown = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "video", "model": null}),
        )
        .await;
    assert_eq!(
        unknown["error"]["data"]["appError"],
        "modality_unknown_operation"
    );
    let other = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "video", "operation": "generate", "model": model("alpha")}),
        )
        .await;
    assert_eq!(
        other["error"]["data"]["appError"], "modality_invalid_selection",
        "configuring is a settings write; a model outside the video options is refused"
    );
    let missing_model = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "generate"}),
        )
        .await;
    assert_eq!(
        missing_model["error"]["data"]["appError"], "invalid_params",
        "an absent model is malformed; only null clears"
    );

    let catalog = client
        .rpc("provider.catalog", json!({"providerId": PROVIDER}))
        .await;
    assert_ok(&catalog);
    let version = catalog["result"]["catalog"]["version"].as_u64().unwrap();
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId": PROVIDER, "expectedVersion": version, "modelIds": ["beta"]}),
            )
            .await,
    );
    let removed = client.rpc("modality.capabilities", json!({})).await;
    let generate = operation(&removed, "generate");
    assert_eq!(generate["default"]["available"], false, "{generate}");
    assert!(
        generate["default"]["reason"]["detail"]
            .as_str()
            .unwrap()
            .contains("no longer selectable")
    );
    assert!(
        generate["effective"].is_null(),
        "an unavailable default never substitutes another model"
    );

    let cleared = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": removed["result"]["revision"], "category": "image", "operation": "generate", "model": null}),
        )
        .await;
    assert_ok(&cleared);
    let generate = operation(&cleared, "generate");
    assert!(generate["default"].is_null());
    assert_eq!(generate["effective"]["source"], "automatic");

    let edit_only = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": cleared["result"]["revision"], "category": "image", "operation": "edit", "model": model("alpha")}),
        )
        .await;
    assert_eq!(
        edit_only["error"]["data"]["appError"], "modality_invalid_selection",
        "a disabled model is not a valid default"
    );
    fixture.serve(&[("beta", &["generate"])]);
    let beta_edit = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": cleared["result"]["revision"], "category": "image", "operation": "edit", "model": model("beta")}),
        )
        .await;
    assert_eq!(
        beta_edit["error"]["data"]["appError"], "modality_invalid_selection",
        "the adapter serves edit only for the models it declares"
    );
    let beta_generate = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": cleared["result"]["revision"], "category": "image", "operation": "generate", "model": model("beta")}),
        )
        .await;
    assert_ok(&beta_generate);
    assert_eq!(
        operation(&beta_generate, "generate")["effective"]["modelId"],
        "beta",
        "selecting a nondefault model changes the model a task resolves"
    );
    assert!(
        !operation(&beta_generate, "edit")["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["available"] == true),
        "the adapter serves edit only for the models it declares"
    );
    let unserved = client.rpc("modality.capabilities", json!({})).await;
    let edit = operation(&unserved, "edit");
    assert_eq!(
        edit["default"]["available"], false,
        "a selectable default that no adapter serves is unavailable: {edit}"
    );
    assert_eq!(edit["default"]["reason"]["dimension"], "adapter");
    assert!(edit["effective"].is_null());

    fixture.set_available(false);
    let offline = client.rpc("modality.capabilities", json!({})).await;
    let generate = operation(&offline, "generate");
    assert_eq!(generate["available"], false);
    assert_eq!(generate["reasons"][0]["dimension"], "runtime");
    let refused = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": offline["result"]["revision"], "category": "image", "operation": "generate", "model": model("beta")}),
        )
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "modality_invalid_selection",
        "an unavailable adapter cannot serve a new default"
    );

    let mut bare = Client::new_version(&server, true, 3).await;
    for method in ["modality.capabilities", "modality.configure"] {
        assert_eq!(
            bare.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required",
            "{method}"
        );
    }

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The new Provider read exists for the media models execution rejects: when
/// an adapter serves one, the operation offers it instead of reporting that
/// no selectable model is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_media_model_served_by_the_adapter_is_an_option() {
    let fixture = Arc::new(FixtureAdapter::default());
    fixture.serve(&[("image-media", &["generate"])]);
    let (server, home) = start(Some(fixture), "dm-declared-adapter").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![media("image-media", &["image.generate"])]).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let generate = operation(&snapshot, "generate");
    assert_eq!(
        generate["options"],
        json!([{"providerId": PROVIDER, "modelId": "image-media", "available": true}]),
        "a declared model the adapter serves is an option without execution selectability: {generate}"
    );
    assert_eq!(generate["available"], true);
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "image-media", "source": "automatic"})
    );
    let understand = operation(&snapshot, "understand");
    assert!(
        understand["options"].as_array().unwrap().is_empty(),
        "the model is not in the execution read understanding uses: {understand}"
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": snapshot["result"]["revision"], "category": "image", "operation": "generate", "model": model("image-media")}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(
        operation(&configured, "generate")["effective"],
        json!({"providerId": PROVIDER, "modelId": "image-media", "source": "default"})
    );
    let edit = operation(&snapshot, "edit");
    assert_eq!(edit["available"], false);
    assert_eq!(
        edit["reasons"][0]["detail"], "No selectable model is served by an available adapter",
        "the adapter narrows the candidates per operation"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An adapter that serves no model either Provider read names is not the same
/// as a build without an adapter: the operation says which case it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_serving_no_provider_model_is_not_reported_as_missing() {
    let fixture = Arc::new(FixtureAdapter::default());
    fixture.serve(&[("unknown-model", &["generate"])]);
    let (server, home) = start(Some(fixture), "dm-adapter-idle").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![media("image-media", &["image.generate"])]).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let generate = operation(&snapshot, "generate");
    assert_eq!(generate["available"], false);
    assert_eq!(generate["reasons"][0]["dimension"], "adapter");
    assert_eq!(
        generate["reasons"][0]["detail"], "The adapter serves no image model Provider lists",
        "a collected adapter that serves no candidate is not a missing adapter: {generate}"
    );
    assert_eq!(
        generate["options"],
        json!([{"providerId": PROVIDER, "modelId": "image-media", "available": true}]),
        "an operation no adapter serves falls back to the declared models"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_product_build_reports_image_operations_without_an_adapter() {
    let (server, home) = start(None, "dm-product").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider(&mut client).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let categories = snapshot["result"]["categories"].as_array().unwrap();
    assert_eq!(categories.len(), 9, "every category is present");
    for category in categories
        .iter()
        .filter(|category| category["category"] != "image")
    {
        assert_eq!(category["available"], false, "{category}");
        assert_eq!(category["reasons"][0]["dimension"], "build", "{category}");
        assert!(
            category["operations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|operation| operation["reasons"][0]["dimension"] == "build"),
            "{category}"
        );
    }
    for name in ["generate", "edit", "enhance"] {
        let entry = operation(&snapshot, name);
        assert_eq!(entry["available"], false, "{entry}");
        assert_eq!(entry["reasons"][0]["dimension"], "adapter");
        assert!(entry["options"].as_array().unwrap().is_empty());
        assert!(entry["default"].is_null());
        assert!(entry["effective"].is_null());
        let refused = client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": snapshot["result"]["revision"], "category": "image", "operation": name, "model": model("alpha")}),
            )
            .await;
        assert_eq!(
            refused["error"]["data"]["appError"], "modality_invalid_selection",
            "{name} cannot have a default without an adapter: {refused}"
        );
    }
    let understand = operation(&snapshot, "understand");
    assert_eq!(
        understand["available"], true,
        "understanding resolves a selectable vision model"
    );
    assert_eq!(
        understand["options"],
        json!([{"providerId": PROVIDER, "modelId": "alpha", "available": true}])
    );
    assert_eq!(
        understand["effective"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "source": "automatic"})
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": snapshot["result"]["revision"], "category": "image", "operation": "understand", "model": model("alpha")}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(
        operation(&configured, "understand")["default"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "available": true})
    );
    assert_eq!(
        operation(&configured, "understand")["effective"]["source"],
        "default"
    );
    let reread = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(configured["result"], reread["result"]);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_an_operation_after_all_defaults_matches_the_next_read() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture), "dm-clear").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut vision = CatalogCapabilities::with_limits(128_000, 16_384);
    vision.vision = true;
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {"driverKind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "auth": {"type": "anonymous"}},
                    "models": [
                        {"modelId": "alpha", "displayName": "alpha", "status": "active", "capabilities": vision},
                        {"modelId": "gamma", "displayName": "gamma", "status": "active", "capabilities": vision}
                    ],
                    "enabled": true
                }),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "gamma"}),
            )
            .await,
    );
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let mut revision = initial["result"]["revision"].as_str().unwrap().to_owned();
    for operation in ["generate", "edit", "enhance", "understand"] {
        let configured = client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": revision, "category": "image", "operation": operation, "model": model("alpha")}),
            )
            .await;
        assert_ok(&configured);
        revision = configured["result"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
    }
    let cleared = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "understand", "model": null}),
        )
        .await;
    assert_ok(&cleared);
    assert_eq!(
        operation(&cleared, "understand")["effective"],
        json!({"providerId": PROVIDER, "modelId": "gamma", "source": "automatic"}),
        "the reply uses the global selection that serves the cleared operation"
    );
    let next = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        cleared["result"], next["result"],
        "reply equals the next read"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_default_survives_a_restart() {
    let home = home("dm-reload");
    let fixture = Arc::new(FixtureAdapter::default());
    let server = start_in(home.clone(), Some(fixture.clone())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider(&mut client).await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "beta"}),
            )
            .await,
    );
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let saved = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "image", "operation": "generate", "model": model("alpha")}),
        )
        .await;
    assert_ok(&saved);
    let revision = saved["result"]["revision"].as_str().unwrap().to_owned();
    server.shutdown();
    server.join().await.unwrap();

    let server = start_in(home.clone(), Some(fixture)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let restored = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&restored);
    assert_eq!(
        operation(&restored, "generate")["default"]["modelId"],
        "alpha",
        "the persisted default is read from the modality settings document"
    );
    assert_eq!(restored["result"]["revision"], revision);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_a1_category_and_operation_is_reported_with_its_default_key() {
    let (server, home) = start(None, "dm-catalog").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let categories = snapshot["result"]["categories"].as_array().unwrap();
    let names: Vec<Option<&str>> = categories
        .iter()
        .map(|category| category["category"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            Some("image"),
            Some("video"),
            Some("speech"),
            Some("music"),
            Some("audio"),
            Some("asset"),
            Some("motion"),
            Some("realtime"),
            Some("ocr"),
        ]
    );
    assert_operations(
        categories,
        "image",
        &[
            ("generate", Some("modality.image.generate")),
            ("edit", Some("modality.image.edit")),
            ("enhance", Some("modality.image.enhance")),
            ("understand", Some("modality.image.understand")),
        ],
    );
    assert_operations(
        categories,
        "video",
        &[
            ("generate", Some("modality.video")),
            ("edit", Some("modality.video")),
            ("enhance", Some("modality.video")),
        ],
    );
    assert_operations(
        categories,
        "speech",
        &[
            ("synthesize", Some("modality.speech.synthesize")),
            ("transcribe", Some("modality.speech.transcribe")),
        ],
    );
    for (category, operation, key) in [
        ("music", "generate", "modality.music"),
        ("audio", "generate", "modality.audio"),
        ("asset", "generate", "modality.asset"),
        ("motion", "generate", "modality.motion"),
    ] {
        assert_operations(categories, category, &[(operation, Some(key))]);
    }
    assert_operations(
        categories,
        "realtime",
        &[
            ("voice", Some("modality.realtime")),
            ("transcribe", Some("modality.realtime")),
            ("avatar", Some("modality.realtime")),
        ],
    );
    assert_operations(categories, "ocr", &[("recognize", Some("modality.ocr"))]);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_models_are_the_options_of_an_operation_without_an_adapter() {
    let (server, home) = start(None, "dm-declared").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(
        &mut client,
        vec![
            media("video-edit", &["video.edit"]),
            media("video-generate", &["video.generate", "video.edit"]),
            chat("text-model", &[]),
        ],
    )
    .await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "text-model"}),
            )
            .await,
    );
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let generate = operation_in(&snapshot, "video", "generate");
    assert_eq!(generate["available"], false);
    assert_eq!(generate["reasons"][0]["dimension"], "build");
    assert_eq!(
        generate["options"],
        json!([{"providerId": PROVIDER, "modelId": "video-generate", "available": true}]),
        "only a declared model is an option, in Provider's order"
    );
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "video-generate", "source": "automatic"}),
        "the global selection does not serve video, so the first declared option resolves"
    );
    let edit = operation_in(&snapshot, "video", "edit");
    assert_eq!(
        edit["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "video-edit", "available": true},
            {"providerId": PROVIDER, "modelId": "video-generate", "available": true}
        ])
    );
    let enhance = operation_in(&snapshot, "video", "enhance");
    assert!(enhance["options"].as_array().unwrap().is_empty());
    assert!(enhance["effective"].is_null());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defaults_for_category_keyed_and_per_operation_keys_survive_a_restart() {
    let home = home("dm-keys");
    let server = start_in(home.clone(), None).await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(
        &mut client,
        vec![
            media("video-a", &["video.generate"]),
            media("speech-a", &["speech.synthesize", "speech.transcribe"]),
        ],
    )
    .await;
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let video = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "model": model("video-a")}),
        )
        .await;
    assert_ok(&video);
    for operation in ["generate", "edit", "enhance"] {
        assert_eq!(
            operation_in(&video, "video", operation)["default"]["modelId"],
            "video-a",
            "the category key governs every video operation"
        );
    }
    let speech = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": video["result"]["revision"], "category": "speech", "operation": "synthesize", "model": model("speech-a")}),
        )
        .await;
    assert_ok(&speech);
    assert_eq!(
        operation_in(&speech, "speech", "synthesize")["default"]["modelId"],
        "speech-a"
    );
    assert!(
        operation_in(&speech, "speech", "transcribe")["default"].is_null(),
        "speech keys each operation"
    );
    let conflict = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "model": model("video-a")}),
        )
        .await;
    assert_eq!(
        conflict["error"]["data"]["appError"], "modality_conflict",
        "a stale revision writes nothing"
    );
    let outside = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": speech["result"]["revision"], "category": "video", "model": model("speech-a")}),
        )
        .await;
    assert_eq!(
        outside["error"]["data"]["appError"],
        "modality_invalid_selection"
    );
    assert!(
        outside["error"]["message"]
            .as_str()
            .unwrap()
            .contains("video"),
        "the refusal names the operation: {outside}"
    );
    let revision = speech["result"]["revision"].as_str().unwrap().to_owned();
    server.shutdown();
    server.join().await.unwrap();

    let server = start_in(home.clone(), None).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let restored = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&restored);
    assert_eq!(restored["result"]["revision"], revision);
    for operation in ["generate", "edit", "enhance"] {
        assert_eq!(
            operation_in(&restored, "video", operation)["default"]["modelId"],
            "video-a",
            "{operation}"
        );
    }
    assert_eq!(
        operation_in(&restored, "speech", "synthesize")["default"]["modelId"],
        "speech-a"
    );
    assert!(operation_in(&restored, "speech", "transcribe")["default"].is_null());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A model that declares only a later operation of a category-keyed category
/// is a valid default, so re-saving the category without `operation` keeps it
/// even though the earlier rows carry the persisted default as unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_category_wide_resave_accepts_the_default_of_its_last_operation() {
    let (server, home) = start(None, "dm-category-resave").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![media("video-edit", &["video.edit"])]).await;
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let edited = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "operation": "edit", "model": model("video-edit")}),
        )
        .await;
    assert_ok(&edited);
    let resaved = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": edited["result"]["revision"], "category": "video", "model": model("video-edit")}),
        )
        .await;
    assert_ok(&resaved);
    for operation in ["generate", "edit", "enhance"] {
        assert_eq!(
            operation_in(&resaved, "video", operation)["default"]["modelId"],
            "video-edit",
            "the category-wide save keeps the stored default: {operation}"
        );
    }
    let next = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        resaved["result"], next["result"],
        "reply equals the next read"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operations_without_a_default_key_or_unknown_answers_are_unknown() {
    let (server, home) = start(None, "dm-unknown").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let revision = snapshot["result"]["revision"].clone();
    for params in [
        json!({"expectedRevision": revision, "category": "ocr", "model": null}),
        json!({"expectedRevision": revision, "category": "video", "operation": "unknown", "model": null}),
        json!({"expectedRevision": revision, "category": "image", "model": null}),
        json!({"expectedRevision": revision, "category": "unknown", "operation": "generate", "model": null}),
    ] {
        let reply = client.rpc("modality.configure", params.clone()).await;
        assert_eq!(
            reply["error"]["data"]["appError"], "modality_unknown_operation",
            "{params}"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effective_resolves_over_declared_options_and_stays_null_for_an_unavailable_default() {
    let (server, home) = start(None, "dm-effective").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(
        &mut client,
        vec![
            media("media-option", &["video.generate"]),
            chat("chat-option", &["video.generate"]),
        ],
    )
    .await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "chat-option"}),
            )
            .await,
    );
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let generate = operation_in(&initial, "video", "generate");
    assert!(generate["default"].is_null());
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "chat-option", "source": "automatic"}),
        "automatic resolution prefers the global selection when it serves the operation"
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "operation": "generate", "model": model("chat-option")}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(
        operation_in(&configured, "video", "generate")["effective"]["source"],
        "default"
    );
    let catalog = client
        .rpc("provider.catalog", json!({"providerId": PROVIDER}))
        .await;
    assert_ok(&catalog);
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId": PROVIDER, "expectedVersion": catalog["result"]["catalog"]["version"], "modelIds": []}),
            )
            .await,
    );
    let disabled = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&disabled);
    let generate = operation_in(&disabled, "video", "generate");
    assert_eq!(generate["default"]["available"], false, "{generate}");
    assert_eq!(generate["default"]["reason"]["dimension"], "provider");
    assert!(
        generate["effective"].is_null(),
        "an unavailable default never substitutes another model"
    );
    assert_eq!(
        generate["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "media-option", "available": true},
            {"providerId": PROVIDER, "modelId": "chat-option", "available": false,
                "reason": {"dimension": "provider", "detail": "Model is no longer selectable"}}
        ]),
        "the declared media model stays; the persisted default follows as unavailable"
    );
    let cleared = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": disabled["result"]["revision"], "category": "video", "operation": "generate", "model": null}),
        )
        .await;
    assert_ok(&cleared);
    assert_eq!(
        operation_in(&cleared, "video", "generate")["effective"],
        json!({"providerId": PROVIDER, "modelId": "media-option", "source": "automatic"})
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

// ---------------------------------------------------------------------------
// The task layer: modality.task.* through the real methods transport.
// ---------------------------------------------------------------------------

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A provider whose models declare the image adapter operations, with the
/// global selection serving `image.generate`.
async fn create_task_provider(client: &mut Client) {
    create_provider_with(
        client,
        vec![
            chat("alpha", &["image.generate", "image.edit", "image.enhance"]),
            chat("beta", &["image.generate", "image.edit", "image.enhance"]),
        ],
    )
    .await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "alpha"}),
            )
            .await,
    );
}

fn state_of(task: &Value) -> &str {
    task["state"].as_str().unwrap()
}
fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_default()
}

async fn submit_task(client: &mut Client, params: Value) -> Value {
    let reply = client.rpc("modality.task.submit", params).await;
    assert_ok(&reply);
    reply["result"]["task"].clone()
}

async fn task_status(client: &mut Client, task_id: &str) -> Value {
    let reply = client
        .rpc("modality.task.status", json!({"task_id": task_id}))
        .await;
    assert_ok(&reply);
    reply["result"]["task"].clone()
}

async fn wait_state(client: &mut Client, task_id: &str, wanted: &[&str]) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let task = task_status(client, task_id).await;
        if wanted.contains(&state_of(&task)) {
            return task;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "task {task_id} never reached {wanted:?}: {task}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_handle(client: &mut Client, task_id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let task = task_status(client, task_id).await;
        if task["remote_handle"].is_string() {
            return task;
        }
        assert!(
            !matches!(
                state_of(&task),
                "succeeded" | "failed" | "cancelled" | "interrupted"
            ),
            "task {task_id} ended without a handle: {task}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "task {task_id} never recorded a handle: {task}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_terminal(client: &mut Client, task_id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let reply = client
            .rpc(
                "modality.task.wait",
                json!({"task_id": task_id, "timeout_ms": 1000}),
            )
            .await;
        assert_ok(&reply);
        let task = reply["result"]["task"].clone();
        if !matches!(state_of(&task), "queued" | "running") {
            return task;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "task {task_id} never finished: {task}"
        );
    }
}

fn inline_file(name: &str, media_type: &str, bytes: &[u8]) -> Value {
    json!({"name": name, "media_type": media_type, "data_base64": STANDARD.encode(bytes)})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_run_through_the_methods_from_submit_to_result() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-task").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;

    let input = b"inline image".to_vec();
    let submitted = submit_task(
        &mut client,
        json!({
            "category": "image", "operation": "generate",
            "params": {"prompt": "a cat"},
            "files": [inline_file("in.png", "image/png", &input)],
            "client_task_id": "client-1"
        }),
    )
    .await;
    assert_eq!(state_of(&submitted), "queued");
    assert_eq!(submitted["executor"]["kind"], "provider");
    assert_eq!(
        submitted["executor"]["provider"]["modelId"], "alpha",
        "no model, no default: the global selection serves the operation"
    );
    assert_eq!(submitted["params"]["files"][0]["name"], "in.png");
    assert_eq!(submitted["params"]["files"][0]["bytes"], input.len());
    assert_eq!(submitted["params"]["files"][0]["sha256"], sha256(&input));
    let task_id = submitted["task_id"].as_str().unwrap().to_owned();

    let finished = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&finished), "succeeded", "{finished}");
    let file = finished["result"]["files"][0].clone();
    assert_eq!(file["name"], "out.bin");
    assert_eq!(file["bytes"], 14);
    assert_eq!(file["sha256"], sha256(b"fixture result"));
    assert_eq!(task_status(&mut client, &task_id).await, finished);

    let job = fixture.job(&task_id).expect("the adapter saw the job");
    assert_eq!(job.category, "image");
    assert_eq!(job.operation, "generate");
    assert_eq!(job.model.model_id, "alpha");
    assert_eq!(job.params["prompt"], "a cat");
    assert!(job.files[0].path.is_absolute());
    assert!(
        job.files[0]
            .path
            .ends_with("modality/tasks/".to_owned() + &task_id + "/input/in.png"),
        "{:?}",
        job.files[0].path
    );
    assert!(job.work_dir.ends_with("work"));

    // Chunked result through the streaming read.
    let first = client
        .rpc(
            "modality.task.result",
            json!({"task_id": task_id, "name": "out.bin", "offset": 0, "length": 7}),
        )
        .await;
    assert_ok(&first);
    assert_eq!(first["result"]["eof"], false);
    let second = client
        .rpc(
            "modality.task.result",
            json!({"task_id": task_id, "name": "out.bin", "offset": 7, "length": 100}),
        )
        .await;
    assert_ok(&second);
    assert_eq!(second["result"]["eof"], true);
    let mut data = STANDARD
        .decode(first["result"]["data_base64"].as_str().unwrap())
        .unwrap();
    data.extend(
        STANDARD
            .decode(second["result"]["data_base64"].as_str().unwrap())
            .unwrap(),
    );
    assert_eq!(data, b"fixture result");
    assert_eq!(sha256(&data), file["sha256"]);
    let beyond = client
        .rpc(
            "modality.task.result",
            json!({"task_id": task_id, "name": "out.bin", "offset": 0, "length": 5 * 1024 * 1024}),
        )
        .await;
    assert_eq!(app_error(&beyond), "invalid_input");

    // A duplicate client task id returns the same task.
    let duplicate = submit_task(
        &mut client,
        json!({
            "category": "image", "operation": "generate",
            "params": {"prompt": "a cat"},
            "files": [inline_file("in.png", "image/png", &input)],
            "client_task_id": "client-1"
        }),
    )
    .await;
    assert_eq!(duplicate["task_id"], task_id);
    assert_eq!(
        fixture.submits(),
        1,
        "the duplicate never reached the adapter"
    );

    // Newest first, one page at a time.
    for index in 0..2 {
        let task = submit_task(
            &mut client,
            json!({
                "category": "image", "operation": "edit",
                "params": {"prompt": format!("edit {index}")}
            }),
        )
        .await;
        wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await;
    }
    let page = client.rpc("modality.task.list", json!({"limit": 2})).await;
    assert_ok(&page);
    assert_eq!(page["result"]["tasks"].as_array().unwrap().len(), 2);
    let next = page["result"]["next"].as_str().unwrap().to_owned();
    let rest = client
        .rpc("modality.task.list", json!({"limit": 2, "after": next}))
        .await;
    assert_ok(&rest);
    assert_eq!(rest["result"]["tasks"].as_array().unwrap().len(), 1);
    assert!(rest["result"].get("next").is_none());
    let all = client
        .rpc("modality.task.list", json!({"state": "succeeded"}))
        .await;
    assert_eq!(all["result"]["tasks"].as_array().unwrap().len(), 3);
    let none = client
        .rpc("modality.task.list", json!({"state": "failed"}))
        .await;
    assert!(none["result"]["tasks"].as_array().unwrap().is_empty());
    let too_many = client
        .rpc("modality.task.list", json!({"limit": 101}))
        .await;
    assert_eq!(app_error(&too_many), "invalid_input");

    // A task keeps the model it resolved after the default changes.
    let capabilities = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(
        &client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": capabilities["result"]["revision"],
                    "category": "image", "operation": "generate", "model": model("beta")}),
            )
            .await,
    );
    let beta_task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "b"}}),
    )
    .await;
    assert_eq!(beta_task["executor"]["provider"]["modelId"], "beta");
    let beta_id = beta_task["task_id"].as_str().unwrap().to_owned();
    let beta_done = wait_terminal(&mut client, &beta_id).await;
    assert_eq!(state_of(&beta_done), "succeeded");
    assert_eq!(
        beta_done["executor"]["provider"]["modelId"], "beta",
        "the record keeps its resolved model"
    );
    let reread = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(
        &client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": reread["result"]["revision"],
                    "category": "image", "operation": "generate", "model": model("alpha")}),
            )
            .await,
    );
    assert_eq!(
        task_status(&mut client, &beta_id).await["executor"]["provider"]["modelId"],
        "beta",
        "changing the default later never rewrites a task"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fifth_task_of_a_category_waits_queued() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-queue").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let mut ids = Vec::new();
    for _ in 0..5 {
        let task = submit_task(
            &mut client,
            json!({"category": "image", "operation": "generate", "params": {"prompt": "q"}}),
        )
        .await;
        ids.push(task["task_id"].as_str().unwrap().to_owned());
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let states: Vec<String> = loop {
        let mut states = Vec::new();
        for id in &ids {
            states.push(state_of(&task_status(&mut client, id).await).to_owned());
        }
        if states.iter().filter(|state| *state == "running").count() == 4
            && states.iter().any(|state| state == "queued")
        {
            break states;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the queue never settled: {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(
        states,
        ["running", "running", "running", "running", "queued"],
        "four run, the fifth waits in submission order"
    );
    assert_eq!(fixture.submits(), 4);
    gate.cancel();
    for id in &ids {
        assert_eq!(state_of(&wait_terminal(&mut client, id).await), "succeeded");
    }
    assert_eq!(fixture.submits(), 5);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_covers_before_start_a_run_and_a_terminal_task() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-cancel").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;

    // Before start: four tasks hold the category, the fifth is cancelled
    // without contacting the executor.
    let mut ids = Vec::new();
    for _ in 0..5 {
        let task = submit_task(
            &mut client,
            json!({"category": "image", "operation": "generate", "params": {"prompt": "c"}}),
        )
        .await;
        ids.push(task["task_id"].as_str().unwrap().to_owned());
    }
    let queued = wait_state(&mut client, &ids[4], &["queued"]).await;
    assert_eq!(state_of(&queued), "queued");
    let cancelled = client
        .rpc("modality.task.cancel", json!({"task_id": ids[4]}))
        .await;
    assert_ok(&cancelled);
    assert_eq!(cancelled["result"]["task"]["state"], "cancelled");
    assert_eq!(
        fixture.submits(),
        4,
        "a cancelled queued task never reaches the adapter"
    );
    gate.cancel();
    for id in &ids[..4] {
        assert_eq!(state_of(&wait_terminal(&mut client, id).await), "succeeded");
    }

    // During a run with no handle: the local abort ends it, and no remote
    // cancel is attempted.
    let gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        handles: Handles::None,
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "r"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    wait_state(&mut client, &task_id, &["running"]).await;
    let cancelled = client
        .rpc("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    assert_ok(&cancelled);
    let record = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&record), "cancelled", "{record}");
    assert!(record["remote_cancel"].is_null());
    assert_eq!(fixture.remote_cancels(), 0);

    // During a run with a handle: the executor is asked, and its answer is
    // recorded.
    for (outcome, wanted) in [
        (RemoteCancel::Cancelled, "cancelled"),
        (RemoteCancel::Unsupported, "unsupported"),
    ] {
        let gate = CancellationToken::new();
        fixture.set_behavior(Behavior {
            gate: Some(gate.clone()),
            remote_cancel: outcome,
            ..Default::default()
        });
        let task = submit_task(
            &mut client,
            json!({"category": "image", "operation": "generate", "params": {"prompt": "h"}}),
        )
        .await;
        let task_id = task["task_id"].as_str().unwrap().to_owned();
        wait_handle(&mut client, &task_id).await;
        let cancelled = client
            .rpc("modality.task.cancel", json!({"task_id": task_id}))
            .await;
        assert_ok(&cancelled);
        let record = wait_terminal(&mut client, &task_id).await;
        assert_eq!(state_of(&record), "cancelled", "{record}");
        assert_eq!(record["remote_cancel"], wanted);
    }
    assert_eq!(fixture.remote_cancels(), 2);

    // On a terminal task: the record is unchanged.
    fixture.set_behavior(Behavior::default());
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "t"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let finished = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&finished), "succeeded");
    let again = client
        .rpc("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    assert_ok(&again);
    assert_eq!(again["result"]["task"], finished);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_racing_completion_yields_one_terminal_state() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    let files: Vec<(String, String, Vec<u8>)> = (0..400)
        .map(|index| {
            (
                format!("f{index:03}.bin"),
                "image/png".to_owned(),
                vec![7u8; 1024],
            )
        })
        .collect();
    fixture.set_behavior(Behavior {
        gate: Some(gate.clone()),
        files,
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-race").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "race"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    wait_state(&mut client, &task_id, &["running"]).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fixture.waits() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter was never asked to wait"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Let the completion start streaming its 400 files, then cancel while it
    // is writing them: the cancel's terminal write wins.
    gate.cancel();
    let result_dir = home.join(format!("modality/tasks/{task_id}/result"));
    let count_results = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                    .count()
            })
            .unwrap_or(0)
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while count_results(&result_dir) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the completion never started streaming"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let cancelled = client
        .rpc("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    assert_ok(&cancelled);
    assert_eq!(
        state_of(&cancelled["result"]["task"]),
        "cancelled",
        "{cancelled}"
    );
    let at_settled = count_results(&result_dir);
    let record = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&record), "cancelled", "{record}");
    assert!(
        record["result"].is_null(),
        "the losing completion wrote no result: {record}"
    );
    assert_eq!(record["remote_cancel"], "cancelled");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let leftovers = count_results(&result_dir);
        // The cancel replies only after `settled_cancelled` returned, and
        // `finish` observes the terminal record before it returns, so the
        // mirror is already terminal when the reply arrives. The copier
        // checks that mirror before opening each file and copies one at a
        // time, so past the reply it can add at most the one file it had
        // already opened.
        assert!(
            leftovers <= at_settled + 1,
            "the losing completion kept copying: {leftovers} files seen, {at_settled} when the cancel settled"
        );
        if leftovers == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the discarded outcome removed its files: {leftovers} left"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let again = client
        .rpc("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    assert_eq!(again["result"]["task"], record, "one terminal state");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_duplicate_client_task_ids_create_one_task() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-duplicate").await;
    let mut first = Client::new_version(&server, false, 3).await;
    let mut second = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut first).await;
    let params = json!({
        "category": "image", "operation": "generate",
        "params": {"prompt": "same"}, "client_task_id": "same-id"
    });
    let one = first.send("modality.task.submit", params.clone()).await;
    let two = second.send("modality.task.submit", params).await;
    let one = first.response(one).await;
    let two = second.response(two).await;
    assert_ok(&one);
    assert_ok(&two);
    assert_eq!(
        one["result"]["task"]["task_id"], two["result"]["task"]["task_id"],
        "two concurrent submissions are one task"
    );
    let list = first.rpc("modality.task.list", json!({})).await;
    assert_ok(&list);
    assert_eq!(list["result"]["tasks"].as_array().unwrap().len(), 1);
    wait_terminal(
        &mut first,
        one["result"]["task"]["task_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(fixture.submits(), 1, "one task reached the adapter once");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_adapter_makes_a_new_submit_unavailable() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-removed").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "a"}}),
    )
    .await;
    assert_eq!(
        state_of(&wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await),
        "succeeded"
    );
    fixture.remove().await;
    let refused = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate",
                "params": {"prompt": "b"}, "model": model("alpha")}),
        )
        .await;
    assert_eq!(
        app_error(&refused),
        "unavailable",
        "a removed adapter makes a new submit unavailable: {refused}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submission_boundaries_refuse_invalid_inputs() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-boundaries").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;

    let allowed = vec![0u8; 8 * 1024 * 1024];
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "big"},
            "files": [inline_file("big.png", "image/png", &allowed)]}),
    )
    .await;
    wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await;
    let over = vec![0u8; 8 * 1024 * 1024 + 1];
    let refused = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "big"},
                "files": [inline_file("big.png", "image/png", &over)]}),
        )
        .await;
    assert_eq!(
        app_error(&refused),
        "invalid_input",
        "one byte over the inline limit is refused: {refused}"
    );

    let wrong_type = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "v"},
                "files": [inline_file("clip.mp4", "video/mp4", b"x")]}),
        )
        .await;
    assert_eq!(app_error(&wrong_type), "invalid_input");

    let duplicate_names = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "d"},
                "files": [inline_file("a.png", "image/png", b"x"),
                    inline_file("a.png", "image/png", b"y")]}),
        )
        .await;
    assert_eq!(app_error(&duplicate_names), "invalid_input");

    fixture.set_schema(
        "generate",
        json!({"type": "object", "required": ["prompt"],
            "properties": {"prompt": {"type": "string"}},
            "additionalProperties": false}),
    );
    let tasks_dir = home.join("modality/tasks");
    let before = std::fs::read_dir(&tasks_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    let invalid = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {}}),
        )
        .await;
    assert_eq!(
        app_error(&invalid),
        "invalid_input",
        "parameters are checked against the adapter's schema: {invalid}"
    );
    let after = std::fs::read_dir(&tasks_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(before, after, "an invalid submission writes nothing");

    // `node_path` is accepted for a provider operation.
    let node_path = home.join("node.png");
    std::fs::write(&node_path, b"node bytes").unwrap();
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "n"},
            "files": [{"name": "n.png", "media_type": "image/png",
                "node_path": node_path.to_str().unwrap()}]}),
    )
    .await;
    let canonical = std::fs::canonicalize(&node_path).unwrap();
    let meta = &task["params"]["files"][0];
    assert_eq!(meta["node_path"], canonical.to_str().unwrap());
    assert_eq!(meta["bytes"], 10);
    assert_eq!(meta["sha256"], sha256(b"node bytes"));
    assert_eq!(
        state_of(&wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await),
        "succeeded"
    );
    let job = fixture.job(task["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        job.files[0].path, canonical,
        "the executor reads the canonical path; Modality never copies it"
    );

    let relative = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "r"},
                "files": [{"name": "r.png", "media_type": "image/png", "node_path": "rel.png"}]}),
        )
        .await;
    assert_eq!(app_error(&relative), "invalid_input");
    #[cfg(unix)]
    {
        let link = home.join("link.png");
        std::os::unix::fs::symlink(&node_path, &link).unwrap();
        let refused = client
            .rpc(
                "modality.task.submit",
                json!({"category": "image", "operation": "generate", "params": {"prompt": "l"},
                    "files": [{"name": "l.png", "media_type": "image/png",
                        "node_path": link.to_str().unwrap()}]}),
            )
            .await;
        assert_eq!(
            app_error(&refused),
            "invalid_input",
            "a link is not a node_path"
        );
    }
    let directory = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "d"},
                "files": [{"name": "d.png", "media_type": "image/png",
                    "node_path": home.to_str().unwrap()}]}),
        )
        .await;
    assert_eq!(app_error(&directory), "invalid_input");

    let node = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "n"},
                "node": "control"}),
        )
        .await;
    assert_eq!(
        app_error(&node),
        "invalid_input",
        "node is refused for a provider category"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_result_outside_the_work_directory_fails_the_task() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-outside").await;
    let outside = home.join("outside.bin");
    std::fs::write(&outside, b"stolen").unwrap();
    fixture.set_behavior(Behavior {
        outside: Some(outside),
        ..Default::default()
    });
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "o"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let record = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&record), "failed", "{record}");
    assert_eq!(record["error"]["code"], "provider_error");
    assert!(record["result"].is_null(), "no result is written");
    let result_dir = home.join(format!("modality/tasks/{task_id}/result"));
    let leftovers = std::fs::read_dir(&result_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_result_that_is_a_link_fails_the_task() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-link").await;
    let outside = home.join("outside.bin");
    std::fs::write(&outside, b"stolen").unwrap();
    fixture.set_behavior(Behavior {
        link: Some(("out.bin".into(), outside)),
        ..Default::default()
    });
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "o"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let record = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&record), "failed", "{record}");
    assert_eq!(record["error"]["code"], "provider_error", "{record}");
    assert!(record["result"].is_null(), "no result is written");
    assert!(!record.to_string().contains("stolen"), "{record}");
    let result_dir = home.join(format!("modality/tasks/{task_id}/result"));
    let leftovers = std::fs::read_dir(&result_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_is_streamed_to_wait_and_written_at_most_once_per_second() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    let progress: Vec<AdapterProgress> = (0..10)
        .map(|index| AdapterProgress {
            fraction: Some(index as f64 / 10.0),
            message: Some(format!("step {index}")),
        })
        .collect();
    fixture.set_behavior(Behavior {
        handles: Handles::None,
        progress,
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-progress").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "p"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let id = client
        .send(
            "modality.task.wait",
            json!({"task_id": task_id, "timeout_ms": 400}),
        )
        .await;
    let mut notices = Vec::new();
    loop {
        let value = client.read("wait progress").await;
        if value["method"] != "management.progress" {
            client.notices.push(value);
            continue;
        }
        assert_eq!(value["params"]["id"], id);
        notices.push(value["params"]["data"].clone());
        if notices.len() == 2 {
            break;
        }
    }
    assert!(
        notices.iter().all(|notice| notice["message"].is_string()),
        "progress carries fraction and message: {notices:?}"
    );
    let reply = client.response(id).await;
    assert_ok(&reply);
    assert_eq!(
        state_of(&reply["result"]["task"]),
        "running",
        "a timeout is not an error and the task keeps running"
    );
    let record = task_status(&mut client, &task_id).await;
    assert!(
        record["progress"].is_object(),
        "progress is persisted: {record}"
    );
    assert!(
        record["revision"].as_u64().unwrap() <= 3,
        "ten reports in 200 ms are written once: {record}"
    );
    gate.cancel();
    assert_eq!(
        state_of(&wait_terminal(&mut client, &task_id).await),
        "succeeded"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An operation the evaluation marks unavailable is refused with the
/// listing's own reasons, before model resolution: the `build` dimension
/// answers `modality_unavailable`, anything else `unavailable`, and no task is
/// written. An operation it marks available keeps the model path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_operation_answers_with_the_listings_reasons() {
    let home = home("dm-unavailable");
    let server = start_in(home.clone(), None).await;
    let mut client = Client::new_version(&server, false, 3).await;
    // `image` declares its operation, so only the adapter is missing; `music`
    // declares nothing, so its model resolution has no answer either.
    create_provider_with(&mut client, vec![chat("alpha", &["image.generate"])]).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    for (category, operation, code) in [
        // No adapter in this build: the operation itself is `adapter`.
        ("image", "generate", "unavailable"),
        // Nothing implements music: the reason is `build`.
        ("music", "generate", "modality_unavailable"),
    ] {
        let entry = operation_in(&snapshot, category, operation);
        assert_eq!(entry["available"], false, "{entry}");
        let id = format!("{category}.{operation}");
        let refused = client
            .rpc(
                "modality.task.submit",
                json!({"category": category, "operation": operation, "params": {"prompt": "x"}}),
            )
            .await;
        assert_eq!(app_error(&refused), code, "{id}: {refused}");
        let message = refused["error"]["message"].as_str().unwrap();
        let mut lines = message.lines();
        assert_eq!(
            lines.next(),
            Some(format!("{id} is unavailable.").as_str()),
            "{message}"
        );
        for reason in entry["reasons"].as_array().unwrap() {
            let line = format!(
                "{}: {}",
                reason["dimension"].as_str().unwrap(),
                reason["detail"].as_str().unwrap()
            );
            assert!(
                message.lines().any(|found| found == line),
                "{id} answers without `{line}`: {message}"
            );
        }
    }
    let list = client.rpc("modality.task.list", json!({})).await;
    assert_ok(&list);
    assert!(
        list["result"]["tasks"].as_array().unwrap().is_empty(),
        "a refused submission writes no task: {list}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An operation the evaluation marks available keeps the model path: with no
/// configured default the task resolves one automatically, so the
/// availability check refuses only what the listing refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_available_operation_still_resolves_its_model() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture), "dm-available").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![chat("alpha", &["image.generate"])]).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "a"}}),
    )
    .await;
    assert_eq!(
        task["executor"]["provider"]["modelId"], "alpha",
        "the task resolved its model: {task}"
    );
    assert_eq!(
        state_of(&wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await),
        "succeeded"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_default_fails_submit() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture), "dm-default").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let capabilities = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(
        &client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": capabilities["result"]["revision"],
                    "category": "image", "operation": "generate", "model": model("beta")}),
            )
            .await,
    );
    let catalog = client
        .rpc("provider.catalog", json!({"providerId": PROVIDER}))
        .await;
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId": PROVIDER,
                    "expectedVersion": catalog["result"]["catalog"]["version"],
                    "modelIds": ["alpha"]}),
            )
            .await,
    );
    let refused = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "d"}}),
        )
        .await;
    assert_eq!(
        app_error(&refused),
        "default_unavailable",
        "an unavailable persisted default fails the task: {refused}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operations_that_are_not_tasks_answer_for_themselves() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture), "dm-nontask").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    for (category, operation, code) in [
        ("image", "understand", "modality_unknown_operation"),
        ("realtime", "voice", "modality_unknown_operation"),
        ("realtime", "transcribe", "modality_unknown_operation"),
        ("realtime", "avatar", "modality_unknown_operation"),
    ] {
        let refused = client
            .rpc(
                "modality.task.submit",
                json!({"category": category, "operation": operation, "params": {}}),
            )
            .await;
        assert_eq!(app_error(&refused), code, "{category}.{operation}");
    }
    let unknown = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "unknown", "params": {}}),
        )
        .await;
    assert_eq!(app_error(&unknown), "invalid_input");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A non-chat model that declares one realtime operation, and its transport
/// unless `declared` is false.
fn realtime_model(model_id: &str, operation: &str, declared: bool) -> Value {
    let mut raw = json!({"modalityCapabilities": [format!("realtime.{operation}")]});
    if declared {
        raw["realtime"] = json!({"protocol": "openai-realtime", "path": "/realtime"});
    }
    json!({
        "modelId": model_id, "displayName": model_id, "status": "active",
        "capabilities": {"modalities":["audio"],"toolCalling":false,"streaming":true,
            "structuredOutput":false,"vision":false,"audio":true,"reasoning":false},
        "metadata": {"raw": raw}
    })
}

/// A provider with a bearer credential and the models a realtime flow needs.
async fn create_realtime_provider(client: &mut Client, models: Vec<Value>) {
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {
                        "driverKind": "openai-compatible",
                        "baseUrl": "http://127.0.0.1:1/v1",
                        "auth": {"type": "bearer"},
                        "credential": {"action": "set", "value": REALTIME_CREDENTIAL}
                    },
                    "models": models,
                    "enabled": true
                }),
            )
            .await,
    );
}

const REALTIME_CREDENTIAL: &str = "sk-realtime-secret-key";

/// The next notification `method` names, buffering every other message.
async fn notice(client: &mut Client, method: &str) -> Value {
    if let Some(index) = client
        .notices
        .iter()
        .position(|value| value["method"] == method)
    {
        return client.notices.remove(index);
    }
    loop {
        let value = client.read(method).await;
        if value["method"] == method {
            return value;
        }
        client.notices.push(value);
    }
}

/// Opens one session and returns the reply's result.
async fn open_realtime(client: &mut Client, operation: &str, params: Value) -> Value {
    let reply = client
        .rpc(
            "modality.realtime.open",
            json!({"operation": operation, "model": model("voice"), "params": params}),
        )
        .await;
    assert_ok(&reply);
    reply["result"].clone()
}

/// A relay fixture that serves `realtime.voice` for the `voice` model with a
/// params schema that accepts nothing (D7).
fn realtime_fixture() -> Arc<FixtureAdapter> {
    let fixture = Arc::new(FixtureAdapter::default());
    fixture.serve(&[
        ("voice", &["realtime.voice"]),
        ("undeclared", &["realtime.voice"]),
    ]);
    fixture.set_schema(
        "realtime.voice",
        json!({"type": "object", "maxProperties": 0}),
    );
    fixture
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_realtime_session_relays_frames_and_events_to_its_owner() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    let opened = open_realtime(&mut client, "voice", json!({})).await;
    let id = opened["connectionId"].as_str().unwrap().to_owned();
    assert!(id.starts_with("rc_"));
    assert_eq!(opened["transport"], "relay");
    assert!(
        opened.get("connection").is_none(),
        "a relay session has no connection object"
    );
    let (channel, params, protocol, path) = fixture.realtime_open();
    assert_eq!(params, json!({}), "the relay's params are empty (D7)");
    assert_eq!(protocol.as_deref(), Some("openai-realtime"));
    assert_eq!(path.as_deref(), Some("/realtime"));

    let sent = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "session.update", "session": {"instructions": "hi"}}}}),
        )
        .await;
    assert_ok(&sent);
    let sent = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "audio", "data": "aGk="}}),
        )
        .await;
    assert_ok(&sent);
    let frames = fixture.realtime_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].1.kind, dim_modality::RealtimeFrameKind::Text);
    assert_eq!(frames[0].1.data["type"], "session.update");
    assert_eq!(frames[1].1.kind, dim_modality::RealtimeFrameKind::Audio);
    assert_eq!(frames[1].1.data, "aGk=", "an audio frame's data is base64");

    fixture.emit(json!({"type": "response.text.delta", "delta": "hello"}));
    let event = notice(&mut client, "modality.realtime.event").await;
    assert_eq!(event["params"]["connectionId"], id);
    assert_eq!(
        event["params"]["event"],
        json!({"type": "response.text.delta", "delta": "hello"})
    );

    let closed = client
        .rpc("modality.realtime.close", json!({"connectionId": id}))
        .await;
    assert_ok(&closed);
    let ended = notice(&mut client, "modality.realtime.ended").await;
    assert_eq!(ended["params"]["connectionId"], id);
    assert_eq!(ended["params"]["reason"], "closed");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while fixture.realtime_closes().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter was not closed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fixture.realtime_closes(),
        [channel],
        "the adapter saw its channel close"
    );
    for value in client.notices.iter().chain([&ended, &opened, &closed]) {
        assert!(
            !value.to_string().contains(REALTIME_CREDENTIAL),
            "no reply or notice carries the credential: {value}"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_realtime_session_ends_with_its_opening_connection() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-drop").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    open_realtime(&mut client, "voice", json!({})).await;
    drop(client);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while fixture.realtime_closes().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the dropped connection did not end its session"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_provider_ending_a_realtime_session_is_reported_once() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-provider").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    let opened = open_realtime(&mut client, "voice", json!({})).await;
    let id = opened["connectionId"].as_str().unwrap().to_owned();
    fixture.end(RealtimeEnd::Provider);
    let ended = notice(&mut client, "modality.realtime.ended").await;
    assert_eq!(ended["params"]["reason"], "provider");
    let refused = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "x"}}}),
        )
        .await;
    assert_eq!(app_error(&refused), "not_found", "an ended session is gone");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_realtime_frame_is_refused_and_the_session_stays() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-frame").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    let opened = open_realtime(&mut client, "voice", json!({})).await;
    let id = opened["connectionId"].as_str().unwrap().to_owned();
    let oversized = "x".repeat(1024 * 1024 + 1024);
    let refused = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "input_audio_buffer.append", "padding": oversized}}}),
        )
        .await;
    assert_eq!(app_error(&refused), "invalid_input");
    assert!(
        fixture.realtime_frames().is_empty(),
        "the adapter saw nothing"
    );
    let sent = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "still.here"}}}),
        )
        .await;
    assert_ok(&sent);
    assert_eq!(
        fixture.realtime_frames().len(),
        1,
        "the session stayed open"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fifth_realtime_session_is_refused_before_the_adapter_is_called() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-cap").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    let mut ids = Vec::new();
    for _ in 0..4 {
        let opened = open_realtime(&mut client, "voice", json!({})).await;
        ids.push(opened["connectionId"].as_str().unwrap().to_owned());
    }
    let refused = client
        .rpc(
            "modality.realtime.open",
            json!({"operation": "voice", "model": model("voice"), "params": {}}),
        )
        .await;
    assert_eq!(app_error(&refused), "realtime_limit");
    assert_eq!(
        fixture.realtime_opens(),
        4,
        "the fifth never reached the adapter"
    );
    let closed = client
        .rpc("modality.realtime.close", json!({"connectionId": ids[0]}))
        .await;
    assert_ok(&closed);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let opened = client
            .rpc(
                "modality.realtime.open",
                json!({"operation": "voice", "model": model("voice"), "params": {}}),
            )
            .await;
        if opened.get("error").is_none() {
            break;
        }
        assert_eq!(app_error(&opened), "realtime_limit");
        assert!(
            std::time::Instant::now() < deadline,
            "the closed slot never freed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_realtime_session_ends_after_its_bound() {
    let fixture = realtime_fixture();
    let home = home("dm-realtime-idle");
    let server = start_in_with_limits(
        home.clone(),
        Some(fixture.clone()),
        RealtimeLimits {
            idle: Duration::from_millis(200),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    open_realtime(&mut client, "voice", json!({})).await;
    let ended = notice(&mut client, "modality.realtime.ended").await;
    assert_eq!(ended["params"]["reason"], "idle");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_opening_connection_may_send_or_close_a_realtime_session() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-owner").await;
    let mut owner = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut owner, vec![realtime_model("voice", "voice", true)]).await;
    let opened = open_realtime(&mut owner, "voice", json!({})).await;
    let id = opened["connectionId"].as_str().unwrap().to_owned();
    let mut other = Client::new_version(&server, false, 3).await;
    let refused = other
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "x"}}}),
        )
        .await;
    assert_eq!(app_error(&refused), "not_found");
    let refused = other
        .rpc("modality.realtime.close", json!({"connectionId": id}))
        .await;
    assert_eq!(app_error(&refused), "not_found");
    assert!(fixture.realtime_frames().is_empty());
    let sent = owner
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "text", "data": {"type": "mine"}}}),
        )
        .await;
    assert_ok(&sent);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_capabilities_name_the_unserved_operations() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-caps").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(
        &mut client,
        vec![
            realtime_model("voice", "voice", true),
            realtime_model("undeclared", "voice", false),
        ],
    )
    .await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let voice = operation_in(&snapshot, "realtime", "voice");
    assert_eq!(voice["available"], true);
    assert_eq!(
        voice["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "voice", "available": true},
            {"providerId": PROVIDER, "modelId": "undeclared", "available": true}
        ])
    );
    for operation in ["transcribe", "avatar"] {
        let unserved = operation_in(&snapshot, "realtime", operation);
        assert_eq!(unserved["available"], false, "{operation}");
        assert_eq!(unserved["reasons"][0]["dimension"], "adapter");
        assert_eq!(
            unserved["reasons"][0]["detail"],
            format!("No available adapter serves realtime.{operation}")
        );
        assert!(unserved["options"].as_array().unwrap().is_empty());
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_realtime_model_without_a_declaration_cannot_open() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-undeclared").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(
        &mut client,
        vec![
            realtime_model("voice", "voice", true),
            realtime_model("undeclared", "voice", false),
        ],
    )
    .await;
    let refused = client
        .rpc(
            "modality.realtime.open",
            json!({"operation": "voice", "model": model("undeclared"), "params": {}}),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable");
    assert_eq!(
        refused["error"]["message"],
        "The model declares no realtime transport"
    );
    assert_eq!(fixture.realtime_opens(), 0, "no provider side was reached");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_open_refuses_unknown_operations_and_unserved_parameters() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-params").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    let unknown = client
        .rpc(
            "modality.realtime.open",
            json!({"operation": "connect", "model": model("voice"), "params": {}}),
        )
        .await;
    assert_eq!(app_error(&unknown), "modality_unknown_operation");
    let invalid = client
        .rpc(
            "modality.realtime.open",
            json!({"operation": "voice", "model": model("voice"), "params": {"session": {}}}),
        )
        .await;
    assert_eq!(app_error(&invalid), "invalid_input");
    assert_eq!(fixture.realtime_opens(), 0);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_realtime_session_passes_its_connection_through_and_expires() {
    let fixture = realtime_fixture();
    let (server, home) = start(Some(fixture.clone()), "dm-realtime-direct").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_realtime_provider(&mut client, vec![realtime_model("voice", "voice", true)]).await;
    fixture.set_realtime_behavior(RealtimeBehavior {
        direct: true,
        expires_in: Some(Duration::from_millis(500)),
        ..Default::default()
    });
    let opened = open_realtime(&mut client, "voice", json!({})).await;
    let id = opened["connectionId"].as_str().unwrap().to_owned();
    assert_eq!(opened["transport"], "direct");
    let connection = &opened["connection"];
    assert_eq!(connection["kind"], "webrtc");
    assert_eq!(connection["url"], "https://realtime.invalid/session");
    assert!(
        connection.get("expiresAt").is_some(),
        "a direct session states when it expires"
    );
    let sent = client
        .rpc(
            "modality.realtime.send",
            json!({"connectionId": id, "frame": {"kind": "control", "data": {"type": "ice", "candidate": "c"}}}),
        )
        .await;
    assert_ok(&sent);
    let frames = fixture.realtime_frames();
    assert_eq!(frames[0].1.kind, dim_modality::RealtimeFrameKind::Control);
    let ended = notice(&mut client, "modality.realtime.ended").await;
    assert_eq!(ended["params"]["reason"], "expired");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while fixture.realtime_closes().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter was not closed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_task_leaves_every_session_untouched() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture), "dm-session").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let cwd = home.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let created = client
        .rpc("session.create", json!({"cwd": cwd, "tools": ["read"]}))
        .await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "s"}}),
    )
    .await;
    wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await;
    let methods = client
        .rpc("session.methods", json!({"sessionId": session_id}))
        .await;
    assert_ok(&methods);
    assert!(
        !methods["result"].to_string().contains("modality"),
        "Modality contributes no Session method: {}",
        methods["result"]
    );
    let after = client
        .rpc(
            "session.history",
            json!({"sessionId": session_id, "branchId": "main", "after": 0, "limit": 256}),
        )
        .await;
    assert_ok(&after);
    let events = after["result"]["events"].to_string();
    assert!(
        !events.contains("modality") && !events.contains(task["task_id"].as_str().unwrap()),
        "Modality writes no Timeline event: {after}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

// ---------------------------------------------------------------------------
// Startup, unload and retention (A.6, A.4).
// ---------------------------------------------------------------------------

/// A clock a test sets; retention reads it at `apply` and on every terminal
/// write.
struct FixedClock(Mutex<DateTime<Utc>>);
impl FixedClock {
    fn new(now: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(now)))
    }
}
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn task_dir(home: &std::path::Path, task_id: &str) -> std::path::PathBuf {
    home.join("modality/tasks").join(task_id)
}

fn write_task_record(home: &std::path::Path, record: &Value) {
    let dir = task_dir(home, record["task_id"].as_str().unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let envelope = json!({"version": 1, "text": record.to_string()});
    std::fs::write(dir.join("task.json"), envelope.to_string()).unwrap();
}

fn read_task_record(home: &std::path::Path, task_id: &str) -> Value {
    let bytes = std::fs::read(task_dir(home, task_id).join("task.json")).unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope["version"], 1);
    serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap()
}

fn seed_record(task_id: &str, state: &str, time: &str) -> Value {
    json!({
        "version": 1,
        "task_id": task_id,
        "client_task_id": null,
        "revision": 2,
        "category": "image",
        "operation": "generate",
        "executor": {"kind": "provider",
            "provider": {"providerId": PROVIDER, "modelId": "alpha"},
            "adapter": "modalityAdapter"},
        "params": {"prompt": "seed"},
        "state": state,
        "remote_handle": null,
        "progress": null,
        "result": null,
        "error": null,
        "remote_cancel": null,
        "created_at": time,
        "updated_at": time,
        "finished_at": if state == "queued" || state == "running" { Value::Null } else { json!(time) },
    })
}

async fn wait_until_waits(fixture: &FixtureAdapter, wanted: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fixture.waits() < wanted {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter was not asked to wait {wanted} times"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_observes_a_running_task_it_never_submits_again() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-restart").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "r"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    wait_handle(&mut client, &task_id).await;
    wait_until_waits(&fixture, 1).await;
    server.shutdown();
    server.join().await.unwrap();
    assert_eq!(fixture.submits(), 1);
    // A running task with a handle survives the unload (A.6).
    let stored = read_task_record(&home, &task_id);
    assert_eq!(stored["state"], "running", "{stored}");
    assert_eq!(stored["remote_handle"], format!("handle-{task_id}"));

    // The next generation waits on the stored handle and never submits.
    let waits_before = fixture.waits();
    let server = start_in(home.clone(), Some(fixture.clone())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    wait_until_waits(&fixture, waits_before + 1).await;
    assert_eq!(fixture.submits(), 1, "observation never submits again");
    assert_eq!(
        state_of(&task_status(&mut client, &task_id).await),
        "running"
    );

    gate.cancel();
    let finished = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&finished), "succeeded", "{finished}");
    assert_eq!(
        finished["result"]["files"][0]["sha256"],
        sha256(b"fixture result")
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_interrupts_queued_and_unresumable_tasks() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        handles: Handles::None,
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-restart-settle").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "x"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    wait_until_waits(&fixture, 1).await;
    server.shutdown();
    server.join().await.unwrap();
    // The unload interrupted the task that had no handle.
    let stored = read_task_record(&home, &task_id);
    assert_eq!(stored["state"], "interrupted", "{stored}");
    assert!(stored["remote_handle"].is_null());

    // Rewrite what a crash would have left: `running` without a handle and a
    // `queued` task that never started. Startup settles both.
    let running = seed_record(&task_id, "running", "2026-11-06T00:00:00Z");
    write_task_record(&home, &running);
    let queued_id = "mt_crashed_queued";
    let queued = seed_record(queued_id, "queued", "2026-11-06T00:00:01Z");
    write_task_record(&home, &queued);

    let server = start_in(home.clone(), Some(fixture.clone())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let running = task_status(&mut client, &task_id).await;
    assert_eq!(state_of(&running), "interrupted", "{running}");
    assert_eq!(running["error"]["message"], "did not survive recovery");
    let queued = task_status(&mut client, queued_id).await;
    assert_eq!(state_of(&queued), "interrupted", "{queued}");
    assert!(queued["error"].is_null(), "{queued}");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_deletes_terminal_tasks_past_thirty_days() {
    let now = DateTime::parse_from_rfc3339("2026-11-06T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let recent = now - chrono::Duration::days(10);
    let old = now - chrono::Duration::days(31);
    let format = |time: DateTime<Utc>| time.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let home = home("dm-retention-age");
    let recent_id = "mt_recent";
    let old_id = "mt_old";
    write_task_record(&home, &seed_record(recent_id, "succeeded", &format(recent)));
    write_task_record(&home, &seed_record(old_id, "succeeded", &format(old)));
    for id in [old_id, recent_id] {
        let result = task_dir(&home, id).join("result");
        std::fs::create_dir_all(&result).unwrap();
        std::fs::write(result.join("out.bin"), b"kept or deleted").unwrap();
    }

    let server = start_in_with_clock(home.clone(), None, FixedClock::new(now)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        state_of(&task_status(&mut client, recent_id).await),
        "succeeded"
    );
    let gone = client
        .rpc("modality.task.status", json!({"task_id": old_id}))
        .await;
    assert_eq!(app_error(&gone), "not_found");
    assert!(
        !task_dir(&home, old_id).exists(),
        "retention deletes the task with its files"
    );
    assert!(task_dir(&home, recent_id).join("result/out.bin").exists());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_keeps_the_newest_two_hundred_terminal_tasks() {
    let now = DateTime::parse_from_rfc3339("2026-11-06T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let home = home("dm-retention-count");
    let mut ids = Vec::new();
    for index in 0..200 {
        let id = format!("mt_seed_{index:03}");
        let time = (now - chrono::Duration::minutes(index + 1))
            .to_rfc3339_opts(SecondsFormat::Nanos, true);
        write_task_record(&home, &seed_record(&id, "succeeded", &time));
        ids.push(id);
    }
    // The oldest seeded task carries a result file that retention removes.
    let oldest = ids.last().unwrap().clone();
    let result = task_dir(&home, &oldest).join("result");
    std::fs::create_dir_all(&result).unwrap();
    std::fs::write(result.join("out.bin"), b"oldest").unwrap();
    // A crash left this one queued; startup settles it `interrupted` before
    // retention runs, so it counts toward the total in this same apply.
    let crashed = "mt_crashed_queued";
    write_task_record(
        &home,
        &seed_record(crashed, "queued", "2026-11-05T23:59:30.000000000Z"),
    );

    let fixture = Arc::new(FixtureAdapter::default());
    let server =
        start_in_with_clock(home.clone(), Some(fixture.clone()), FixedClock::new(now)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    // 200 terminal tasks plus the one startup interrupted: the oldest goes.
    let gone = client
        .rpc("modality.task.status", json!({"task_id": oldest}))
        .await;
    assert_eq!(app_error(&gone), "not_found", "startup retention ran");
    assert!(
        !task_dir(&home, &oldest).exists(),
        "retention deletes the task with its files"
    );
    assert_eq!(
        state_of(&task_status(&mut client, crashed).await),
        "interrupted"
    );
    assert_eq!(
        state_of(&task_status(&mut client, &ids[0]).await),
        "succeeded"
    );

    // The next terminal task pushes the next oldest out (A.4).
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "n"}}),
    )
    .await;
    let finished = wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await;
    assert_eq!(state_of(&finished), "succeeded", "{finished}");
    let next_oldest = &ids[198];
    let gone = client
        .rpc("modality.task.status", json!({"task_id": next_oldest}))
        .await;
    assert_eq!(app_error(&gone), "not_found");
    assert!(!task_dir(&home, next_oldest).exists());
    assert_eq!(
        state_of(&task_status(&mut client, &ids[0]).await),
        "succeeded"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_unhandled_tasks_receive_their_own_results() {
    let fixture = Arc::new(FixtureAdapter::default());
    let gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        handles: Handles::None,
        gate: Some(gate.clone()),
        named_output: true,
        reverse_completion: true,
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-unhandled").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let first = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "first"}}),
    )
    .await;
    let second = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "second"}}),
    )
    .await;
    let first_id = first["task_id"].as_str().unwrap().to_owned();
    let second_id = second["task_id"].as_str().unwrap().to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fixture.submits() < 2 || fixture.waits() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "both jobs must reach the adapter's wait"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The second job completes before the first, and each wait gets its own
    // task id, so no submission-order pairing can produce the right results.
    gate.cancel();
    for task_id in [&first_id, &second_id] {
        let record = wait_terminal(&mut client, task_id).await;
        assert_eq!(state_of(&record), "succeeded", "{record}");
        let file = record["result"]["files"][0].clone();
        assert_eq!(file["bytes"], task_id.len());
        assert_eq!(file["sha256"], sha256(task_id.as_bytes()));
        let chunk = client
            .rpc(
                "modality.task.result",
                json!({"task_id": task_id, "name": "out.bin", "offset": 0, "length": 100}),
            )
            .await;
        assert_ok(&chunk);
        assert_eq!(
            STANDARD
                .decode(chunk["result"]["data_base64"].as_str().unwrap())
                .unwrap(),
            task_id.as_bytes(),
            "the result names the task that produced it"
        );
    }
    let mut received = fixture.wait_ids();
    received.sort();
    let mut wanted = vec![first_id, second_id];
    wanted.sort();
    assert_eq!(received, wanted, "every wait received its own task id");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_submit_reaches_the_accepted_job() {
    let fixture = Arc::new(FixtureAdapter::default());
    let submit_gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        submit_gate: Some(submit_gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-cancel-submit").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "s"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fixture.submits() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter was never asked to submit"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Cancel while `submit` is blocked, then let it return its handle.
    let cancel_id = client
        .send("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    submit_gate.cancel();
    let cancelled = client.response(cancel_id).await;
    assert_ok(&cancelled);
    assert_eq!(fixture.remote_cancels(), 1, "the executor was asked once");
    let record = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&record), "cancelled", "{record}");
    assert_eq!(record["remote_handle"], format!("handle-{task_id}"));
    assert_eq!(record["remote_cancel"], "cancelled", "{record}");
    assert_eq!(fixture.submits(), 1);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unload_during_submit_keeps_a_handle_for_the_next_generation() {
    let fixture = Arc::new(FixtureAdapter::default());
    let submit_gate = CancellationToken::new();
    fixture.set_behavior(Behavior {
        submit_gate: Some(submit_gate.clone()),
        ..Default::default()
    });
    let (server, home) = start(Some(fixture.clone()), "dm-unload-submit").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "u"}}),
    )
    .await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fixture.submits() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the adapter was never asked to submit"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The plugin unloads while `submit` is blocked; the adapter returns its
    // handle within the grace.
    let release = {
        let submit_gate = submit_gate.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            submit_gate.cancel();
        })
    };
    server.shutdown();
    server.join().await.unwrap();
    release.await.unwrap();
    let stored = read_task_record(&home, &task_id);
    assert_eq!(stored["state"], "running", "{stored}");
    assert_eq!(stored["remote_handle"], format!("handle-{task_id}"));
    assert_eq!(fixture.submits(), 1);

    // The next generation observes it without submitting again.
    let server = start_in(home.clone(), Some(fixture.clone())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let finished = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&finished), "succeeded", "{finished}");
    assert_eq!(fixture.submits(), 1, "the observation never submits again");
    assert!(fixture.wait_ids().contains(&task_id));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_path_is_bounded_before_reading_and_recorded_canonical() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-node-path").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;

    // One byte over the submission budget is refused before the file is
    // read, and nothing is written.
    let big = home.join("big.bin");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(dim_modality::MAX_SUBMISSION_BYTES + 1)
        .unwrap();
    drop(file);
    let tasks_dir = home.join("modality/tasks");
    let count = || {
        std::fs::read_dir(&tasks_dir)
            .map(|entries| entries.count())
            .unwrap_or(0)
    };
    let before = count();
    let refused = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate", "params": {"prompt": "b"},
                "files": [{"name": "big.png", "media_type": "image/png",
                    "node_path": big.to_str().unwrap()}]}),
        )
        .await;
    assert_eq!(
        app_error(&refused),
        "invalid_input",
        "one byte over the submission budget is refused: {refused}"
    );
    assert_eq!(count(), before, "an over-size node_path writes nothing");

    // A `node_path` that goes through a linked directory is accepted; the
    // record and the job both carry the canonical path.
    let real = home.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let file = real.join("image.png");
    std::fs::write(&file, b"through a linked directory").unwrap();
    let link = home.join("link-dir");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let through = link.join("image.png");
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "l"},
            "files": [{"name": "in.png", "media_type": "image/png",
                "node_path": through.to_str().unwrap()}]}),
    )
    .await;
    let canonical = std::fs::canonicalize(&file).unwrap();
    assert_eq!(
        task["params"]["files"][0]["node_path"],
        canonical.to_str().unwrap()
    );
    assert_eq!(
        task["params"]["files"][0]["sha256"],
        sha256(b"through a linked directory")
    );
    assert_eq!(
        state_of(&wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await),
        "succeeded"
    );
    let job = fixture.job(task["task_id"].as_str().unwrap()).unwrap();
    assert_eq!(job.files[0].path, canonical);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn params_files_is_reserved_for_the_validated_metadata() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm-reserved").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    let node_file = home.join("node.png");
    std::fs::write(&node_file, b"node bytes").unwrap();
    let link = home.join("link.png");
    std::os::unix::fs::symlink(&node_file, &link).unwrap();
    let tasks_dir = home.join("modality/tasks");
    let count = || {
        std::fs::read_dir(&tasks_dir)
            .map(|entries| entries.count())
            .unwrap_or(0)
    };

    // A caller's `params.files` is refused before the adapter's schema and
    // before anything is written or submitted.
    let before = count();
    let refused = client
        .rpc(
            "modality.task.submit",
            json!({"category": "image", "operation": "generate",
            "params": {"prompt": "p", "files": [
                {"name": "../escape", "media_type": "image/png", "bytes": 1, "sha256": "00"},
                {"name": "link.png", "media_type": "image/png", "bytes": 1, "sha256": "00",
                    "node_path": link.to_str().unwrap()}
            ]}}),
        )
        .await;
    assert_eq!(
        app_error(&refused),
        "invalid_input",
        "params.files is reserved: {refused}"
    );
    assert_eq!(count(), before, "a reserved params.files writes nothing");
    assert_eq!(fixture.submits(), 0, "the adapter is never called");

    // A real submission records exactly the validated metadata.
    let inline = b"reserved inline bytes".to_vec();
    let canonical = std::fs::canonicalize(&node_file).unwrap();
    let task = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "m"},
        "files": [
            inline_file("in.png", "image/png", &inline),
            {"name": "n.png", "media_type": "image/png", "node_path": node_file.to_str().unwrap()}
        ]}),
    )
    .await;
    let files = task["params"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{task}");
    assert_eq!(
        files[0],
        json!({"name": "in.png", "media_type": "image/png",
            "bytes": inline.len(), "sha256": sha256(&inline)})
    );
    assert_eq!(files[1]["name"], "n.png");
    assert_eq!(files[1]["bytes"], 10);
    assert_eq!(files[1]["sha256"], sha256(b"node bytes"));
    assert_eq!(files[1]["node_path"], canonical.to_str().unwrap());
    wait_terminal(&mut client, task["task_id"].as_str().unwrap()).await;

    // No files: the record carries an empty array.
    let empty = submit_task(
        &mut client,
        json!({"category": "image", "operation": "generate", "params": {"prompt": "e"}}),
    )
    .await;
    assert_eq!(empty["params"]["files"], json!([]));
    wait_terminal(&mut client, empty["task_id"].as_str().unwrap()).await;
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

// ---------------------------------------------------------------------------
// OCR host tasks (decision 4): the target the build runs itself.
// ---------------------------------------------------------------------------

const OCR_PROVIDER: &str = "local";
const OCR_MODEL: &str = "apple-vision";
/// The base64 of `b"image"`: the boundary checks a media type, not the bytes.
const IMAGE_BASE64: &str = "aW1hZ2U=";

/// A stand-in helper speaking the process contract in shell, keyed by the
/// input's name so one script covers every failure the mapping decides.
fn fake_helper(directory: &std::path::Path) -> std::path::PathBuf {
    let path = directory.join("fake-ocr-helper.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
input=""
artifacts=""
mode="text"
shift
while [ $# -gt 0 ]; do
  case "$1" in
    --input) input="$2"; shift 2 ;;
    --artifacts) artifacts="$2"; shift 2 ;;
    --output-mode) mode="$2"; shift 2 ;;
    *) echo "unexpected $1" >&2; exit 1 ;;
  esac
done
name=$(basename "$input")
case "$name" in
  exit2.png)
    printf '%s\n' '{"ok":false,"error":{"code":"encrypted_pdf","message":"The PDF requires a password"}}'
    exit 2 ;;
  exit1.png)
    echo "the helper broke" >&2
    exit 1 ;;
  hang.png)
    echo $$ > "$artifacts/pid"
    sleep 30
    exit 0 ;;
  loud.png)
    i=0
    while [ "$i" -lt 1400 ]; do
      printf '%0100d' 0
      i=$((i + 1))
    done
    exit 0 ;;
  big.png)
    text=$(yes a | tr -d '\n' | head -c 600000)
    printf '{"text":"%s","pageCount":3,"pages":[{"page":1,"text":"%s","width":10,"height":10,"regions":[]},{"page":2,"text":"%s","width":10,"height":10,"regions":[]},{"page":3,"text":"%s","width":10,"height":10,"regions":[]}]}' \
      "$text" "$text" "$text" "$text" > "$artifacts/result.json"
    printf '%s' "$text" > "$artifacts/text.txt"
    printf '%s\n' '{"ok":true,"pageCount":3}'
    exit 0 ;;
  fifo.png)
    mkfifo "$artifacts/result.json"
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
  link.png)
    printf '%s' '{"text":"OUTSIDE-MARKER","pageCount":1,"pages":[{"page":1,"text":"OUTSIDE-MARKER","width":10,"height":10,"regions":[]}]}' > "$TMPDIR/outside.json"
    ln -s "$TMPDIR/outside.json" "$artifacts/result.json"
    printf 'hello' > "$artifacts/text.txt"
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
  textlink.png)
    printf '%s' '{"text":"hello","pageCount":1,"pages":[{"page":1,"text":"hello","width":10,"height":10,"regions":[]}]}' > "$artifacts/result.json"
    printf 'stolen' > "$TMPDIR/outside.txt"
    ln -s "$TMPDIR/outside.txt" "$artifacts/text.txt"
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
  imagelink.png)
    printf '%s' '{"text":"hello","pageCount":1,"pages":[{"page":1,"text":"hello","width":10,"height":10,"regions":[]}]}' > "$artifacts/result.json"
    printf 'hello' > "$artifacts/text.txt"
    printf 'PNG' > "$TMPDIR/outside.png"
    ln -s "$TMPDIR/outside.png" "$artifacts/page-0001.png"
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
  oversized.png)
    printf '%s' '{"text":"hello","pageCount":1,"pages":[{"page":1,"text":"hello","width":10,"height":10,"regions":[]}]}' > "$artifacts/result.json"
    printf 'hello' > "$artifacts/text.txt"
    dd if=/dev/null of="$artifacts/page-0001.png" bs=1 seek=1073741825 2>/dev/null
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
  noresult.png)
    printf 'orphan text' > "$artifacts/text.txt"
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
  notext.png)
    printf '%s' '{"text":"hello","pageCount":1,"pages":[{"page":1,"text":"hello","width":10,"height":10,"regions":[]}]}' > "$artifacts/result.json"
    printf '%s\n' '{"ok":true,"pageCount":1}'
    exit 0 ;;
esac
printf '%s' '{"text":"hello\n\nworld","pageCount":2,"pages":[{"page":1,"text":"hello","width":10,"height":10,"regions":[]},{"page":2,"text":"world","width":10,"height":10,"regions":[]}]}' > "$artifacts/result.json"
printf 'hello\n\nworld' > "$artifacts/text.txt"
if [ "$mode" = "layout" ]; then
  printf 'PNG' > "$artifacts/page-0001.png"
fi
printf '%s\n' '{"ok":true,"pageCount":2}'
exit 0
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn start_with_helper(home: std::path::PathBuf, helper: std::path::PathBuf) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(
        Arc::new(MockTools),
        config,
        Some(InstallOptions {
            ocr_helper: Some(helper),
            ..Default::default()
        }),
        |_| {},
    )
    .await
}

async fn start_with_helper_and_hook(
    home: std::path::PathBuf,
    helper: std::path::PathBuf,
    hook: Arc<dyn ArtifactHook>,
) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(
        Arc::new(MockTools),
        config,
        Some(InstallOptions {
            ocr_helper: Some(helper),
            artifact: hook,
            ..Default::default()
        }),
        |_| {},
    )
    .await
}

fn ocr_submit(name: &str, mode: &str) -> Value {
    json!({
        "category": "ocr",
        "operation": "recognize",
        "params": {"outputMode": mode},
        "files": [{"name": name, "media_type": "image/png", "data_base64": IMAGE_BASE64}],
        "client_task_id": format!("ocr-{name}-{mode}"),
    })
}

/// The bytes of one result file through `modality.task.result`.
async fn result_bytes(client: &mut Client, task_id: &str, name: &str, length: usize) -> Vec<u8> {
    let reply = client
        .rpc(
            "modality.task.result",
            json!({"task_id": task_id, "name": name, "offset": 0, "length": length}),
        )
        .await;
    assert_ok(&reply);
    base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        reply["result"]["data_base64"].as_str().unwrap(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_offers_the_local_target_and_configure_sets_the_default() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let ocr = snapshot["result"]["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|category| category["category"] == "ocr")
        .unwrap();
    assert_eq!(ocr["available"], true);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert_eq!(recognize["available"], true);
    assert_eq!(
        recognize["options"],
        json!([{"providerId": OCR_PROVIDER, "modelId": OCR_MODEL, "available": true}])
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({
                "expectedRevision": snapshot["result"]["revision"],
                "category": "ocr",
                "operation": "recognize",
                "model": {"providerId": OCR_PROVIDER, "modelId": OCR_MODEL},
            }),
        )
        .await;
    assert_ok(&configured);
    let recognize = operation_in(&configured, "ocr", "recognize");
    assert_eq!(
        recognize["default"],
        json!({"providerId": OCR_PROVIDER, "modelId": OCR_MODEL, "available": true})
    );
    assert_eq!(
        recognize["effective"],
        json!({"providerId": OCR_PROVIDER, "modelId": OCR_MODEL, "source": "default"})
    );
    // A Provider model is not an OCR option.
    let refused = client
        .rpc(
            "modality.configure",
            json!({
                "expectedRevision": configured["result"]["revision"],
                "category": "ocr",
                "operation": "recognize",
                "model": {"providerId": PROVIDER, "modelId": "alpha"},
            }),
        )
        .await;
    assert_eq!(app_error(&refused), "modality_invalid_selection");
    let cleared = client
        .rpc(
            "modality.configure",
            json!({
                "expectedRevision": configured["result"]["revision"],
                "category": "ocr",
                "operation": "recognize",
                "model": null,
            }),
        )
        .await;
    assert_ok(&cleared);
    let next = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&next);
    assert_eq!(cleared["result"], next["result"]);
    let recognize = operation_in(&next, "ocr", "recognize");
    assert_eq!(recognize["default"], Value::Null, "{recognize}");
    assert_eq!(
        recognize["effective"],
        json!({"providerId": OCR_PROVIDER, "modelId": OCR_MODEL, "source": "automatic"})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ocr_task_publishes_the_helper_artifacts() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    assert_eq!(
        task["executor"],
        json!({
            "kind": "host",
            "node": null,
            "target": {"providerId": OCR_PROVIDER, "modelId": OCR_MODEL},
            "confined": null,
        })
    );
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    if cfg!(target_os = "macos") {
        assert_eq!(done["executor"]["confined"], true, "{done}");
    }
    let result = &done["result"];
    let names: Vec<&str> = result["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["result.json", "text.txt"]);
    assert_eq!(
        result["data"],
        json!({"pages": [{"page": 1, "text": "hello"}, {"page": 2, "text": "world"}]})
    );
    assert_eq!(
        result_bytes(&mut client, &task_id, "text.txt", 65536).await,
        b"hello\n\nworld"
    );
    let result_json = result_bytes(&mut client, &task_id, "result.json", 65536).await;
    let document: Value = serde_json::from_slice(&result_json).unwrap();
    assert_eq!(document["pageCount"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_carries_at_most_one_mebibyte_of_page_text() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("big.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    let data = &done["result"]["data"];
    assert_eq!(data["textTruncated"], true, "{data}");
    let pages = data["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3, "{data}");
    let numbers: Vec<u64> = pages
        .iter()
        .map(|page| page["page"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, [1, 2, 3]);
    // Page one fits the 1 MiB budget; page two would cross it and keeps only
    // its number, and so does page three.
    assert_eq!(pages[0]["text"].as_str().unwrap().len(), 600_000);
    assert!(pages[1].get("text").is_none(), "{data}");
    assert!(pages[2].get("text").is_none(), "{data}");
    let kept: usize = pages
        .iter()
        .filter_map(|page| page["text"].as_str())
        .map(str::len)
        .sum();
    assert!(kept <= 1024 * 1024, "{kept} bytes of text recorded");
    // The whole text is still in text.txt.
    let text = result_bytes(&mut client, &task_id, "text.txt", 1_048_576).await;
    assert_eq!(text.len(), 600_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_layout_task_publishes_the_page_image() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("scan.png", "layout")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    let names: Vec<&str> = done["result"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["result.json", "text.txt", "page-0001.png"]);
    assert_eq!(
        result_bytes(&mut client, &task_id, "page-0001.png", 65536).await,
        b"PNG"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_artifacts_that_are_not_regular_files_are_refused_without_blocking() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home.clone(), helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for (name, mode) in [
        ("fifo.png", "text"),
        ("link.png", "text"),
        ("textlink.png", "text"),
        ("imagelink.png", "layout"),
    ] {
        let started = std::time::Instant::now();
        let task = submit_task(&mut client, ocr_submit(name, mode)).await;
        let task_id = task["task_id"].as_str().unwrap().to_owned();
        let done = wait_terminal(&mut client, &task_id).await;
        assert_eq!(state_of(&done), "failed", "{name}: {done}");
        assert_eq!(done["error"]["code"], "executor_error", "{name}: {done}");
        assert!(done["result"].is_null(), "{name}: no result is written");
        let fetched = task_status(&mut client, &task_id).await;
        for record in [&done, &fetched] {
            let text = record.to_string();
            assert!(!text.contains("OUTSIDE-MARKER"), "{name}: {text}");
            assert!(!text.contains("stolen"), "{name}: {text}");
        }
        let result_dir = home.join(format!("modality/tasks/{task_id}/result"));
        // Artifacts published before the refused one are discarded; the
        // storage's own dot-directories are infrastructure, not results.
        let leftovers = std::fs::read_dir(&result_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "{name}: {leftovers} result files");
        if name == "fifo.png" {
            // A FIFO left at the artifact name must not block the read.
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the FIFO blocked the daemon for {:?}",
                started.elapsed()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_helper_result_is_refused_before_it_is_read() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home.clone(), helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("oversized.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "executor_error", "{done}");
    assert!(
        done["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("exceed"),
        "{done}"
    );
    assert!(done["result"].is_null(), "no result is written");
    let result_dir = home.join(format!("modality/tasks/{task_id}/result"));
    // The small artifacts copied before the oversized page are discarded with
    // it; the storage's own dot-directories are infrastructure, not results.
    let leftovers = std::fs::read_dir(&result_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_host_task_removes_its_work_directory() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home.clone(), helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for (name, message) in [
        ("noresult.png", "The OCR helper wrote no readable result"),
        ("notext.png", "The OCR helper wrote no text"),
    ] {
        let task = submit_task(&mut client, ocr_submit(name, "text")).await;
        let task_id = task["task_id"].as_str().unwrap().to_owned();
        let done = wait_terminal(&mut client, &task_id).await;
        assert_eq!(state_of(&done), "failed", "{name}: {done}");
        assert_eq!(done["error"]["code"], "executor_error", "{name}: {done}");
        assert_eq!(done["error"]["message"], message, "{name}: {done}");
        assert!(done["result"].is_null(), "{name}: no result is written");
        let work_dir = home.join(format!("modality/tasks/{task_id}/work"));
        assert!(
            !work_dir.exists(),
            "{name}: the failed task kept its work directory"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The count hook of the flow below: after a work file is opened and counted,
/// it records the size the count saw and, for `result.json` and
/// `page-0001.png`, replaces that file under the same name. A copy from a
/// second open would publish the replacement; a copy from the counted reader
/// publishes the recorded size.
struct ReplaceAfterCount {
    counted: Mutex<BTreeMap<String, u64>>,
}

impl ArtifactHook for ReplaceAfterCount {
    fn after_count(&self, work: &std::path::Path, name: &str) {
        let Ok(metadata) = std::fs::metadata(work.join(name)) else {
            return;
        };
        self.counted
            .lock()
            .unwrap()
            .insert(name.to_owned(), metadata.len());
        let replacement = work.join("replacement.bin");
        match name {
            "result.json" => std::fs::write(
                &replacement,
                b"{\"text\":\"CHANGED\",\"pageCount\":1,\"pages\":[{\"page\":1,\"text\":\"CHANGED\",\"width\":10,\"height\":10,\"regions\":[]}]}",
            )
            .unwrap(),
            "page-0001.png" => std::fs::write(&replacement, vec![b'X'; 100]).unwrap(),
            _ => return,
        }
        std::fs::rename(&replacement, work.join(name)).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_path_publishes_the_bytes_the_cap_counted() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let hook = Arc::new(ReplaceAfterCount {
        counted: Mutex::new(BTreeMap::new()),
    });
    let server = start_with_helper_and_hook(home, helper, hook.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("scan.png", "layout")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    let counted = hook.counted.lock().unwrap().clone();
    assert_eq!(counted["page-0001.png"], 3, "{counted:?}");
    assert_eq!(counted["text.txt"], 12, "{counted:?}");
    // Every published file has the size its count saw, and the total
    // is the counted total: the replacements were never copied.
    let files = done["result"]["files"].as_array().unwrap();
    let mut total = 0;
    for file in files {
        let name = file["name"].as_str().unwrap();
        let bytes = file["bytes"].as_u64().unwrap();
        assert_eq!(Some(&bytes), counted.get(name), "{name}: {done}");
        total += bytes;
    }
    assert_eq!(total, counted.values().sum::<u64>(), "{done}");
    assert_eq!(
        result_bytes(&mut client, &task_id, "page-0001.png", 65536).await,
        b"PNG"
    );
    // data.pages describes the published result.json, not the replacement.
    let result_json = result_bytes(&mut client, &task_id, "result.json", 65536).await;
    let document: Value = serde_json::from_slice(&result_json).unwrap();
    let expected: Vec<Value> = document["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| json!({"page": page["page"], "text": page["text"]}))
        .collect();
    assert_eq!(done["result"]["data"]["pages"], json!(expected), "{done}");
    assert!(!done.to_string().contains("CHANGED"), "{done}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_and_broken_helpers_fail_with_the_mapped_code() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for (name, code) in [
        ("exit2.png", "invalid_input"),
        ("exit1.png", "executor_error"),
    ] {
        let task = submit_task(&mut client, ocr_submit(name, "text")).await;
        let task_id = task["task_id"].as_str().unwrap().to_owned();
        let done = wait_terminal(&mut client, &task_id).await;
        assert_eq!(state_of(&done), "failed", "{done}");
        assert_eq!(done["error"]["code"], code, "{done}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_over_its_output_limit_fails_executor_error() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("loud.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "executor_error", "{done}");
    assert!(
        done["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("stdout"),
        "{done}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_a_running_helper() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home.clone(), helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, ocr_submit("hang.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    // The helper records its pid inside its work directory before sleeping.
    let pid_file = home.join("modality/tasks").join(&task_id).join("work/pid");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let pid = loop {
        if let Ok(pid) = std::fs::read_to_string(&pid_file) {
            break pid.trim().to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the helper never recorded its pid"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let cancelled = client
        .rpc("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    assert_ok(&cancelled);
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "cancelled", "{done}");
    assert!(
        done["executor"]["confined"].is_null(),
        "a started helper that returned no result records no confinement result: {done}"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !alive {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the helper {pid} outlived its task"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_named_node_is_unavailable() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut submit = ocr_submit("scan.png", "text");
    submit["node"] = json!("n2");
    let task = submit_task(&mut client, submit).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "node_unavailable", "{done}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_explicit_model_is_refused() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut submit = ocr_submit("scan.png", "text");
    submit["model"] = json!({"providerId": PROVIDER, "modelId": "alpha"});
    let reply = client.rpc("modality.task.submit", submit).await;
    assert_eq!(app_error(&reply), "model_unavailable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_takes_one_input_file() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut submit = ocr_submit("scan.png", "text");
    submit["files"] = json!([]);
    let reply = client.rpc("modality.task.submit", submit).await;
    assert_eq!(app_error(&reply), "invalid_input");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanished_input_fails_invalid_input() {
    let home = home("dm-ocr");
    let helper = fake_helper(&home);
    let server = start_with_helper(home.clone(), helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    // Four hanging tasks occupy the category; the fifth waits in the queue.
    let mut hanging = Vec::new();
    for index in 0..4 {
        let mut submit = ocr_submit("hang.png", "text");
        submit["client_task_id"] = json!(format!("hang-{index}"));
        let task = submit_task(&mut client, submit).await;
        hanging.push(task["task_id"].as_str().unwrap().to_owned());
    }
    let input = home.join("scan.png");
    std::fs::write(&input, b"image").unwrap();
    let mut queued = ocr_submit("scan.png", "text");
    queued["files"] = json!([{
        "name": "scan.png",
        "media_type": "image/png",
        "node_path": input.to_str().unwrap(),
    }]);
    queued["client_task_id"] = json!("vanishing");
    let task = submit_task(&mut client, queued).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    assert_eq!(state_of(&task), "queued");
    std::fs::remove_file(&input).unwrap();
    for hanging in &hanging {
        let reply = client
            .rpc("modality.task.cancel", json!({"task_id": hanging}))
            .await;
        assert_ok(&reply);
    }
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "invalid_input", "{done}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_is_absent_when_the_build_starts_no_helper() {
    let (server, _home) = start(None, "dm-ocr-nobuild").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert_eq!(recognize["available"], false);
    assert_eq!(recognize["options"], json!([]));
    assert_eq!(recognize["reasons"][0]["dimension"], "build");
    let refused = client
        .rpc(
            "modality.configure",
            json!({
                "expectedRevision": snapshot["result"]["revision"],
                "category": "ocr",
                "operation": "recognize",
                "model": {"providerId": OCR_PROVIDER, "modelId": OCR_MODEL},
            }),
        )
        .await;
    assert_eq!(app_error(&refused), "modality_invalid_selection");
    let submit = client
        .rpc(
            "modality.task.submit",
            json!({
                "category": "ocr",
                "operation": "recognize",
                "model": {"providerId": OCR_PROVIDER, "modelId": OCR_MODEL},
                "files": [{"name": "scan.png", "media_type": "image/png", "data_base64": IMAGE_BASE64}],
            }),
        )
        .await;
    assert_eq!(
        app_error(&submit),
        "modality_unavailable",
        "a build reason names the code: {submit}"
    );
    let message = submit["error"]["message"].as_str().unwrap();
    assert_eq!(
        message.lines().next(),
        Some("ocr.recognize is unavailable."),
        "{message}"
    );
    let reason = &recognize["reasons"][0];
    assert!(
        message.contains(&format!(
            "\n{}: {}",
            reason["dimension"].as_str().unwrap(),
            reason["detail"].as_str().unwrap()
        )),
        "the refusal carries the listing's reason: {message}"
    );
}

#[cfg(not(target_os = "macos"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_is_absent_on_a_platform_without_apple_vision() {
    let home = home("dm-ocr-platform");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert_eq!(recognize["available"], false);
    assert_eq!(recognize["reasons"][0]["dimension"], "platform");
    let submit = client
        .rpc(
            "modality.task.submit",
            json!({
                "category": "ocr",
                "operation": "recognize",
                "model": {"providerId": OCR_PROVIDER, "modelId": OCR_MODEL},
                "files": [{"name": "scan.png", "media_type": "image/png", "data_base64": IMAGE_BASE64}],
            }),
        )
        .await;
    assert_eq!(app_error(&submit), "unavailable");
}

// ---------------------------------------------------------------------------
// The plugin-supplied OCR target (A.2.1): a fixture process row provides
// `ocrHelper`, its `describe` reply is the test's, and the helper it names is
// test code. No plugin package, engine or downloaded file is involved.
// ---------------------------------------------------------------------------

use rutis::host::CtxHost;
use rutis_bridge::session::{HostDispatch, Reply, Value as ServiceValue, host_key};

const PP_OCR_MODEL: &str = "pp-ocr";

/// The fixture `ocrHelper` service: its `describe` returns the JSON the test
/// set, or `null`, and counts the calls so a flow can tell a task read it.
/// Past the replies it is allowed, it answers with a future that never
/// completes — a row that stops answering — and counts the reads dropped
/// without an answer.
#[derive(Clone, Default)]
struct OcrHelperFixture {
    description: Arc<Mutex<Option<Value>>>,
    /// The service's declared method kinds, as `HostDispatch::methods`
    /// returns them.
    declared: Arc<Mutex<Value>>,
    describes: Arc<AtomicUsize>,
    replies: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}
impl OcrHelperFixture {
    fn new(description: Option<Value>) -> Self {
        Self::declaring(description, json!({"describe": "async"}))
    }
    /// A fixture whose service declares `declared` — `{ method: kind }`, as a
    /// manifest's `provides` entry does.
    fn declaring(description: Option<Value>, declared: Value) -> Self {
        Self {
            description: Arc::new(Mutex::new(description)),
            declared: Arc::new(Mutex::new(declared)),
            describes: Default::default(),
            replies: Arc::new(AtomicUsize::new(usize::MAX)),
            dropped: Default::default(),
        }
    }
    fn set(&self, description: Option<Value>) {
        *self.description.lock().unwrap() = description;
    }
    fn describes(&self) -> usize {
        self.describes.load(Ordering::SeqCst)
    }
    /// Every `describe` from now on hangs.
    fn silent(&self) {
        self.replies.store(0, Ordering::SeqCst);
    }
    /// The next `describe` answers; every one after it hangs.
    fn reply_once_then_silent(&self) {
        self.replies.store(self.describes() + 1, Ordering::SeqCst);
    }
    /// How many hanging replies were given up on (a bound or a cancel).
    fn dropped(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }
}
/// Counts a pending `describe` answer dropped without one.
struct PendingRead(Arc<AtomicUsize>);
impl Drop for PendingRead {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl HostDispatch for OcrHelperFixture {
    fn methods(&self) -> Option<Value> {
        Some(self.declared.lock().unwrap().clone())
    }
    fn invoke(&self, method: &str, _args: ServiceValue) -> Reply {
        assert_eq!(method, "describe", "the fixture serves one method");
        let count = self.describes.fetch_add(1, Ordering::SeqCst) + 1;
        if count > self.replies.load(Ordering::SeqCst) {
            let dropped = self.dropped.clone();
            return Ok(ServiceValue::future(async move {
                let _pending = PendingRead(dropped);
                std::future::pending::<Reply>().await
            }));
        }
        let description = self.description.lock().unwrap().clone();
        // An `async` method answers with a future (LightOcr §3).
        Ok(ServiceValue::future(async move {
            Ok(ServiceValue::Data(description.unwrap_or(Value::Null)))
        }))
    }
}

/// The fixture plugin's process row: it provides `ocrHelper` and nothing else.
struct OcrHelperPlugin(OcrHelperFixture);
impl Plugin for OcrHelperPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "test.ocr.helper"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide_at(
            host_key("ocrHelper"),
            Arc::new(self.0.clone()) as Arc<dyn HostDispatch>,
        )?;
        Ok(())
    }
}

/// A product profile with the fixture plugin installed beside the build's own
/// OCR helper.
async fn start_with_ocr_targets(
    home: std::path::PathBuf,
    helper: Option<std::path::PathBuf>,
    fixture: OcrHelperFixture,
) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(
        Arc::new(MockTools),
        config,
        Some(InstallOptions {
            ocr_helper: helper,
            ..Default::default()
        }),
        move |builder| {
            builder.plugin(move |_| Ok(Box::new(OcrHelperPlugin(fixture.clone()))));
        },
    )
    .await
}

/// The `ocrHelper.describe` reply of A.2.1.
fn ocr_description(
    contract: u32,
    executable: &std::path::Path,
    args: &[std::path::PathBuf],
    read: &[&std::path::Path],
) -> Value {
    json!({
        "contract": contract,
        "target": {"providerId": OCR_PROVIDER, "modelId": PP_OCR_MODEL},
        "executable": executable.to_string_lossy(),
        "args": args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "read": read.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>(),
    })
}

fn write_script(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A plugin directory with the stub helper and the file it reads at start; the
/// stub writes the marker it read into `result.json` and `text.txt`.
fn plugin_copy(root: &std::path::Path, name: &str, marker: &str) -> std::path::PathBuf {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("marker.txt"), marker).unwrap();
    write_script(
        &directory.join("stub.sh"),
        r#"#!/bin/sh
input=""
artifacts=""
mode="text"
while [ $# -gt 0 ]; do
  case "$1" in
    --input) input="$2"; shift 2 ;;
    --artifacts) artifacts="$2"; shift 2 ;;
    --output-mode) mode="$2"; shift 2 ;;
    *) echo "unexpected $1" >&2; exit 1 ;;
  esac
done
directory=$(dirname "$0")
marker=$(cat "$directory/marker.txt")
printf '{"text":"%s","pageCount":1,"pages":[{"page":1,"text":"%s","width":10,"height":10,"regions":[]}]}' "$marker" "$marker" > "$artifacts/result.json"
printf '%s' "$marker" > "$artifacts/text.txt"
if [ "$mode" = "layout" ]; then printf 'PNG' > "$artifacts/page-0001.png"; fi
printf '{"ok":true,"pageCount":1}\n'
"#,
    );
    directory
}

/// The test-owned gate the (c) flows name: it runs outside the plugin
/// directory, marks the work directory ready, waits for the go marker, and
/// then execs the stub the description named.
fn ocr_gate(root: &std::path::Path) -> std::path::PathBuf {
    let gate = root.join("ocr-gate.sh");
    write_script(
        &gate,
        r#"#!/bin/sh
stub="$1"
shift
input=""
artifacts=""
mode="text"
while [ $# -gt 0 ]; do
  case "$1" in
    --input) input="$2"; shift 2 ;;
    --artifacts) artifacts="$2"; shift 2 ;;
    --output-mode) mode="$2"; shift 2 ;;
    *) echo "unexpected $1" >&2; exit 1 ;;
  esac
done
printf 'ready' > "$artifacts/ready"
n=0
while [ ! -f "$artifacts/go" ]; do
  n=$((n + 1))
  if [ "$n" -ge 600 ]; then echo "no go marker" >&2; exit 4; fi
  sleep 0.05
done
exec /bin/sh "$stub" --input "$input" --artifacts "$artifacts" --output-mode "$mode"
"#,
    );
    gate
}

/// A stub that refuses with a typed helper error: exit 2, one JSON line.
fn refusing_stub(directory: &std::path::Path) -> std::path::PathBuf {
    let stub = directory.join("refuse.sh");
    write_script(
        &stub,
        r#"#!/bin/sh
printf '{"ok":false,"error":{"code":"encrypted_pdf","message":"The PDF requires a password"}}\n'
exit 2
"#,
    );
    stub
}

/// A stub that exits unexpectedly: neither the success nor the error line.
fn breaking_stub(directory: &std::path::Path) -> std::path::PathBuf {
    let stub = directory.join("break.sh");
    write_script(
        &stub,
        r#"#!/bin/sh
echo "the plugin helper broke" >&2
exit 1
"#,
    );
    stub
}

/// A stub that reads a file beside its plugin directory, outside any grant the
/// description gives: under confinement the read fails and it exits 9.
#[cfg(target_os = "macos")]
fn leaking_stub(directory: &std::path::Path) -> std::path::PathBuf {
    let stub = directory.join("leak.sh");
    write_script(
        &stub,
        r#"#!/bin/sh
artifacts=""
while [ $# -gt 0 ]; do
  case "$1" in
    --input) shift 2 ;;
    --artifacts) artifacts="$2"; shift 2 ;;
    --output-mode) shift 2 ;;
    *) echo "unexpected $1" >&2; exit 1 ;;
  esac
done
directory=$(dirname "$0")
content=$(cat "$directory/../secret.txt") || exit 9
printf '{"text":"%s","pageCount":1,"pages":[{"page":1,"text":"%s","width":10,"height":10,"regions":[]}]}' "$content" "$content" > "$artifacts/result.json"
printf '%s' "$content" > "$artifacts/text.txt"
printf '{"ok":true,"pageCount":1}\n'
"#,
    );
    stub
}

/// An `ocr.recognize` submission naming the plugin target explicitly, with a
/// fresh `client_task_id` so one test can run several tasks.
fn plugin_ocr_submit(name: &str, mode: &str) -> Value {
    let mut submit = ocr_submit(name, mode);
    submit["model"] = json!({"providerId": OCR_PROVIDER, "modelId": PP_OCR_MODEL});
    submit["client_task_id"] = json!(format!("plugin-{name}-{mode}-{}", uuid::Uuid::new_v4()));
    submit
}

async fn wait_for_path(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn work_of(home: &std::path::Path, task_id: &str) -> std::path::PathBuf {
    home.join("modality/tasks").join(task_id).join("work")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_lists_a_plugin_target_and_runs_its_helper() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    let fixture = OcrHelperFixture::new(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[plugin.join("stub.sh")],
        &[&plugin],
    )));
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    let plugin_option = recognize["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["modelId"] == PP_OCR_MODEL)
        .unwrap_or_else(|| panic!("the plugin target is not listed: {recognize}"));
    assert_eq!(plugin_option["available"], true, "{recognize}");
    assert_eq!(plugin_option["reason"], Value::Null);
    if cfg!(target_os = "macos") {
        assert!(
            recognize["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|option| option["modelId"] == OCR_MODEL && option["available"] == true),
            "Apple Vision is not listed beside the plugin target: {recognize}"
        );
    }
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    assert_eq!(
        task["executor"],
        json!({
            "kind": "host",
            "node": null,
            "target": {"providerId": OCR_PROVIDER, "modelId": PP_OCR_MODEL},
            "confined": null,
        })
    );
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    let names: Vec<&str> = done["result"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["result.json", "text.txt"]);
    assert_eq!(
        done["result"]["data"],
        json!({"pages": [{"page": 1, "text": "plugin-copy"}]})
    );
    assert_eq!(
        result_bytes(&mut client, &task_id, "text.txt", 65536).await,
        b"plugin-copy"
    );
    if cfg!(target_os = "macos") {
        assert_eq!(done["executor"]["confined"], true, "{done}");
    } else {
        assert!(done["executor"]["confined"].is_boolean(), "{done}");
    }
    assert!(!work_of(&home, &task_id).exists(), "{done}");
    assert!(fixture.describes() >= 2, "submit and start each describe");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_target_is_absent_without_the_service() {
    let home = home("dm-ocr-plugin");
    let helper = fake_helper(&home);
    let server = start_with_helper(home, helper).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert!(
        !recognize["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["modelId"] == PP_OCR_MODEL),
        "an absent service lists no plugin target: {recognize}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            recognize["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|option| option["modelId"] == OCR_MODEL && option["available"] == true),
            "Apple Vision stays available: {recognize}"
        );
    }
    let refused = client
        .rpc(
            "modality.task.submit",
            plugin_ocr_submit("scan.png", "text"),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable");
    assert_eq!(
        refused["error"]["message"], "The PP-OCR plugin is not running",
        "{refused}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_target_with_an_older_contract_is_unavailable() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    // A newer contract changes the description's shape: an unknown field and
    // no `args`. It must still say its contract and name its target.
    let mut newer = ocr_description(
        2,
        std::path::Path::new("/bin/sh"),
        &[plugin.join("stub.sh")],
        &[&plugin],
    );
    let object = newer.as_object_mut().unwrap();
    object.remove("args");
    object.insert("gpu".into(), json!(true));
    let fixture = OcrHelperFixture::new(Some(newer));
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home, Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    let plugin_option = recognize["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["modelId"] == PP_OCR_MODEL)
        .unwrap_or_else(|| panic!("the outdated target is still listed: {recognize}"));
    assert_eq!(plugin_option["available"], false, "{recognize}");
    assert_eq!(
        plugin_option["reason"],
        json!({"dimension": "runtime", "detail": "The PP-OCR plugin needs an update"}),
        "{recognize}"
    );
    let refused = client
        .rpc(
            "modality.task.submit",
            plugin_ocr_submit("scan.png", "text"),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable");
    assert_eq!(
        refused["error"]["message"], "The PP-OCR plugin needs an update",
        "{refused}"
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({
                "expectedRevision": snapshot["result"]["revision"],
                "category": "ocr",
                "operation": "recognize",
                "model": {"providerId": OCR_PROVIDER, "modelId": PP_OCR_MODEL},
            }),
        )
        .await;
    assert_eq!(app_error(&configured), "modality_invalid_selection");
    assert_eq!(
        configured["error"]["message"], "The PP-OCR plugin needs an update",
        "{configured}"
    );
    // A reply that says this build's contract but is not the strict
    // description is no target at all.
    let mut malformed = ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[plugin.join("stub.sh")],
        &[&plugin],
    );
    malformed.as_object_mut().unwrap().remove("args");
    fixture.set(Some(malformed));
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert!(
        !recognize["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["modelId"] == PP_OCR_MODEL),
        "a malformed current-contract reply lists no plugin target: {recognize}"
    );
    let refused = client
        .rpc(
            "modality.task.submit",
            plugin_ocr_submit("scan.png", "text"),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable");
    assert_eq!(
        refused["error"]["message"], "The PP-OCR plugin is not running",
        "{refused}"
    );
}

/// A service that declares `describe` sync is never called: it reads as
/// needing an update, so the target is listed unavailable and a submission
/// fails with that reason. The kind is checked before the call, so a package
/// built against the old form cannot block a thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_target_declaring_describe_sync_needs_an_update() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    let fixture = OcrHelperFixture::declaring(
        Some(ocr_description(
            1,
            std::path::Path::new("/bin/sh"),
            &[plugin.join("stub.sh")],
            &[&plugin],
        )),
        json!({"describe": "sync"}),
    );
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    let plugin_option = recognize["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["modelId"] == PP_OCR_MODEL)
        .unwrap_or_else(|| panic!("the outdated target is still listed: {recognize}"));
    assert_eq!(plugin_option["available"], false, "{recognize}");
    assert_eq!(
        plugin_option["reason"],
        json!({"dimension": "runtime", "detail": "The PP-OCR plugin needs an update"}),
        "{recognize}"
    );
    let refused = client
        .rpc(
            "modality.task.submit",
            plugin_ocr_submit("scan.png", "text"),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable", "{refused}");
    assert_eq!(
        refused["error"]["message"], "The PP-OCR plugin needs an update",
        "{refused}"
    );
    assert_eq!(
        fixture.describes(),
        0,
        "a service declaring describe sync is never called"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_removal_after_describe_keeps_the_admitted_task() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "old-copy");
    let gate = ocr_gate(&home);
    let fixture = OcrHelperFixture::new(Some(ocr_description(
        1,
        &gate,
        &[plugin.join("stub.sh")],
        &[&plugin],
    )));
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let work = work_of(&home, &task_id);
    wait_for_path(&work.join("ready")).await;
    assert!(fixture.describes() >= 2, "the task read the description");
    // The provider is removed in the window between that read and the helper.
    fixture.set(None);
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert!(
        !recognize["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["modelId"] == PP_OCR_MODEL),
        "a removed provider lists no plugin target: {recognize}"
    );
    if cfg!(target_os = "macos") {
        assert_eq!(
            recognize["options"],
            json!([{"providerId": OCR_PROVIDER, "modelId": OCR_MODEL, "available": true}]),
            "{recognize}"
        );
    }
    let refused = client
        .rpc(
            "modality.task.submit",
            plugin_ocr_submit("scan.png", "text"),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable");
    assert_eq!(
        refused["error"]["message"], "The PP-OCR plugin is not running",
        "{refused}"
    );
    std::fs::write(work.join("go"), b"go").unwrap();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    assert_eq!(
        result_bytes(&mut client, &task_id, "text.txt", 65536).await,
        b"old-copy"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_update_after_describe_runs_the_old_copy() {
    let home = home("dm-ocr-plugin");
    let old = plugin_copy(&home, "pp-ocr-old", "old-copy");
    let new = plugin_copy(&home, "pp-ocr-new", "new-copy");
    let gate = ocr_gate(&home);
    let fixture = OcrHelperFixture::new(Some(ocr_description(
        1,
        &gate,
        &[old.join("stub.sh")],
        &[&old],
    )));
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let work = work_of(&home, &task_id);
    wait_for_path(&work.join("ready")).await;
    // The provider is updated in the window: later calls see the new copy, the
    // admitted task keeps the description it read.
    fixture.set(Some(ocr_description(
        1,
        &gate,
        &[new.join("stub.sh")],
        &[&new],
    )));
    std::fs::write(work.join("go"), b"go").unwrap();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    assert_eq!(
        result_bytes(&mut client, &task_id, "text.txt", 65536).await,
        b"old-copy"
    );
    // Later calls run the updated description: the new copy, directly.
    fixture.set(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[new.join("stub.sh")],
        &[&new],
    )));
    let later = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let later_id = later["task_id"].as_str().unwrap().to_owned();
    let later = wait_terminal(&mut client, &later_id).await;
    assert_eq!(state_of(&later), "succeeded", "{later}");
    assert_eq!(
        result_bytes(&mut client, &later_id, "text.txt", 65536).await,
        b"new-copy"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_helper_failures_clear_the_work_directory() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    let refuse = refusing_stub(&plugin);
    let break_it = breaking_stub(&plugin);
    let fixture = OcrHelperFixture::new(None);
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    // A typed refusal keeps the helper's code, mapped to the task vocabulary.
    fixture.set(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[refuse],
        &[&plugin],
    )));
    let task = submit_task(&mut client, plugin_ocr_submit("refused.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "invalid_input", "{done}");
    assert_eq!(
        done["error"]["message"], "The PDF requires a password",
        "{done}"
    );
    assert!(!work_of(&home, &task_id).exists(), "{done}");
    // Any other exit is the helper's unexpected failure.
    fixture.set(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[break_it],
        &[&plugin],
    )));
    let task = submit_task(&mut client, plugin_ocr_submit("broken.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "executor_error", "{done}");
    assert!(
        done["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("the plugin helper broke"),
        "{done}"
    );
    assert!(!work_of(&home, &task_id).exists(), "{done}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_helper_copy_gone_before_spawn_fails_the_task() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    let fixture = OcrHelperFixture::new(Some(ocr_description(
        1,
        &plugin.join("stub.sh"),
        &[],
        &[&plugin],
    )));
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture).await;
    let mut client = Client::new_version(&server, false, 3).await;
    std::fs::remove_dir_all(&plugin).unwrap();
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "executor_error", "{done}");
    assert!(
        done["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unreadable"),
        "{done}"
    );
    assert!(!work_of(&home, &task_id).exists(), "{done}");
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocr_plugin_helper_cannot_read_outside_its_read_set() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    let leak = leaking_stub(&plugin);
    std::fs::write(home.join("secret.txt"), "do-not-read").unwrap();
    let fixture = OcrHelperFixture::new(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        std::slice::from_ref(&leak),
        &[&plugin],
    )));
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    // The description's read set does not grant the file: the helper's read is
    // denied and its exit fails the task.
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "failed", "{done}");
    assert_eq!(done["error"]["code"], "executor_error", "{done}");
    assert!(!work_of(&home, &task_id).exists(), "{done}");
    // Granting the directory makes the same stub succeed: the denial was the
    // read set, not the stub.
    fixture.set(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[leak],
        &[&plugin, &home],
    )));
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    assert_eq!(
        result_bytes(&mut client, &task_id, "text.txt", 65536).await,
        b"do-not-read"
    );
}

/// A product profile with the fixture plugin and the fixture adapter: a flow
/// that runs an image task beside a stalled OCR service.
async fn start_with_ocr_targets_and_adapter(
    home: std::path::PathBuf,
    helper: Option<std::path::PathBuf>,
    fixture: OcrHelperFixture,
    adapter: Arc<FixtureAdapter>,
) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(
        Arc::new(MockTools),
        config,
        Some(InstallOptions {
            ocr_helper: helper,
            ..Default::default()
        }),
        move |builder| {
            builder.plugin(move |_| Ok(Box::new(OcrHelperPlugin(fixture.clone()))));
            builder.plugin(move |_| Ok(Box::new(FixturePlugin(adapter.clone()))));
        },
    )
    .await
}

/// Waits until `count` reaches `wanted`, failing at the deadline.
async fn wait_for_count(count: impl Fn() -> usize, wanted: usize, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while count() < wanted {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} never reached {wanted}: {}",
            count()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// `describe` is read only where the OCR target matters: an image task never
/// calls it, and a snapshot calls it once, whatever the row does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_submit_never_reads_a_silent_ocr_helper() {
    let home = home("dm-ocr-plugin");
    let fixture = OcrHelperFixture::new(None);
    fixture.silent();
    let helper = fake_helper(&home);
    let adapter = Arc::new(FixtureAdapter::default());
    let server =
        start_with_ocr_targets_and_adapter(home.clone(), Some(helper), fixture.clone(), adapter)
            .await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_task_provider(&mut client).await;
    // The image task completes while `describe` never answers, and its path
    // never reaches the service.
    let submit = json!({
        "category": "image", "operation": "generate",
        "params": {"prompt": "a cat"},
        "files": [inline_file("in.png", "image/png", b"inline image")],
        "client_task_id": "image-beside-ocr",
    });
    let task = submit_task(&mut client, submit).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    let done = wait_terminal(&mut client, &task_id).await;
    assert_eq!(state_of(&done), "succeeded", "{done}");
    assert_eq!(
        fixture.describes(),
        0,
        "an image task never reads ocrHelper"
    );
    // The snapshot carries every category, the OCR one included, so it reads
    // the service once and answers within the bound.
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    assert_eq!(
        fixture.describes(),
        1,
        "one read per snapshot, not one per category"
    );
    wait_for_count(|| fixture.dropped(), 1, "the unanswered describe").await;
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert!(
        !recognize["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["modelId"] == PP_OCR_MODEL),
        "a silent row lists no plugin target: {recognize}"
    );
    assert_eq!(
        operation_in(&snapshot, "image", "generate")["available"],
        true,
        "the image category is unaffected"
    );
}

/// A row that never answers reads as absent, bounded: the snapshot lists no
/// plugin target, Apple Vision is unchanged, and an explicit submit of the
/// plugin target fails with the one runtime reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_ocr_helper_reads_as_absent() {
    let home = home("dm-ocr-plugin");
    let fixture = OcrHelperFixture::new(None);
    fixture.silent();
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    assert_eq!(fixture.describes(), 1);
    wait_for_count(|| fixture.dropped(), 1, "the unanswered describe").await;
    let recognize = operation_in(&snapshot, "ocr", "recognize");
    assert!(
        !recognize["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["modelId"] == PP_OCR_MODEL),
        "a silent row lists no plugin target: {recognize}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            recognize["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|option| option["modelId"] == OCR_MODEL && option["available"] == true),
            "Apple Vision stays available: {recognize}"
        );
    } else {
        assert!(
            recognize["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason["dimension"] == "platform"),
            "Apple Vision keeps its platform reason: {recognize}"
        );
    }
    // A submission of the plugin target reads once, gives up at the bound,
    // and fails unavailable.
    let refused = client
        .rpc(
            "modality.task.submit",
            plugin_ocr_submit("scan.png", "text"),
        )
        .await;
    assert_eq!(app_error(&refused), "unavailable", "{refused}");
    let expected = if recognize["available"] == true {
        "The PP-OCR plugin is not running".to_owned()
    } else {
        let reasons = recognize["reasons"].as_array().unwrap();
        assert!(reasons.iter().any(|reason| reason["dimension"] == "runtime"
            && reason["detail"] == "The PP-OCR plugin is not running"));
        let mut message = "ocr.recognize is unavailable.".to_owned();
        for reason in reasons {
            message.push_str(&format!(
                "\n{}: {}",
                reason["dimension"].as_str().unwrap(),
                reason["detail"].as_str().unwrap()
            ));
        }
        message
    };
    assert_eq!(refused["error"]["message"], expected, "{refused}");
    assert_eq!(fixture.describes(), 2, "the submit reads once");
    wait_for_count(|| fixture.dropped(), 2, "the unanswered describe").await;
}

/// A cancel while the start-time `describe` is pending settles the task
/// `cancelled`, not `failed` with the absent reason, and the pending read is
/// dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_the_start_time_describe_settles_cancelled() {
    let home = home("dm-ocr-plugin");
    let plugin = plugin_copy(&home, "pp-ocr-plugin", "plugin-copy");
    let fixture = OcrHelperFixture::new(Some(ocr_description(
        1,
        std::path::Path::new("/bin/sh"),
        &[plugin.join("stub.sh")],
        &[&plugin],
    )));
    // The submit's read answers; the task's start-time read hangs.
    fixture.reply_once_then_silent();
    let helper = fake_helper(&home);
    let server = start_with_ocr_targets(home.clone(), Some(helper), fixture.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let task = submit_task(&mut client, plugin_ocr_submit("scan.png", "text")).await;
    let task_id = task["task_id"].as_str().unwrap().to_owned();
    wait_for_count(|| fixture.describes(), 2, "the start-time describe").await;
    let cancelled = client
        .rpc("modality.task.cancel", json!({"task_id": task_id}))
        .await;
    assert_ok(&cancelled);
    let done = tokio::time::timeout(Duration::from_secs(5), wait_terminal(&mut client, &task_id))
        .await
        .expect("the cancel waited on the pending describe");
    assert_eq!(state_of(&done), "cancelled", "{done}");
    wait_for_count(|| fixture.dropped(), 1, "the cancelled describe").await;
}
