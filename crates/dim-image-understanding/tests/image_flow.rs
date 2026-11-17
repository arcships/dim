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
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "id":"answer","object":"chat.completion.chunk","created":0,"model":request["model"],
            "choices":[{"index":0,"delta":{"content":format!("answer from {}",request["model"].as_str().unwrap())},"finish_reason":reason}]
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
                    accepted = listener.accept() => { calls.spawn(provider(accepted.unwrap().0, remote.clone())); }
                    completed = calls.join_next(), if !calls.is_empty() => { completed.unwrap().unwrap(); }
                }
            }
        });
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(Arc::new(MemoryTimelineStore::default()));
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
                {"modelId":"beta","displayName":"beta","status":"active","capabilities":vision}
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
        let revision = self.rpc("modality.capabilities", json!({})).await["revision"].clone();
        self.rpc("modality.configure", json!({"expectedRevision":revision,"category":"image","operation":"understand","model":model.map(|id| json!({"providerId":PROVIDER,"modelId":id}))})).await;
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
async fn attach(fixture: &Fixture, session: &Session, branch: &str) {
    use base64::Engine;
    let stored = fixture
        .server
        .service::<SessionFiles>()
        .unwrap()
        .0
        .put(
            session.id().into(),
            "attached.png".into(),
            base64::engine::general_purpose::STANDARD
                .decode(PNG)
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    session
        .configure(
            ConfigureUpdate {
                model: Some(ModelSelection {
                    metadata: Some(RequestMetadata {
                        session_id: Some(session.id().into()),
                        ..Default::default()
                    }),
                    ..SessionConfig::default().model
                }),
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
fn question() -> Value {
    json!({"image":REFERENCE, "question":"What is visible?"})
}
fn text(result: &ToolResult) -> &str {
    let ToolContent::Text(text) = &result.content else {
        panic!("text answer")
    };
    text
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
        assert!(requests[0].messages.iter().any(|m| matches!(&m.content, MessageContent::Parts(parts) if parts.iter().any(|p| matches!(p, MessagePart::Text { text, .. } if text == REFERENCE)))));
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
