//! A real App Server with EventHub installed and a scripted model, plus a
//! fixture producer row that declares a source and publishes.
#![allow(dead_code)]
pub mod fixture;
use dim_agent_capabilities::{
    defaults, history::HistoryRenderer, host::*, workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig, Names};
use dim_agent_types::*;
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server};
use dim_event_hub::{DeclareRequest, EventHub, EventHubError, PublishRequest};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Replies with queued tool batches, then with text; keeps every request. A
/// request whose text names an event `file` that was not read yet replies
/// with one `read` call for that path, so a flow can prove the whole body is
/// readable.
#[derive(Default)]
pub struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    reads: Mutex<Vec<String>>,
}

impl Script {
    pub fn push(&self, calls: Vec<ToolCall>) {
        self.batches.lock().unwrap().push_back(calls);
    }
    /// Every request's rendered text, oldest first.
    pub fn texts(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect()
    }
    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    /// The tool names the newest request offers.
    pub fn tools(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .last()
            .and_then(|request| request.tools.as_ref())
            .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect())
            .unwrap_or_default()
    }
    /// The `file` path an event marker in `rendered` names, once.
    fn read_call(&self, rendered: &str) -> Option<ToolCall> {
        let (start, escaped) = match rendered.find("file=\\\"") {
            Some(start) => (start + "file=\\\"".len(), true),
            None => (rendered.find("file=\"")? + "file=\"".len(), false),
        };
        let rest = &rendered[start..];
        let end = if escaped {
            rest.find("\\\"")?
        } else {
            rest.find('"')?
        };
        let path = rest[..end].to_owned();
        let mut reads = self.reads.lock().unwrap();
        if reads.contains(&path) {
            return None;
        }
        reads.push(path.clone());
        Some(ToolCall {
            id: format!("read-{}", reads.len()),
            name: "read".into(),
            input: json!({ "path": path }).as_object().unwrap().clone(),
        })
    }
}

impl ModelDriver for Script {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let rendered = format!("{request:?}");
            self.requests.lock().unwrap().push(request);
            let id = format!("m{}", self.count());
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let calls = match self.batches.lock().unwrap().pop_front() {
                Some(calls) => Some(calls),
                None => self.read_call(&rendered).map(|call| vec![call]),
            };
            let reason = match calls {
                Some(calls) => {
                    for call in calls {
                        chunks.push(Ok(ModelChunk::ToolStart {
                            id: call.id.clone(),
                            name: call.name.clone(),
                        }));
                        chunks.push(Ok(ModelChunk::ToolUse(call)));
                    }
                    ModelStopReason::ToolUse
                }
                None => {
                    chunks.push(Ok(ModelChunk::Text("done".into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    ModelStopReason::EndTurn
                }
            };
            chunks.push(Ok(ModelChunk::MessageStop {
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

/// What a fixture producer learns in its own `apply`: its row's context and
/// the key it reads `eventHub` with on every call (design Appendix F), and how
/// many times it declared.
#[derive(Default)]
pub struct ProducerState {
    installed: Mutex<Option<(Ctx, Option<Key<EventHub>>)>>,
    declares: std::sync::atomic::AtomicUsize,
}

impl ProducerState {
    fn install(&self, ctx: Ctx, key: Option<Key<EventHub>>) {
        *self.installed.lock().unwrap() = Some((ctx, key));
    }
    pub fn ctx(&self) -> Ctx {
        self.installed.lock().unwrap().as_ref().unwrap().0.clone()
    }
    /// The service as the row reads it now: absent while EventHub is off or
    /// reloading (design §2.1, Appendix F).
    pub fn hub(&self) -> Option<Arc<EventHub>> {
        let installed = self.installed.lock().unwrap();
        let (ctx, key) = installed.as_ref()?;
        ctx.get_at(key.as_ref()?)
    }
    /// How many declarations this producer made.
    pub fn declares(&self) -> usize {
        self.declares.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub async fn declare(&self, request: DeclareRequest) -> Result<(), EventHubError> {
        self.declares
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let hub = self
            .hub()
            .ok_or_else(|| EventHubError::new("no_caller_row", "eventHub is not installed"))?;
        hub.declare(&self.ctx(), request).await
    }
    pub async fn publish(&self, request: PublishRequest) -> Result<Value, EventHubError> {
        let hub = self
            .hub()
            .ok_or_else(|| EventHubError::new("no_caller_row", "eventHub is not installed"))?;
        hub.publish(&self.ctx(), request).await
    }
}

/// The fixture producer row: it captures its context and the `eventHub` name,
/// if the assembly registered it, so the test can call both. It registers the
/// `eventhub/ready` listener before reading the service and refreshes what it
/// found on each notice (design §2.1's producer pattern).
pub struct ProducerPart {
    state: Arc<ProducerState>,
    hub: Option<Key<EventHub>>,
}

impl DynPlugin for ProducerPart {
    fn name(&self) -> &str {
        "EventHubFixtureProducer"
    }
    fn injects(&self) -> Vec<TypeKey> {
        // No injected dependency: a producer reads `eventHub` optionally with
        // `get_at`, so nothing reapplies it when EventHub does.
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let Some(key) = self.hub.clone() else {
                self.state.install(ctx.clone(), None);
                return Ok(Effect::Done);
            };
            // The listener first, then the read: a service provided between
            // the two steps is not missed (design §2.1).
            {
                let state = self.state.clone();
                let key = key.clone();
                let listen_ctx = ctx.clone();
                ctx.listen::<dim_event_hub::Ready>().on(move |_, _| {
                    let state = state.clone();
                    let key = key.clone();
                    let ctx = listen_ctx.clone();
                    async move {
                        state.install(ctx.clone(), Some(key));
                        Ok(None)
                    }
                })?;
            }
            self.state.install(ctx.clone(), Some(key));
            Ok(Effect::Done)
        })
    }
}

fn with_producer(builder: &mut AgentBuilder, state: &Arc<ProducerState>) {
    let captured = state.clone();
    builder.plugin(move |names: &Names<'_>| {
        // The name is shared across languages, so a host that registered it
        // shows a key even before any provider loads; the read stays optional.
        let hub = names.key::<EventHub>().ok();
        Ok(Box::new(ProducerPart {
            state: captured.clone(),
            hub,
        }) as Box<dyn DynPlugin>)
    });
}

pub fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("eventhub")).unwrap();
    home
}

/// Starts a server with EventHub, the fixture producer and `model`.
pub async fn start(home: &Path, model: Arc<Script>) -> (RunningServer, Arc<ProducerState>) {
    start_with(home, model, true).await
}

/// [`start`], with or without EventHub installed (design Appendix D item 11).
pub async fn start_with(
    home: &Path,
    model: Arc<Script>,
    event_hub: bool,
) -> (RunningServer, Arc<ProducerState>) {
    start_full(home, model, event_hub, None).await
}

/// [`start`] with the TypeScript producer row loaded too: the bridge from
/// another language to the `eventHub` service (design Appendix F).
pub async fn start_typescript(
    home: &Path,
    model: Arc<Script>,
) -> (RunningServer, Arc<ProducerState>) {
    start_full(home, model, true, Some(typescript())).await
}

/// The TypeScript row of `runtime/fixtures/event-hub-producer.ts`, from the
/// environment the Node-backed flows use (`.github/workflows/ci.yml`).
pub fn typescript() -> SessionTypeScriptConfig {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    SessionTypeScriptConfig {
        python: None,
        acquire: None,
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: root.join("runtime/package.json"),
        rows: vec![json!({
            "id": "event-hub-producer",
            "name": root.join("runtime/fixtures/event-hub-producer.ts"),
            "inject": ["eventHub"],
            "provides": {},
            "config": {},
        })],
    }
}

async fn start_full(
    home: &Path,
    model: Arc<Script>,
    event_hub: bool,
    session_typescript: Option<SessionTypeScriptConfig>,
) -> (RunningServer, Arc<ProducerState>) {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    // The product's local-execution profile also installs Wait, so a branch
    // can wait and a steer delivery ends its wait (design 3.1).
    dim_agent_capabilities::wait::install(&mut builder);
    defaults::mock_model_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    // The real workspace tools, so the model's `read` (Appendix D item 5)
    // opens the Session file the marker names, as it does in the product.
    dim_agent_capabilities::workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: home.join("command-output"),
        },
    );
    if event_hub {
        dim_event_hub::install(&mut builder);
    }
    let state = Arc::new(ProducerState::default());
    with_producer(&mut builder, &state);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.join("dimcode.sqlite")),
            product_home: Some(home.to_owned()),
            unix_path: Some(home.join("native.sock")),
            session_typescript,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    if event_hub {
        // The producer reads `eventHub` optionally: a row that applied first
        // learns the service is there from `eventhub/ready` (design §2.1).
        wait_for("the eventHub service", || state.hub().is_some()).await;
    }
    (server, state)
}

/// A connected client with the fixture model selected.
pub async fn connect(server: &RunningServer) -> Client {
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "eventhub-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    let created = client
        .rpc(
            "provider.create",
            json!({"providerId": "018ec73f-0000-7000-8000-000000000007", "displayName": "Fixture",
                "connection": {"driverKind": "openai-compatible", "baseUrl": "http://localhost/v1",
                    "auth": {"type": "anonymous"}},
                "models": [{"modelId": "fixture", "displayName": "Fixture", "status": "active",
                    "capabilities": dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],
                "enabled": true}),
        )
        .await;
    if created.is_ok() {
        client
            .rpc(
                "provider.select",
                json!({"providerId": "018ec73f-0000-7000-8000-000000000007", "modelId": "fixture"}),
            )
            .await
            .unwrap();
    }
    client
}

/// An interactive Session with full access, `read` (the model opens an
/// event's Session file with it) and `branch`.
pub async fn create_session(client: &mut Client, home: &Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["read", "branch"],
                "interactive": true,
                "permissionPolicy": {"preset": "full-access", "allowedTools": [], "disallowedTools": []}
            }),
        )
        .await
        .unwrap();
    created["sessionId"].as_str().unwrap().to_owned()
}

/// [`create_session`] with `events` in its mount list: the tool the Session
/// part registers (design A.4).
pub async fn create_events_session(client: &mut Client, home: &Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["read", "branch", "events"],
                "interactive": true,
                "permissionPolicy": {"preset": "full-access", "allowedTools": [], "disallowedTools": []}
            }),
        )
        .await
        .unwrap();
    created["sessionId"].as_str().unwrap().to_owned()
}

/// [`create_session`] with `wait` as well: a branch that can wait and be
/// woken by a steer (design 3.1).
pub async fn create_wait_session(client: &mut Client, home: &Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["read", "branch", "wait"],
                "interactive": true,
                "permissionPolicy": {"preset": "full-access", "allowedTools": [], "disallowedTools": []}
            }),
        )
        .await
        .unwrap();
    created["sessionId"].as_str().unwrap().to_owned()
}

pub async fn history(client: &mut Client, session_id: &str) -> Vec<Value> {
    let page: Value = client
        .rpc(
            "session.history",
            json!({"sessionId": session_id, "branchId": "main", "after": 0, "limit": 500}),
        )
        .await
        .unwrap();
    page["events"].as_array().cloned().unwrap_or_default()
}

/// The `input/message` events whose blocks carry `needle`.
pub fn inputs(events: &[Value], needle: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event["type"] == "input" && event["name"] == "message")
        .filter(|event| {
            event["data"]["blocks"].as_array().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(needle))
                })
            })
        })
        .cloned()
        .collect()
}

/// The text of an input event's first block.
pub fn text_of(event: &Value) -> String {
    event["data"]["blocks"]
        .as_array()
        .and_then(|blocks| blocks.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or_default()
        .to_owned()
}

pub async fn wait_for(description: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

/// A declaration of `im` with `im.message`, delivered `queue` by default.
pub fn im_declaration(guide: &str) -> DeclareRequest {
    DeclareRequest {
        source: "im".into(),
        guide: guide.into(),
        types: vec![dim_event_hub::TypeDeclaration {
            r#type: "im.message".into(),
            description: "An instant message".into(),
            mode: InputMode::Queue,
        }],
    }
}

/// An event of the fixture producer.
pub fn im_event(id: &str) -> PublishRequest {
    PublishRequest {
        source: "im".into(),
        r#type: "im.message".into(),
        id: Some(id.into()),
        title: Some("Build failed".into()),
        body: "line one".into(),
        attributes: [("channel".to_owned(), "general".to_owned())]
            .into_iter()
            .collect(),
        to: Vec::new(),
    }
}

/// Subscribes over the management plane and answers the subscription id.
pub async fn subscribe(
    client: &mut Client,
    session: &str,
    types: Vec<&str>,
    mode: Option<&str>,
) -> String {
    subscribe_on(client, session, "main", types, mode).await
}

/// [`subscribe`] against one named branch.
pub async fn subscribe_on(
    client: &mut Client,
    session: &str,
    branch: &str,
    types: Vec<&str>,
    mode: Option<&str>,
) -> String {
    let mut params = json!({
        "types": types,
        "target": {"sessionId": session, "branchId": branch},
    });
    if let Some(mode) = mode {
        params["mode"] = json!(mode);
    }
    let reply = client.rpc("eventhub.subscribe", params).await.unwrap();
    reply["subscriptionId"].as_str().unwrap().to_owned()
}

/// Opens a child branch of `session`'s main line and answers its id, the way
/// a `branch` tool call would.
pub async fn open_branch(server: &RunningServer, client: &mut Client, session: &str) -> String {
    let agent = server.agent().unwrap();
    let at = agent
        .get_session(session)
        .unwrap()
        .sequence("main")
        .unwrap();
    client
        .rpc(
            "session.branch_create",
            json!({"sessionId": session, "parentBranchId": "main", "at": at,
                "prompt": "Child work."}),
        )
        .await
        .unwrap()["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Unloads the idle Session, as the daemon's idle pass would.
pub async fn unload(agent: &dim_agent_runtime::Agent, session: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(loaded) = agent.get_session(session)
                && loaded.unload_if_idle().await.unwrap_or(false)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{session} unloads"));
}

/// The `eventhub.subscriptions` list for one Session, read over the client.
pub async fn listed(client: &mut Client, session: &str) -> Vec<Value> {
    let reply: Value = client
        .rpc("eventhub.subscriptions", json!({"sessionId": session}))
        .await
        .unwrap();
    reply["subscriptions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}
