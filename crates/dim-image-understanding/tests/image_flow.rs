use dim_agent_capabilities::{
    defaults,
    history::HistoryRenderer,
    host::*,
    tool_registry::{ToolContribution, ToolRegistry, definition},
};
use dim_agent_runtime::{testing::branch_events, *};
use dim_agent_types::*;
use dim_app_server::{RunningServer, ServerConfig, methods::ManagementMethods, serve};
use futures_util::stream;
use rutis::{
    Ctx,
    host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=";
const REFERENCE: &str = "img:12c5f2d047c10416";
const PROVIDER: &str = "176ea61e-7705-491f-b005-e2085d52b431";
const GEMINI_PROVIDER: &str = "b5f2c4b6-31d6-4a9a-9f0b-7a3f0c1a2d64";
/// A 1x1 GIF: a format Gemini does not take.
const GIF: &str = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
/// A 1x1 WebP: a format only the compatible, Chat and Responses protocols take.
const WEBP: &str = "UklGRhoAAABXRUJQVlA4TA0AAAAvAAAAEAcQERGIiP4HAA==";
/// The two native xAI driver kinds. The built-in `xai` and `grok-build`
/// providers route through the compatible protocol, so only a custom provider
/// reaches these.
const NATIVE_XAI: [&str; 2] = ["xai", "xai-responses"];
/// The New API route whose model the reuse tests re-point.
const ROUTED_PROVIDER: &str = "2b0f6f5c-6b8f-4a3e-9a02-5e5b1f9c7d31";

fn image() -> ImageSource {
    ImageSource {
        source_type: ImageSourceType::Base64,
        media_type: "image/png".into(),
        data: Some(PNG.into()),
        url: None,
        file_id: None,
    }
}

#[derive(Default)]
struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    auxiliary: Mutex<Vec<Value>>,
    gate: Mutex<Option<CancellationToken>>,
    truncated: Mutex<bool>,
    refusal: Mutex<Option<(u16, &'static str)>>,
    dropped: AtomicUsize,
    selections: AtomicUsize,
}

impl Script {
    fn push(&self, calls: &[(&str, &str, Value)]) {
        self.batches.lock().unwrap().push_back(
            calls
                .iter()
                .map(|(name, id, input)| ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
                    input: input.as_object().unwrap().clone(),
                })
                .collect(),
        );
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
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: "reply".into(),
            })];
            self.requests.lock().unwrap().push(request);
            let reason = match self.batches.lock().unwrap().pop_front() {
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
                message_id: "reply".into(),
                reason,
            }));
            Ok(Box::pin(stream::iter(chunks)) as ModelStream)
        })
    }
}

async fn provider(stream: tokio::net::TcpStream, script: Arc<Script>) {
    let mut stream = BufReader::new(stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await.unwrap() == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().unwrap();
        }
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    script.auxiliary.lock().unwrap().push(request.clone());
    let refusal = *script.refusal.lock().unwrap();
    if let Some((status, message)) = refusal {
        #[derive(serde::Serialize)]
        struct ErrorBody {
            error: ErrorMessage,
        }
        #[derive(serde::Serialize)]
        struct ErrorMessage {
            message: &'static str,
        }
        let body = serde_json::to_string(&ErrorBody {
            error: ErrorMessage { message },
        })
        .unwrap();
        let reply = format!(
            "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        return;
    }
    let gate = script.gate.lock().unwrap().clone();
    if let Some(gate) = gate {
        let mut byte = [0];
        tokio::select! {
            _ = gate.cancelled() => {}
            result = stream.read(&mut byte) => {
                assert_eq!(result.unwrap(), 0, "a cancelled request closes its connection");
                script.dropped.fetch_add(1, Ordering::SeqCst);
                return;
            }
        }
    }
    let reason = if *script.truncated.lock().unwrap() {
        "length"
    } else {
        "stop"
    };
    let model = request["model"].as_str().unwrap_or("gemini");
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "id":"answer","object":"chat.completion.chunk","created":0,"model":model,
            "choices":[{"index":0,"delta":{"content":format!("answer from {model}")},"finish_reason":reason}]
        })
    );
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
}

struct ReadImage;
impl ToolDriver for ReadImage {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            risk: Some("session".into()),
            ..definition(
                "read",
                "Read an image fixture.",
                json!({"type":"object", "properties":{}, "additionalProperties":false}),
            )
        }]
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async {
            Ok(ToolResult {
                content: ToolContent::Blocks(vec![ContentBlock::Image { source: image() }]),
                ..ToolResult::text("")
            })
        })
    }
}

type Tools = Arc<Mutex<HashMap<String, Arc<ToolRegistry>>>>;
type Histories = Arc<Mutex<HashMap<(String, String), Arc<Timeline>>>>;
struct CaptureHistory {
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
    histories: Histories,
}
impl DynPlugin for CaptureHistory {
    fn name(&self) -> &str {
        "FixtureImageHistory"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            self.histories.lock().unwrap().insert(
                (identity.session_id.clone(), identity.branch_id.clone()),
                ctx.require_at(&self.timeline)?,
            );
            Ok(Effect::Done)
        })
    }
}
struct Probe {
    registry: Key<ToolRegistry>,
    identity: Key<SessionIdentity>,
    tools: Tools,
}
impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "FixtureImageTools"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            self.tools.lock().unwrap().insert(
                ctx.require_at(&self.identity)?.session_id.clone(),
                ctx.require_at(&self.registry)?,
            );
            Ok(Effect::Done)
        })
    }
}

struct ObserveSelection {
    modality: dim_modality::ModalityPlugin,
    script: Arc<Script>,
}
impl DynPlugin for ObserveSelection {
    fn name(&self) -> &str {
        self.modality.name()
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.modality.injects()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let selection = ctx.require::<ModelSelectionHost>()?;
            let script = self.script.clone();
            let observed = ModelSelectionHost::new(Arc::new(move |explicit, cwd, cancel| {
                let selection = selection.clone();
                let script = script.clone();
                Box::pin(async move {
                    let request = ModelSelectionRequest::at(explicit, cwd.as_deref())
                        .expect("the owner's request has a UTF-8 directory");
                    let result = selection.resolve(request, cancel).await;
                    script.selections.fetch_add(1, Ordering::SeqCst);
                    result.map_err(|error| HostError::failed(error.to_string()))
                })
            }));
            let ctx = ctx.isolate(
                TypeKey::of::<ModelSelectionHost>(),
                "fixture-image-selection",
            );
            ctx.provide(observed)?;
            self.modality.apply(&ctx).await
        })
    }
}

struct Fixture {
    server: RunningServer,
    script: Arc<Script>,
    tools: Tools,
    home: std::path::PathBuf,
    endpoint: String,
    provider_task: tokio::task::JoinHandle<()>,
    histories: Histories,
}
impl Fixture {
    async fn new() -> Self {
        let home = std::env::temp_dir().join(format!("dim-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let script = Arc::new(Script::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let remote = script.clone();
        let provider_task = tokio::spawn(async move {
            let mut calls = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        calls.spawn(provider(accepted.unwrap().0, remote.clone()));
                    }
                    completed = calls.join_next(), if !calls.is_empty() => {
                        completed.unwrap().unwrap();
                    }
                }
            }
        });
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )));
        defaults::install(&mut builder);
        defaults::mock_model_hosts_with_model(
            &mut builder,
            HistoryRenderer::default(),
            script.clone(),
        );
        defaults::tool_host(&mut builder, Arc::new(ReadImage));
        names::register::<dim_modality::ModalityAdapterService>(builder.catalog_mut(), None);
        builder.catalog_mut().register_shared("modalityTargets");
        let observed = script.clone();
        builder.optional_plugin(move |names| {
            Ok(Box::new(ObserveSelection {
                modality: dim_modality::ModalityPlugin::new(names, Default::default())?,
                script: observed.clone(),
            }))
        });
        dim_image_understanding::install(&mut builder);
        let tools = Tools::default();
        let observed = tools.clone();
        builder.require_feature(Feature::new("FixtureImageTools").session(
            "FixtureImageTools",
            move |names| {
                Ok(Box::new(Probe {
                    registry: names.key()?,
                    identity: names.key()?,
                    tools: observed.clone(),
                }))
            },
        ));
        let histories = Histories::default();
        let recorded = histories.clone();
        builder.require_feature(Feature::new("FixtureImageHistory").branch(
            "FixtureImageHistory",
            move |names| {
                Ok(Box::new(CaptureHistory {
                    timeline: names.key()?,
                    identity: names.key()?,
                    histories: recorded.clone(),
                }))
            },
        ));
        let server = serve(
            builder,
            ServerConfig {
                product_home: Some(home.clone()),
                target_home: Some(home.join("native")),
                platform_home: Some(home.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fixture = Self {
            server,
            script,
            tools,
            home,
            endpoint: endpoint.clone(),
            provider_task,
            histories,
        };
        let mut vision = CatalogCapabilities::with_limits(128_000, 1024);
        vision.vision = true;
        fixture.rpc("provider.create", json!({
            "providerId":PROVIDER, "displayName":"Fixture", "enabled":true,
            "connection":{"driverKind":"openai-compatible","baseUrl":endpoint,"auth":{"type":"anonymous"}},
            "models":[
                {"modelId":"alpha","displayName":"alpha","status":"active","capabilities":vision},
                {"modelId":"beta","displayName":"beta","status":"active","capabilities":vision},
                {"modelId":"text","displayName":"text","status":"active","capabilities":CatalogCapabilities::with_limits(128_000, 1024)}
            ]
        })).await;
        fixture.rpc("provider.create", json!({
            "providerId":GEMINI_PROVIDER, "displayName":"Gemini fixture", "enabled":true,
            "connection":{"driverKind":"gemini","baseUrl":endpoint,"auth":{"type":"anonymous"}},
            "models":[
                {"modelId":"gemini-alpha","displayName":"gemini-alpha","status":"active","capabilities":vision}
            ]
        })).await;
        fixture
    }
    async fn rpc(&self, name: &str, params: Value) -> Value {
        self.server
            .service::<ManagementMethods>()
            .unwrap()
            .provider(name)
            .unwrap()
            .accept(name.into(), params, None, CancellationToken::new(), |_| {})
            .unwrap()
            .reply
            .await
            .unwrap()
            .unwrap()
    }
    async fn configure(&self, model: Option<&str>) {
        self.configure_to(PROVIDER, model).await;
    }
    async fn configure_to(&self, provider: &str, model: Option<&str>) {
        let revision = self.rpc("modality.capabilities", json!({})).await["revision"].clone();
        self.rpc("modality.configure", json!({"expectedRevision":revision,"category":"image","operation":"understand","model":model.map(|id| json!({"providerId":provider,"modelId":id}))})).await;
    }
    /// A custom provider of one native xAI driver kind, serving a vision
    /// model on this fixture's endpoint.
    async fn native_xai(&self, driver: &str) -> String {
        let provider = uuid::Uuid::new_v4().to_string();
        let mut vision = CatalogCapabilities::with_limits(128_000, 1024);
        vision.vision = true;
        self.rpc("provider.create", json!({
            "providerId":provider, "displayName":"Native xAI fixture", "enabled":true,
            "connection":{"driverKind":driver,"baseUrl":self.endpoint,"auth":{"type":"anonymous"}},
            "models":[
                {"modelId":"grok-vision","displayName":"grok-vision","status":"active","capabilities":vision}
            ]
        }))
        .await;
        provider
    }
    /// Points the built-in `xai` provider at this fixture's endpoint: the
    /// catalog entry serves xAI over the compatible protocol, so this keeps its
    /// identity and driver while the wire goes to the fixture.
    async fn builtin_xai(&self) {
        let view = self
            .rpc("provider.management", json!({"providerId":"xai"}))
            .await;
        let version = view["providers"][0]["version"].as_u64().unwrap();
        self.rpc(
            "provider.update",
            json!({
                "providerId":"xai","expectedVersion":version,
                "patch":{
                    "baseUrl":self.endpoint,
                    "credential":{"action":"set","value":"xai-fixture-key"},
                    "enabled":true
                }
            }),
        )
        .await;
    }
    /// A New API route that serves `openai` at first: its model's protocol is
    /// the route metadata, which a model edit can move without changing the
    /// provider or the model id.
    async fn routed(&self) {
        let mut vision = CatalogCapabilities::with_limits(128_000, 1024);
        vision.vision = true;
        self.rpc("provider.create", json!({
            "providerId":ROUTED_PROVIDER, "displayName":"Routed fixture", "enabled":true,
            "connection":{"driverKind":"new-api-router","baseUrl":self.endpoint,"auth":{"type":"anonymous"}},
            "models":[{
                "modelId":"routed", "displayName":"routed", "status":"active", "capabilities":vision,
                "metadata":{"raw":{"endpointType":"openai"}}
            }]
        }))
        .await;
    }
    /// Moves the routed model's protocol, keeping its provider and model id.
    async fn move_route(&self, endpoint: &str) {
        let catalog = self
            .rpc("provider.catalog", json!({"providerId":ROUTED_PROVIDER}))
            .await;
        self.rpc(
            "provider.models.edit",
            json!({
                "providerId":ROUTED_PROVIDER,
                "expectedVersion":catalog["catalog"]["version"],
                "modelId":"routed",
                "edit":{"endpoint":{"action":"set","value":endpoint}}
            }),
        )
        .await;
    }
    async fn session(&self) -> Session {
        self.server
            .agent()
            .unwrap()
            .create_session(SessionConfig {
                cwd: self.home.to_string_lossy().into_owned(),
                tools: Some(vec!["read".into(), "image_understand".into()]),
                ..Default::default()
            })
            .await
            .unwrap()
    }
    fn tool(&self, session: &Session) -> Arc<ToolContribution> {
        self.tools.lock().unwrap()[session.id()]
            .contributed("image_understand")
            .unwrap()
    }
    async fn ask(&self, session: &Session, branch: &str, id: &str, input: Value) -> ToolResult {
        self.script.push(&[("image_understand", id, input)]);
        session
            .submit("inspect".into(), InputMode::Queue, branch)
            .await
            .unwrap();
        idle(session, branch).await;
        let events = branch_events(session, branch, ..).await.unwrap();
        events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(data) => data
                    .results
                    .iter()
                    .find(|r| r.call_id == id)
                    .map(|r| r.result.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for {id}: {events:?}"))
    }
    async fn close(self) {
        self.server.shutdown();
        self.server.join().await.unwrap();
        self.provider_task.abort();
        let _ = self.provider_task.await;
        std::fs::remove_dir_all(self.home).unwrap();
    }
}

async fn idle(session: &Session, branch: &str) {
    tokio::time::timeout(Duration::from_secs(15), session.idle(branch))
        .await
        .unwrap()
        .unwrap();
}
async fn turn(session: &Session, text: &str) {
    session
        .submit(text.into(), InputMode::Queue, "main")
        .await
        .unwrap();
    idle(session, "main").await;
}
async fn attach(fixture: &Fixture, session: &Session, branch: &str) {
    attach_image(
        fixture,
        session,
        branch,
        session_model(session),
        "attached.png",
        PNG,
        "image/png",
    )
    .await;
}
async fn attach_gif(fixture: &Fixture, session: &Session, branch: &str) {
    attach_image(
        fixture,
        session,
        branch,
        session_model(session),
        "attached.gif",
        GIF,
        "image/gif",
    )
    .await;
}
fn session_model(session: &Session) -> ModelSelection {
    ModelSelection {
        metadata: Some(RequestMetadata {
            session_id: Some(session.id().into()),
            ..Default::default()
        }),
        ..SessionConfig::default().model
    }
}
async fn attach_with(fixture: &Fixture, session: &Session, branch: &str, model: ModelSelection) {
    attach_image(
        fixture,
        session,
        branch,
        model,
        "attached.png",
        PNG,
        "image/png",
    )
    .await;
}
async fn attach_image(
    fixture: &Fixture,
    session: &Session,
    branch: &str,
    model: ModelSelection,
    name: &str,
    data: &str,
    media_type: &str,
) {
    use base64::Engine;
    let stored = fixture
        .server
        .service::<SessionFiles>()
        .unwrap()
        .0
        .put(
            session.id().into(),
            name.into(),
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    session
        .configure(
            ConfigureUpdate {
                model: Some(model),
                ..Default::default()
            },
            branch,
        )
        .await
        .unwrap();
    let source = ImageSource {
        source_type: ImageSourceType::File,
        file_id: Some(stored.file_id),
        data: None,
        media_type: media_type.into(),
        ..image()
    };
    session
        .input(
            InputMessageData {
                user: Some(UserInput {
                    content: vec![ContentBlock::Image { source }],
                }),
                ..Default::default()
            },
            branch,
        )
        .await
        .unwrap();
    idle(session, branch).await;
}
fn selection(model_id: &str, vision: bool, session: &str) -> ModelSelection {
    let target = ModelId {
        provider_id: PROVIDER.into(),
        model_id: model_id.into(),
    };
    ModelSelection {
        provider_id: PROVIDER.into(),
        model_id: model_id.into(),
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target,
            capabilities: ModelCapabilities {
                vision,
                ..ModelCapabilities::with_limits(128_000, 1024)
            },
            reasoning: None,
            service_tier: None,
        }),
        config: None,
        metadata: Some(RequestMetadata {
            session_id: Some(session.into()),
            ..Default::default()
        }),
    }
}
async fn configure_model(session: &Session, branch: &str, model_id: &str, vision: bool) {
    session
        .configure(
            ConfigureUpdate {
                model: Some(selection(model_id, vision, session.id())),
                ..Default::default()
            },
            branch,
        )
        .await
        .unwrap();
}
fn question() -> Value {
    json!({"image":REFERENCE, "question":"What is visible?"})
}
fn gif_reference() -> String {
    dim_agent_capabilities::images::image_reference(&ImageSource {
        source_type: ImageSourceType::Base64,
        media_type: "image/gif".into(),
        data: Some(GIF.into()),
        url: None,
        file_id: None,
    })
    .expect("the GIF fixture has a content reference")
}
fn webp_reference() -> String {
    dim_agent_capabilities::images::image_reference(&ImageSource {
        source_type: ImageSourceType::Base64,
        media_type: "image/webp".into(),
        data: Some(WEBP.into()),
        url: None,
        file_id: None,
    })
    .expect("the WebP fixture has a content reference")
}
fn gif_question() -> Value {
    json!({"image":gif_reference(),"question":"What is visible?"})
}
fn text(result: &ToolResult) -> &str {
    let ToolContent::Text(text) = &result.content else {
        panic!("text answer")
    };
    text
}
fn request_images(request: &ModelRequest) -> Vec<ImageSource> {
    let mut images = Vec::new();
    for message in &request.messages {
        let MessageContent::Parts(parts) = &message.content else {
            continue;
        };
        for part in parts {
            match part {
                MessagePart::Image { source } => images.push(source.clone()),
                MessagePart::ToolResult {
                    content: ToolContent::Blocks(blocks),
                    ..
                } => images.extend(blocks.iter().filter_map(|block| match block {
                    ContentBlock::Image { source } => Some(source.clone()),
                    _ => None,
                })),
                _ => {}
            }
        }
    }
    images
}
fn request_texts(request: &ModelRequest) -> Vec<String> {
    let mut texts = Vec::new();
    for message in &request.messages {
        let MessageContent::Parts(parts) = &message.content else {
            continue;
        };
        for part in parts {
            match part {
                MessagePart::Text { text, .. } => texts.push(text.clone()),
                MessagePart::ToolResult {
                    content: ToolContent::Blocks(blocks),
                    ..
                } => texts.extend(blocks.iter().filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })),
                _ => {}
            }
        }
    }
    texts
}

/// A standalone image question has no history a content reference could
/// resolve against, so a target that does not take the image's format fails
/// the call instead of answering without seeing the image.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_the_target_cannot_take_fails_without_an_answer() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach_gif(&fixture, &session, "main").await;
    fixture
        .configure_to(GEMINI_PROVIDER, Some("gemini-alpha"))
        .await;
    let result = fixture
        .ask(
            &session,
            "main",
            "gif",
            json!({"image":gif_reference(),"question":"What is visible?"}),
        )
        .await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert!(
        text(&result).starts_with("image_understand_failed:"),
        "{result:?}"
    );
    assert!(
        fixture.script.auxiliary.lock().unwrap().is_empty(),
        "the target never receives a request without the image"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_questions_follow_current_defaults_and_reuse_only_completed_own_answers() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach(&fixture, &session, "main").await;
    fixture.configure(Some("beta")).await;
    let first = fixture.ask(&session, "main", "first", question()).await;
    assert_eq!(text(&first), "answer from beta");
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    let reused = fixture.ask(&session, "main", "reuse", question()).await;
    assert_eq!(reused, first);
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    assert!(session.unload_if_idle().await.unwrap());
    let session = fixture
        .server
        .agent()
        .unwrap()
        .restore_session(session.id().into())
        .await
        .unwrap();
    assert_eq!(
        fixture.ask(&session, "main", "restored", question()).await,
        first
    );
    assert_eq!(
        fixture.script.auxiliary.lock().unwrap().len(),
        1,
        "reuse is rebuilt from committed facts"
    );
    fixture.configure(Some("alpha")).await;
    assert_eq!(
        text(&fixture.ask(&session, "main", "changed", question()).await),
        "answer from alpha"
    );
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 2);
    fixture.configure(None).await;
    assert_eq!(
        text(&fixture.ask(&session, "main", "automatic", question()).await),
        "answer from alpha"
    );
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 2);
    fixture.configure(Some("alpha")).await;
    let disabled = fixture
        .rpc("provider.catalog", json!({"providerId":PROVIDER}))
        .await;
    fixture
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":PROVIDER,"expectedVersion":disabled["catalog"]["version"],"modelIds":["beta"]}),
        )
        .await;
    let unavailable = fixture
        .ask(&session, "main", "unavailable", question())
        .await;
    assert_eq!(unavailable.is_error, Some(true));
    assert!(text(&unavailable).starts_with("image_model_unavailable:"));
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 2);
    for request in fixture.script.auxiliary.lock().unwrap().iter() {
        assert_eq!(request["messages"].as_array().unwrap().len(), 2);
        assert!(request.get("tools").is_none());
        assert!(request.get("metadata").is_none());
        assert_eq!(request["max_tokens"], 1024);
        assert_eq!(request["messages"][0]["role"], "system");
        let parts = request["messages"][1]["content"].as_array().unwrap();
        assert_eq!(
            parts[0]["image_url"]["url"],
            format!("data:image/png;base64,{PNG}")
        );
        assert_eq!(parts[1]["text"], "What is visible?");
        assert_eq!(parts.len(), 2);
    }
    {
        let requests = fixture.script.requests.lock().unwrap();
        assert!(
            serde_json::to_string(requests[0].system.as_ref().unwrap())
                .unwrap()
                .contains("image_understand")
        );
        assert!(
            request_images(&requests[0])
                .iter()
                .any(|source| source.data.as_deref() == Some(PNG))
        );
        assert!(
            !request_texts(&requests[0])
                .iter()
                .any(|text| text == REFERENCE)
        );
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vision_model_receives_a_read_image_as_pixels() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    configure_model(&session, "main", "alpha", true).await;
    fixture.script.push(&[("read", "read-image", json!({}))]);
    turn(&session, "read the image").await;
    let request = fixture
        .script
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(request.model_id, "alpha");
    assert!(
        request
            .capability_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.capabilities.vision)
    );
    let images = request_images(&request);
    assert_eq!(
        images.len(),
        1,
        "the read image reaches the model as pixels"
    );
    assert_eq!(images[0].source_type, ImageSourceType::Base64);
    assert_eq!(images[0].data.as_deref(), Some(PNG));
    assert!(!request_texts(&request).iter().any(|text| text == REFERENCE));
    assert!(fixture.script.auxiliary.lock().unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vision_model_keeps_two_read_images_in_later_turns_without_image_understanding() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    configure_model(&session, "main", "alpha", true).await;
    fixture
        .script
        .push(&[("read", "read-a", json!({})), ("read", "read-b", json!({}))]);
    turn(&session, "read both images").await;
    turn(&session, "compare the second difference").await;
    let request = fixture
        .script
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        request_images(&request).len(),
        2,
        "both read images stay in the later turn"
    );
    assert!(!request_texts(&request).iter().any(|text| text == REFERENCE));
    let events = branch_events(&session, "main", ..).await.unwrap();
    assert!(!events.iter().any(|event| matches!(&event.kind,
        EventKind::ToolCall(data) if data.calls.iter().any(|call| call.name == "image_understand"))));
    assert!(fixture.script.auxiliary.lock().unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_model_receives_references_and_can_still_ask_image_understand() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    configure_model(&session, "main", "text", false).await;
    fixture.script.push(&[("read", "read-image", json!({}))]);
    fixture
        .script
        .push(&[("image_understand", "understand", question())]);
    turn(&session, "read and inspect").await;
    let after_read = {
        let requests = fixture.script.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        requests[1].clone()
    };
    assert!(request_images(&after_read).is_empty());
    assert!(
        request_texts(&after_read)
            .iter()
            .any(|text| text == REFERENCE),
        "a text model receives the reference"
    );
    assert!(
        after_read
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|tool| tool.name == "image_understand")),
        "image_understand stays available to a text model"
    );
    let events = branch_events(&session, "main", ..).await.unwrap();
    let answer = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data
                .results
                .iter()
                .find(|result| result.call_id == "understand"),
            _ => None,
        })
        .expect("the image question ran");
    assert_eq!(text(&answer.result), "answer from alpha");
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    configure_model(&session, "main", "alpha", true).await;
    turn(&session, "look again").await;
    let after_switch = fixture
        .script
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        request_images(&after_switch).len(),
        1,
        "a later vision turn receives the stored image as pixels"
    );
    assert!(
        !request_texts(&after_switch)
            .iter()
            .any(|text| text == REFERENCE)
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachments_and_read_images_route_alike_on_both_models() {
    let fixture = Fixture::new().await;
    for (model_id, vision) in [("alpha", true), ("text", false)] {
        let session = fixture.session().await;
        attach_with(
            &fixture,
            &session,
            "main",
            selection(model_id, vision, session.id()),
        )
        .await;
        fixture.script.push(&[("read", "read-image", json!({}))]);
        turn(&session, "read the attached image").await;
        let request = fixture
            .script
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(request.model_id, model_id);
        if vision {
            assert_eq!(
                request_images(&request).len(),
                2,
                "the attachment and the read image are both pixels for {model_id}"
            );
            assert!(!request_texts(&request).iter().any(|text| text == REFERENCE));
        } else {
            assert!(request_images(&request).is_empty());
            assert_eq!(
                request_texts(&request)
                    .iter()
                    .filter(|text| *text == REFERENCE)
                    .count(),
                2,
                "the attachment and the read image share one reference for {model_id}"
            );
        }
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsendable_image_is_a_placeholder_for_a_vision_model() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    configure_model(&session, "main", "alpha", true).await;
    session
        .input(
            InputMessageData {
                user: Some(UserInput {
                    content: vec![
                        ContentBlock::Image { source: image() },
                        ContentBlock::Image {
                            source: ImageSource {
                                source_type: ImageSourceType::Url,
                                media_type: "image/png".into(),
                                data: None,
                                url: Some("https://example.invalid/a.png".into()),
                                file_id: None,
                            },
                        },
                        ContentBlock::Image {
                            source: ImageSource {
                                source_type: ImageSourceType::Base64,
                                media_type: "image/svg+xml".into(),
                                data: Some(PNG.into()),
                                url: None,
                                file_id: None,
                            },
                        },
                        ContentBlock::Image {
                            source: ImageSource {
                                source_type: ImageSourceType::Base64,
                                media_type: "image/png".into(),
                                data: Some("not base64!".into()),
                                url: None,
                                file_id: None,
                            },
                        },
                    ],
                }),
                ..Default::default()
            },
            "main",
        )
        .await
        .unwrap();
    idle(&session, "main").await;
    let request = fixture
        .script
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        request_images(&request).len(),
        1,
        "the sendable image still reaches the vision model"
    );
    assert_eq!(
        request_texts(&request)
            .iter()
            .filter(|text| text.starts_with("[Image omitted:"))
            .count(),
        3,
        "an unsendable image is a placeholder even for a vision model"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_file_image_is_a_placeholder_for_a_vision_model() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    configure_model(&session, "main", "alpha", true).await;
    let stored = fixture
        .server
        .service::<SessionFiles>()
        .unwrap()
        .0
        .put(
            session.id().into(),
            "empty.png".into(),
            Vec::new(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    session
        .input(
            InputMessageData {
                user: Some(UserInput {
                    content: vec![ContentBlock::Image {
                        source: ImageSource {
                            source_type: ImageSourceType::File,
                            file_id: Some(stored.file_id),
                            data: None,
                            ..image()
                        },
                    }],
                }),
                ..Default::default()
            },
            "main",
        )
        .await
        .unwrap();
    idle(&session, "main").await;
    let request = fixture
        .script
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert!(
        request_images(&request).is_empty(),
        "the empty owned file is not sent as an image block"
    );
    assert_eq!(
        request_texts(&request)
            .iter()
            .filter(|text| text.starts_with("[Image omitted:"))
            .count(),
        1,
        "the empty file takes the omission placeholder"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_understanding_default_does_not_change_conversational_routing() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    configure_model(&session, "main", "alpha", true).await;
    fixture.configure(Some("beta")).await;
    attach_with(
        &fixture,
        &session,
        "main",
        selection("alpha", true, session.id()),
    )
    .await;
    fixture.script.push(&[("read", "read-image", json!({}))]);
    turn(&session, "read the attached image").await;
    let request = fixture
        .script
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(request.model_id, "alpha");
    assert_eq!(
        request_images(&request).len(),
        2,
        "the understood default does not replace pixels"
    );
    assert!(fixture.script.auxiliary.lock().unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_image_refusals_remain_tool_errors_and_later_batched_calls_send_one_image_each() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach(&fixture, &session, "main").await;
    fixture.configure(Some("beta")).await;

    for (index, (status, id, reason)) in [
        (400, "rejected", "fixture rejected the image"),
        (413, "too-large", "fixture request body too large"),
    ]
    .into_iter()
    .enumerate()
    {
        *fixture.script.refusal.lock().unwrap() = Some((status, reason));
        let result = fixture.ask(&session, "main", id, question()).await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).starts_with("image_understand_failed:"));
        assert!(text(&result).contains(reason), "{}", text(&result));
        assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), index + 1);
    }

    *fixture.script.refusal.lock().unwrap() = None;
    fixture.script.push(&[
        ("image_understand", "after-refusal", question()),
        (
            "image_understand",
            "second-question",
            json!({"image":REFERENCE,"question":"Is there any text?"}),
        ),
    ]);
    session
        .submit("inspect both questions".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    idle(&session, "main").await;
    let events = branch_events(&session, "main", ..).await.unwrap();
    let results = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(&data.results),
            _ => None,
        })
        .unwrap();
    assert_eq!(results.len(), 2);
    for id in ["after-refusal", "second-question"] {
        let result = &results
            .iter()
            .find(|result| result.call_id == id)
            .unwrap()
            .result;
        assert_eq!(text(result), "answer from beta");
    }
    {
        let requests = fixture.script.auxiliary.lock().unwrap();
        assert_eq!(requests.len(), 4);
        let mut questions = Vec::new();
        for request in &requests[2..] {
            assert_eq!(request["model"], "beta");
            let parts = request["messages"][1]["content"].as_array().unwrap();
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[0]["type"], "image_url");
            assert_eq!(
                parts[0]["image_url"]["url"],
                format!("data:image/png;base64,{PNG}")
            );
            questions.push(parts[1]["text"].as_str().unwrap());
        }
        questions.sort_unstable();
        assert_eq!(questions, ["Is there any text?", "What is visible?"]);
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_visibility_is_inherited_but_reuse_and_references_do_not_cross_cuts_or_sessions() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    let before = session.sequence("main").unwrap();
    fixture.script.push(&[("read", "read-image", json!({}))]);
    session
        .submit("read".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    idle(&session, "main").await;
    assert_eq!(
        text(&fixture.ask(&session, "main", "parent", question()).await),
        "answer from alpha"
    );
    let options = || BranchOptions {
        prompt: "child".into(),
        source: BranchSource::Host {
            operation_id: "fixture".into(),
        },
    };
    let inherited = session
        .branch("main".into(), session.sequence("main").unwrap(), options())
        .await
        .unwrap();
    idle(&session, &inherited).await;
    assert_eq!(
        text(&fixture.ask(&session, &inherited, "child", question()).await),
        "answer from alpha"
    );
    assert_eq!(
        fixture.script.auxiliary.lock().unwrap().len(),
        2,
        "a child does not reuse its parent's answer"
    );
    let cut = session
        .branch("main".into(), before, options())
        .await
        .unwrap();
    idle(&session, &cut).await;
    let other = fixture.session().await;
    for (session, branch) in [(&session, cut.as_str()), (&other, "main")] {
        let unknown = fixture.ask(session, branch, "unknown", question()).await;
        assert!(text(&unknown).starts_with("image_reference_unknown:"));
    }
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 2);
    for (input, error) in [
        (
            json!({"image":REFERENCE,"question":"   "}),
            "image_question_invalid:",
        ),
        (
            json!({"image":REFERENCE,"question":"图".repeat(1366)}),
            "image_question_invalid:",
        ),
        (
            json!({"image":REFERENCE,"question":"x","extra":true}),
            "invalid_params:",
        ),
    ] {
        assert!(text(&fixture.ask(&session, "main", "invalid", input).await).starts_with(error));
    }
    *fixture.script.truncated.lock().unwrap() = true;
    let input = json!({"image":REFERENCE,"question":"different"});
    for id in ["partial1", "partial2"] {
        let answer = fixture.ask(&session, "main", id, input.clone()).await;
        assert!(text(&answer).contains("[Incomplete:"));
    }
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 4);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_image_waiters_cancel_independently_and_last_drop_releases_provider_work() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach(&fixture, &session, "main").await;
    let gate = CancellationToken::new();
    *fixture.script.gate.lock().unwrap() = Some(gate.clone());
    let tool = fixture.tool(&session);
    let context = || ToolExecutionContext {
        branch_id: "main".into(),
        request_id: "fixture".into(),
        turn_id: "fixture".into(),
    };
    let call = || ToolCall {
        id: "fixture".into(),
        name: "image_understand".into(),
        input: question().as_object().unwrap().clone(),
    };
    let first_cancel = CancellationToken::new();
    let first = tool
        .handler
        .execute(call(), context(), first_cancel.clone());
    let second = tool
        .handler
        .execute(call(), context(), CancellationToken::new());
    tokio::pin!(first, second);
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.script.auxiliary.lock().unwrap().is_empty() {
            assert!(futures_util::poll!(&mut first).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let selections = fixture.script.selections.load(Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(futures_util::poll!(&mut second).is_pending());
            if fixture.script.selections.load(Ordering::SeqCst) > selections {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second call finishes its current-target owner read and waits for the answer");
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    first_cancel.cancel();
    assert!(matches!(first.await, Err(HostError::Cancelled(_))));
    gate.cancel();
    assert_eq!(text(&second.await.unwrap()), "answer from alpha");
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    assert_eq!(fixture.script.dropped.load(Ordering::SeqCst), 0);
    *fixture.script.gate.lock().unwrap() = Some(CancellationToken::new());
    let mut third = tool
        .handler
        .execute(call(), context(), CancellationToken::new());
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(futures_util::poll!(&mut third).is_pending());
            if fixture.script.auxiliary.lock().unwrap().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(third);
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.script.dropped.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.script.dropped.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compacted_image_resolves_from_history_after_the_index_is_rebuilt() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach(&fixture, &session, "main").await;
    session
        .submit("retain this later turn".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    idle(&session, "main").await;
    let events = branch_events(&session, "main", ..).await.unwrap();
    let retain_from = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .nth(1)
        .unwrap()
        .id
        .clone();
    let timeline = fixture.histories.lock().unwrap()[&(session.id().into(), "main".into())].clone();
    timeline
        .append(
            EventKind::ContextCompaction(ContextCompactionData {
                turn_id: None,
                request_id: None,
                command: None,
                start: None,
                through: session.sequence("main").unwrap(),
                outcome: ContextCompactionResult::Completed {
                    retain_from: retain_from.clone(),
                    replacement: vec![Message::text(Role::User, "Image received earlier.")],
                    strategy: "fixture".into(),
                    context_baseline: vec![],
                    replacement_parts: None,
                    pinned_inputs: None,
                    digest_entries: None,
                    diagnostics: Box::new(CompactionDiagnosticsRecord {
                        reason: CompactionReason::Manual,
                        trigger: CompactionTrigger::Manual,
                        retain_from: Some(retain_from),
                        retain_limit: None,
                        estimated_input_tokens: 100.,
                        estimated_output_tokens: Some(10.),
                        entries_replaced: 1,
                        model_call_count: 0,
                        duration_ms: 0,
                        model_usage: None,
                        insufficient_space: None,
                        strategy: CompactionStrategyRecord {
                            kind: "fixture".into(),
                            details: JsonObject::new(),
                        },
                    }),
                },
            })
            .into(),
        )
        .unwrap();
    session
        .submit(
            "check the shortened window".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    idle(&session, "main").await;
    {
        let requests = fixture.script.requests.lock().unwrap();
        let messages = serde_json::to_string(&requests.last().unwrap().messages).unwrap();
        assert!(messages.contains("Image received earlier."));
        assert!(!messages.contains(REFERENCE));
        assert!(!messages.contains("fileId"));
    }
    assert!(session.unload_if_idle().await.unwrap());
    let session = fixture
        .server
        .agent()
        .unwrap()
        .restore_session(session.id().into())
        .await
        .unwrap();
    assert_eq!(
        text(
            &fixture
                .ask(&session, "main", "after-compaction", question())
                .await
        ),
        "answer from alpha"
    );
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    fixture.close().await;
}

/// A standalone call has no history a content reference could resolve against,
/// so both native xAI routes refuse a WebP before they send anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_the_native_xai_target_cannot_take_fails_without_a_request() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach_image(
        &fixture,
        &session,
        "main",
        session_model(&session),
        "attached.webp",
        WEBP,
        "image/webp",
    )
    .await;
    for driver in NATIVE_XAI {
        let provider = fixture.native_xai(driver).await;
        fixture.configure_to(&provider, Some("grok-vision")).await;
        let result = fixture
            .ask(
                &session,
                "main",
                driver,
                json!({"image":webp_reference(),"question":"What is visible?"}),
            )
            .await;
        assert_eq!(result.is_error, Some(true), "{driver}: {result:?}");
        assert!(
            text(&result).starts_with("image_understand_failed:"),
            "{driver}: {result:?}"
        );
        assert!(
            fixture.script.auxiliary.lock().unwrap().is_empty(),
            "the {driver} route receives no request without the image"
        );
    }
    fixture.close().await;
}

/// The built-in `xai` target is an xAI destination served over the compatible
/// protocol, so a WebP fails the standalone call before it sends anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_the_builtin_xai_target_cannot_take_fails_without_a_request() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach_image(
        &fixture,
        &session,
        "main",
        session_model(&session),
        "attached.webp",
        WEBP,
        "image/webp",
    )
    .await;
    fixture.builtin_xai().await;
    fixture.configure_to("xai", Some("grok-4.7")).await;
    let result = fixture
        .ask(
            &session,
            "main",
            "webp",
            json!({"image":webp_reference(),"question":"What is visible?"}),
        )
        .await;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert!(
        text(&result).starts_with("image_understand_failed:"),
        "{result:?}"
    );
    assert!(
        fixture.script.auxiliary.lock().unwrap().is_empty(),
        "the built-in xai route receives no request without the image"
    );
    fixture.close().await;
}

/// An answer already committed for the built-in `xai` target is not reused for
/// an image the destination does not take: the reference names the content, so
/// the same bytes re-attached as a WebP still find the committed answer, and the
/// format rule of the route that serves the target judges it first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_committed_for_the_builtin_xai_target_is_not_reused_for_an_unsupported_format() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    attach_image(
        &fixture,
        &session,
        "main",
        session_model(&session),
        "attached.png",
        PNG,
        "image/png",
    )
    .await;
    fixture.builtin_xai().await;
    fixture.configure_to("xai", Some("grok-4.7")).await;
    assert_eq!(
        text(&fixture.ask(&session, "main", "png", question()).await),
        "answer from grok-4.7"
    );
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 1);
    attach_image(
        &fixture,
        &session,
        "main",
        session_model(&session),
        "attached.webp",
        PNG,
        "image/webp",
    )
    .await;
    let refused = fixture.ask(&session, "main", "webp", question()).await;
    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    assert!(
        text(&refused).starts_with("image_understand_failed:"),
        "{refused:?}"
    );
    assert_eq!(
        fixture.script.auxiliary.lock().unwrap().len(),
        1,
        "the refused reuse sends no request"
    );
    fixture.close().await;
}

/// An answer stamped on a route that took the image is not reused after the
/// same provider and model id move to a route that does not, while a format the
/// new route still takes keeps its answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_is_not_reused_after_the_route_stops_taking_the_image_format() {
    let fixture = Fixture::new().await;
    fixture.routed().await;
    let session = fixture.session().await;
    attach(&fixture, &session, "main").await;
    attach_gif(&fixture, &session, "main").await;
    fixture.configure_to(ROUTED_PROVIDER, Some("routed")).await;
    assert_eq!(
        text(&fixture.ask(&session, "main", "png", question()).await),
        "answer from routed"
    );
    assert_eq!(
        text(&fixture.ask(&session, "main", "gif", gif_question()).await),
        "answer from routed"
    );
    assert_eq!(fixture.script.auxiliary.lock().unwrap().len(), 2);
    fixture.move_route("gemini").await;
    assert_eq!(
        text(&fixture.ask(&session, "main", "png-again", question()).await),
        "answer from routed",
        "a format the new route takes keeps its completed answer"
    );
    let refused = fixture
        .ask(&session, "main", "gif-again", gif_question())
        .await;
    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    assert!(
        text(&refused).starts_with("image_understand_failed:"),
        "{refused:?}"
    );
    assert_eq!(
        fixture.script.auxiliary.lock().unwrap().len(),
        2,
        "the refused reuse sends no request"
    );
    fixture.close().await;
}

/// A call that arrives after the route moved is not handed the answer another
/// caller's older route is still producing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_in_flight_is_not_shared_with_a_route_that_cannot_take_the_image() {
    let fixture = Fixture::new().await;
    fixture.routed().await;
    let session = fixture.session().await;
    attach_gif(&fixture, &session, "main").await;
    fixture.configure_to(ROUTED_PROVIDER, Some("routed")).await;
    let gate = CancellationToken::new();
    *fixture.script.gate.lock().unwrap() = Some(gate.clone());
    let tool = fixture.tool(&session);
    let context = || ToolExecutionContext {
        branch_id: "main".into(),
        request_id: "fixture".into(),
        turn_id: "fixture".into(),
    };
    let call = || ToolCall {
        id: "fixture".into(),
        name: "image_understand".into(),
        input: gif_question().as_object().unwrap().clone(),
    };
    let first = tool
        .handler
        .execute(call(), context(), CancellationToken::new());
    tokio::pin!(first);
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.script.auxiliary.lock().unwrap().is_empty() {
            assert!(futures_util::poll!(&mut first).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first call reaches the route and waits for its answer");
    fixture.move_route("gemini").await;
    let selections = fixture.script.selections.load(Ordering::SeqCst);
    let second = tool
        .handler
        .execute(call(), context(), CancellationToken::new());
    tokio::pin!(second);
    let second = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let std::task::Poll::Ready(result) = futures_util::poll!(&mut second) {
                break result;
            }
            if fixture.script.selections.load(Ordering::SeqCst) > selections {
                // The second call has read the moved route; let the answer in
                // flight settle so the call returns either way.
                gate.cancel();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second call settles while the first answer is in flight")
    .unwrap();
    assert_eq!(second.is_error, Some(true), "{second:?}");
    assert!(
        text(&second).starts_with("image_understand_failed:"),
        "{second:?}"
    );
    assert_eq!(
        fixture.script.auxiliary.lock().unwrap().len(),
        1,
        "the second call neither joins the flight nor sends"
    );
    gate.cancel();
    assert_eq!(text(&first.await.unwrap()), "answer from routed");
    fixture.close().await;
}
