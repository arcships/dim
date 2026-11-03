//! PromptSuggestion through the Session methods, a scripted model and the
//! Session broadcast.
use super::*;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use dim_agent_capabilities::adapters::DefaultPrepareModelPlugin;
use dim_agent_capabilities::defaults::{self as capability_defaults};
use dim_agent_capabilities::history::HistoryRenderer;
use dim_agent_capabilities::host::{
    ContextModel, HostError, HostFuture, IdentityContext, ModelChunk, ModelDriver, ModelHost,
    ModelStopReason, ModelStream, PrepareContextHost, ResolveContextModel,
};
use dim_agent_capabilities::model_provider::ModelProviderPlugin;
use dim_agent_capabilities::prompt_suggestion;
use dim_agent_capabilities::timeline_tree::BranchToolConfig;
use dim_agent_capabilities::workspace_tools::WorkspaceToolsConfig;
use dim_agent_types::{EventKind, MessageContent, MessagePart, ModelRequest, Role};
use std::sync::Mutex;

#[derive(Clone)]
enum Reply {
    Text(String),
    Chunks(Vec<String>),
    Thinking,
    ThinkingThenText { thinking: String, text: String },
}

struct Scripted {
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    reply: Mutex<Reply>,
    main: Mutex<Reply>,
    hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    hold_main: Mutex<Option<Arc<tokio::sync::Notify>>>,
    cancellations: std::sync::atomic::AtomicUsize,
}

impl Scripted {
    fn new(reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            reply: Mutex::new(reply),
            main: Mutex::new(Reply::Text("done".into())),
            hold: Mutex::new(None),
            hold_main: Mutex::new(None),
            cancellations: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn set_reply(&self, reply: Reply) {
        *self.reply.lock().unwrap() = reply;
    }
    fn set_main(&self, reply: Reply) {
        *self.main.lock().unwrap() = reply;
    }
    /// The suggestion requests so far.
    fn suggestions(&self) -> Vec<Arc<ModelRequest>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| is_suggestion(request))
            .cloned()
            .collect()
    }
    /// Waits until `count` suggestion requests have reached the driver.
    async fn wait_suggestions(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.suggestions().len() < count {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the suggestion reaches the model");
    }
    fn cancellations(&self) -> usize {
        self.cancellations.load(std::sync::atomic::Ordering::SeqCst)
    }
    async fn wait_cancellations(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.cancellations() < count {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the held call is cancelled");
    }
    fn holds_next(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        *self.hold.lock().unwrap() = Some(gate.clone());
        gate
    }
    /// Waits until the turn has made `count` model calls.
    async fn wait_main_calls(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let calls = self
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| !is_suggestion(request))
                    .count();
                if calls >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the turn reaches the model");
    }
    /// Holds the next turn's model call, as [`Scripted::holds_next`] does for
    /// a suggestion, until the gate is notified or the call is cancelled.
    fn holds_next_main(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        *self.hold_main.lock().unwrap() = Some(gate.clone());
        gate
    }
}

fn last_text(request: &ModelRequest) -> Option<String> {
    match &request.messages.last()?.content {
        MessageContent::Text(text) => Some(text.clone()),
        MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.clone()),
            _ => None,
        }),
    }
}

fn is_suggestion(request: &ModelRequest) -> bool {
    last_text(request).is_some_and(|text| text.starts_with("You predict the next message"))
}

impl ModelDriver for Scripted {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                self.cancellations
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(HostError::Cancelled("cancelled".into()));
            }
            self.requests.lock().unwrap().push(request.clone());
            let hold = if is_suggestion(&request) {
                self.hold.lock().unwrap().take()
            } else {
                self.hold_main.lock().unwrap().take()
            };
            if let Some(gate) = hold {
                tokio::select! {
                    _ = gate.notified() => {}
                    _ = cancel.cancelled() => {
                        self.cancellations
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Err(HostError::Cancelled("cancelled".into()));
                    }
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            let reply = if is_suggestion(&request) {
                self.reply.lock().unwrap().clone()
            } else {
                self.main.lock().unwrap().clone()
            };
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let reason = match reply {
                Reply::Text(text) => {
                    chunks.push(Ok(ModelChunk::Text(text.clone())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    chunks.push(Ok(ModelChunk::Content(vec![MessagePart::Text {
                        provider_metadata: None,
                        text,
                        start_time: None,
                        end_time: None,
                    }])));
                    ModelStopReason::EndTurn
                }
                Reply::Chunks(chunks_text) => {
                    for text in &chunks_text {
                        chunks.push(Ok(ModelChunk::Text(text.clone())));
                    }
                    chunks.push(Ok(ModelChunk::TextEnd));
                    chunks.push(Ok(ModelChunk::Content(vec![MessagePart::Text {
                        provider_metadata: None,
                        text: chunks_text.concat(),
                        start_time: None,
                        end_time: None,
                    }])));
                    ModelStopReason::EndTurn
                }
                Reply::ThinkingThenText { thinking, text } => {
                    chunks.push(Ok(ModelChunk::Thinking(thinking.clone())));
                    chunks.push(Ok(ModelChunk::ThinkingEnd {
                        thinking: thinking.clone(),
                    }));
                    chunks.push(Ok(ModelChunk::Text(text.clone())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    ModelStopReason::EndTurn
                }
                Reply::Thinking => {
                    chunks.push(Ok(ModelChunk::Thinking("considering".into())));
                    chunks.push(Ok(ModelChunk::ThinkingEnd {
                        thinking: "considering".into(),
                    }));
                    chunks.push(Ok(ModelChunk::Content(vec![MessagePart::Thinking {
                        provider_metadata: None,
                        thinking: "considering".into(),
                        start_time: None,
                        end_time: None,
                    }])));
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

struct Fixture {
    server: RunningServer,
    home: std::path::PathBuf,
    model: Arc<Scripted>,
    provisioned: std::sync::atomic::AtomicBool,
}

/// The local-execution assembly with a scripted local model.
async fn serve_fixture(reply: Reply) -> Fixture {
    use std::os::unix::fs::DirBuilderExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("ps-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    capability_defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: home.join("rust").join("outputs"),
        },
        BranchToolConfig::default(),
    );
    prompt_suggestion::install_local(&mut builder);
    dim_app_server::permission_rules::install(&mut builder);
    let model = Scripted::new(reply);
    capability_defaults::mock_model_hosts_with_model(
        &mut builder,
        HistoryRenderer::default(),
        model.clone(),
    );
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            unix_path: Some(home.join("daemon.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Fixture {
        server,
        home,
        model,
        provisioned: std::sync::atomic::AtomicBool::new(false),
    }
}

impl Fixture {
    fn work(&self) -> std::path::PathBuf {
        self.home.join("work")
    }
    async fn end(self) {
        self.server.shutdown();
        self.server.join().await.unwrap();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Window facts every Session's context exceeds at once. The gate only reads
/// whether the head is over its threshold, so the numbers can be tiny.
struct TinyWindow;
impl ResolveContextModel for TinyWindow {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async { Ok(Arc::new(Tiny) as Arc<dyn ContextModel>) })
    }
}
struct Tiny;
impl ContextModel for Tiny {
    fn context_window(&self) -> f64 {
        1_000.
    }
    fn output_tokens(&self) -> f64 {
        100.
    }
    fn threshold_tokens(&self) -> f64 {
        10.
    }
    fn count_tokens(&self, _: &ModelRequest) -> f64 {
        50.
    }
}

/// A window with room to spare: a request's usage check passes once the
/// resolver returns it.
struct Room;
impl ContextModel for Room {
    fn context_window(&self) -> f64 {
        100_000.
    }
    fn output_tokens(&self) -> f64 {
        4_096.
    }
    fn threshold_tokens(&self) -> f64 {
        50_000.
    }
    fn count_tokens(&self, _: &ModelRequest) -> f64 {
        10.
    }
}

/// A window whose next resolution the test can hold, to pin a request inside
/// its preflight.
#[derive(Default)]
struct Held {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    /// Answer with the window the head is already past, so the preflight
    /// refuses for compaction once it is released.
    tiny: std::sync::atomic::AtomicBool,
}

impl Held {
    /// A resolver that holds the next resolution and then answers with a
    /// window the head is already past.
    fn holding_tiny() -> Self {
        Self {
            tiny: std::sync::atomic::AtomicBool::new(true),
            ..Self::default()
        }
    }
    /// Holds the next resolution.
    fn hold(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    /// Resolves once a resolution waits at the barrier.
    async fn entered(&self) {
        self.entered.notified().await;
    }
    fn release(&self) {
        self.release.notify_one();
    }
}

impl ResolveContextModel for Held {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move {
            if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if self.tiny.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(Arc::new(Tiny) as Arc<dyn ContextModel>)
            } else {
                Ok(Arc::new(Room) as Arc<dyn ContextModel>)
            }
        })
    }
}

/// [`serve_fixture`] with the product's compaction installed.
async fn serve_compaction_fixture(reply: Reply) -> Fixture {
    serve_compaction_fixture_with(reply, Arc::new(TinyWindow)).await
}

/// [`serve_compaction_fixture`] with the window resolver the test controls.
async fn serve_compaction_fixture_with(
    reply: Reply,
    resolver: Arc<dyn ResolveContextModel>,
) -> Fixture {
    use std::os::unix::fs::DirBuilderExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("ps-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    std::fs::create_dir_all(home.join("work")).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    capability_defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: home.join("rust").join("outputs"),
        },
        BranchToolConfig::default(),
    );
    prompt_suggestion::install_local(&mut builder);
    dim_app_server::permission_rules::install(&mut builder);
    let model = Scripted::new(reply);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .process_value(Arc::new(ModelHost(model.clone())))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    capability_defaults::install_compaction(&mut builder, resolver, Default::default());
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            unix_path: Some(home.join("daemon.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Fixture {
        server,
        home,
        model,
        provisioned: std::sync::atomic::AtomicBool::new(false),
    }
}

/// Creates the provider the Session's model selection resolves against; the
/// scripted model host never opens its connection.
async fn provision(client: &mut Client) {
    let caps = dim_agent_types::CatalogCapabilities::with_limits(32_000, 4_096);
    let provider = uuid::Uuid::new_v4().to_string();
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId":provider,"displayName":"Fixture",
                    "connection":{"driverKind":"openai-compatible","baseUrl":"https://model.example/v1",
                        "auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture"}},
                    "models":[{"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps}],
                    "enabled":true}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":provider,"modelId":"alpha"}),
            )
            .await,
    );
}

async fn create(fixture: &Fixture, interactive: bool) -> (Client, String) {
    let mut client = Client::new_version(&fixture.server, false, 3).await;
    if !fixture
        .provisioned
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        provision(&mut client).await;
    }
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":fixture.work(),"interactive":interactive,
                "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    (client, id)
}

/// Sends `text` as input without waiting for the run it may start.
async fn send(client: &mut Client, id: &str, text: &str, mode: Option<&str>) {
    let mut params = json!({"sessionId":id,"branchId":"main","text":text});
    if let Some(mode) = mode {
        params["mode"] = json!(mode);
    }
    assert_ok(&client.rpc("session.submit", params).await);
}

async fn idle(client: &mut Client, id: &str) {
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
}

async fn submit(client: &mut Client, id: &str, text: &str) {
    send(client, id, text, None).await;
    idle(client, id).await;
}

/// A second client attached to `session_id`, for a call that runs while the
/// first one waits.
async fn attach(fixture: &Fixture, session_id: &str) -> Client {
    let mut client = Client::new_version(&fixture.server, false, 3).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    client
}

/// The `turnId` of the newest `turn/end` on `main`.
async fn latest_turn(client: &mut Client, id: &str) -> String {
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&history);
    history["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["type"] == "turn" && event["name"] == "end")
        .map(|event| event["data"]["turn_id"].as_str().unwrap().to_owned())
        .expect("a completed turn")
}

async fn configure(client: &mut Client, document: Value) -> Value {
    let read = client
        .rpc(
            "settings.read",
            json!({"source":"prompt-suggestion","scope":"user"}),
        )
        .await;
    assert_ok(&read);
    let updated = client
        .rpc(
            "settings.update",
            json!({"source":"prompt-suggestion","scope":"user",
                "expectedRevision":read["result"]["revision"],"document":document}),
        )
        .await;
    assert_ok(&updated);
    updated
}

/// One `session.notice` from PromptSuggestion of `kind`, waiting for it.
async fn suggestion_notice(client: &mut Client, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(index) = client.notices.iter().position(|value| {
                value["method"] == "session.notice"
                    && value["params"]["source"] == "PromptSuggestion"
                    && value["params"]["kind"] == kind
            }) {
                return client.notices.remove(index);
            }
            let value = client.read("a PromptSuggestion notice").await;
            client.notices.push(value);
        }
    })
    .await
    .expect("the suggestion notice arrives")
}

/// Asserts no PromptSuggestion notice of `kind` arrives: a request that
/// never started announces nothing.
async fn no_suggestion_notice(client: &mut Client, kind: &str) {
    let notice =
        tokio::time::timeout(Duration::from_millis(300), suggestion_notice(client, kind)).await;
    assert!(
        notice.is_err(),
        "a {kind} notice arrived for a request that never started: {:?}",
        notice.ok()
    );
}

async fn suggest(client: &mut Client, id: &str, turn: &str) -> Value {
    let reply = client
        .rpc(
            "session.suggestion.request",
            json!({"sessionId":id,"turnId":turn}),
        )
        .await;
    assert_ok(&reply);
    reply["result"].clone()
}

async fn respond(client: &mut Client, id: &str, turn: &str, action: &str) {
    let reply = client
        .rpc(
            "session.suggestion.respond",
            json!({"sessionId":id,"turnId":turn,"action":action}),
        )
        .await;
    assert_ok(&reply);
    assert_eq!(reply["result"], json!({}));
}

/// The branch's event ids, in order.
async fn timeline(fixture: &Fixture, id: &str) -> Vec<String> {
    let session = fixture.server.agent().unwrap().get_session(id).unwrap();
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .map(|event| event.id.clone())
        .collect()
}

fn serialized(value: impl serde::Serialize) -> String {
    serde_json::to_string(&value).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_suggestion_reuses_the_turn_request_and_changes_nothing() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    let listed = client
        .rpc("session.methods", json!({"sessionId": id}))
        .await;
    assert_ok(&listed);
    let names: Vec<&str> = listed["result"]["methods"]
        .as_array()
        .expect("a method table")
        .iter()
        .map(|decl| decl["name"].as_str().expect("a method name"))
        .collect();
    for method in ["session.suggestion.request", "session.suggestion.respond"] {
        assert!(
            names.contains(&method),
            "session.methods lists {method}: {listed}"
        );
    }
    fixture.model.set_main(Reply::ThinkingThenText {
        thinking: "considering".into(),
        text: "done".into(),
    });
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    let session = fixture.server.agent().unwrap().get_session(&id).unwrap();
    let events = session.local("main", ..).await.unwrap();
    let call = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.clone()),
            _ => None,
        })
        .expect("a model call");
    let before = timeline(&fixture, &id).await;

    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["sessionId"], json!(id));
    assert_eq!(offer["params"]["data"]["turnId"], json!(turn));
    assert_eq!(offer["params"]["data"]["text"], "run the tests");

    let suggestion = fixture.model.suggestions();
    assert_eq!(suggestion.len(), 1, "one model call for one suggestion");
    let suggestion = &suggestion[0];
    assert_eq!(suggestion.provider_id, call.request.provider_id);
    assert_eq!(suggestion.model_id, call.request.model_id);
    assert_eq!(
        suggestion.capability_snapshot,
        call.request.capability_snapshot
    );
    assert_eq!(
        serialized(&suggestion.system),
        serialized(&call.request.system)
    );
    assert_eq!(
        serialized(&suggestion.tools),
        serialized(&call.request.tools)
    );
    assert_eq!(
        serialized(&suggestion.config),
        serialized(&call.request.config)
    );
    let prefix = call.request.messages.len();
    assert_eq!(suggestion.messages.len(), prefix + 2);
    for (index, message) in call.request.messages.iter().enumerate() {
        assert_eq!(
            serialized(&suggestion.messages[index]),
            serialized(message),
            "message {index} differs from the turn's"
        );
    }
    assert_eq!(
        suggestion.messages[prefix].role,
        Role::Assistant,
        "the turn's assistant message follows the prefix"
    );
    let MessageContent::Parts(parts) = &suggestion.messages[prefix].content else {
        panic!("the assistant message carries the turn's parts");
    };
    assert!(
        matches!(&parts[0], MessagePart::Thinking { thinking, .. } if thinking == "considering"),
        "the streamed thinking is kept, as history keeps it: {parts:?}"
    );
    assert!(
        matches!(&parts[1], MessagePart::Text { text, .. } if text == "done"),
        "the streamed text follows: {parts:?}"
    );
    assert_eq!(suggestion.messages[prefix + 1].role, Role::User);
    let instruction = last_text(suggestion).unwrap();
    assert_eq!(instruction, prompt_suggestion::instruction(80));
    let metadata = suggestion.metadata.as_ref().expect("request metadata");
    let identity = metadata.extensions["requestId"]
        .as_str()
        .expect("the suggestion's own request identity");
    assert_ne!(
        identity, call.request_id,
        "the suggestion is never the turn's call"
    );
    assert_eq!(
        timeline(&fixture, &id).await,
        before,
        "the Timeline is unchanged"
    );

    respond(&mut client, &id, &turn, "accept").await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(cleared["params"]["data"]["reason"], "accepted");

    submit(&mut client, &id, "and the next one").await;
    let last = {
        let events = session.local("main", ..).await.unwrap();
        events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data.request.clone()),
                _ => None,
            })
            .expect("another model call")
    };
    assert!(
        !last.messages.iter().any(|message| {
            matches!(&message.content, MessageContent::Text(text) if text == &instruction)
        }),
        "the suggestion never entered history"
    );
    assert_eq!(
        last.messages.len(),
        prefix + 2,
        "assistant and the new input"
    );
    assert_eq!(
        serialized(&last.messages[prefix]),
        serialized(&suggestion.messages[prefix]),
        "the completed assistant message is history's own"
    );
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gates_refuse_before_a_request_starts() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;

    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "disabled"})
    );

    configure(&mut client, json!({"enabled":true})).await;
    submit(&mut client, &id, "and another").await;
    let latest = latest_turn(&mut client, &id).await;
    assert_ne!(latest, turn);
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "not_latest_turn"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");

    let (mut member, member_id) = create(&fixture, false).await;
    submit(&mut member, &member_id, "hello").await;
    let member_turn = latest_turn(&mut member, &member_id).await;
    assert_eq!(
        suggest(&mut member, &member_id, &member_turn).await,
        json!({"accepted": false, "reason": "not_root"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    fixture.end().await;
}

/// A turn the user steered while it ran still offers, because a steer is the
/// running turn's own input; input written after the turn's end refuses that
/// completed turn as `not_latest_turn`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steered_turn_offers_and_input_after_its_end_refuses() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    configure(&mut client, json!({"enabled":true})).await;

    fixture.model.holds_next_main();
    send(&mut client, &id, "start the work", None).await;
    fixture.model.wait_main_calls(1).await;
    send(&mut client, &id, "and also this", Some("steer")).await;
    // The steer replaces the held call: the turn asks the model again with
    // the steer folded in, then completes.
    fixture.model.wait_main_calls(2).await;
    idle(&mut client, &id).await;
    let turn = latest_turn(&mut client, &id).await;

    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true}),
        "the steer is the running turn's input"
    );
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "run the tests");
    respond(&mut client, &id, &turn, "accept").await;
    suggestion_notice(&mut client, "cleared").await;

    // An appended input starts no turn, so the completed turn stays the
    // latest one; the input that followed its end refuses it.
    send(&mut client, &id, "a note", Some("append")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "not_latest_turn"})
    );
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_head_past_the_compaction_threshold_is_not_asked() {
    let fixture = serve_compaction_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    let usage = client
        .rpc(
            "session.context.usage",
            json!({"sessionId":id,"branchId":"main"}),
        )
        .await;
    assert_ok(&usage);
    assert_eq!(usage["result"]["estimatedInputTokens"], json!(50.0));
    assert_eq!(usage["result"]["thresholdTokens"], json!(10.0));

    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "compaction_due"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    assert!(client.notices.iter().all(|notice| {
        notice["method"] != "session.notice" || notice["params"]["source"] != "PromptSuggestion"
    }));
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_without_text_offers_nothing() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    fixture.model.set_main(Reply::Thinking);
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "no_text"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_request_in_flight_is_shared() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    let gate = fixture.model.holds_next();
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true}),
        "a second request for the same turn shares the first"
    );
    gate.notify_one();
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "run the tests");
    assert_eq!(fixture.model.suggestions().len(), 1, "one model call");
    fixture.end().await;
}

/// Waits until the part records that `asking` waits on another request's
/// preflight, with its receiver installed, so the caller is ordered before it
/// changes anything. An early answer fails the test: that is the bug it pins.
async fn ordered_sharing<F>(id: &str, mut asking: std::pin::Pin<&mut F>)
where
    F: std::future::Future<Output = Value>,
{
    let seen = prompt_suggestion::testing::shared_requests(id);
    tokio::select! {
        () = prompt_suggestion::testing::shared_after(id, seen) => {}
        answer = asking.as_mut() => {
            panic!("the sharer answered before its preflight settled: {answer:?}");
        }
    }
}

/// A request that shares a preflight which turns off is refused as its owner
/// is: it is never accepted for a model call that does not start, and
/// nothing announces.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sharer_answers_disabled_when_the_shared_preflight_is_disabled() {
    let held = Arc::new(Held::default());
    let fixture =
        serve_compaction_fixture_with(Reply::Text("run the tests".into()), held.clone()).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;
    let mut other = attach(&fixture, &id).await;
    let mut writer = attach(&fixture, &id).await;

    held.hold();
    let asked = (id.clone(), turn.clone());
    let owner = tokio::spawn(async move { suggest(&mut client, &asked.0, &asked.1).await });
    held.entered().await;
    let mut asking = Box::pin(suggest(&mut other, &id, &turn));
    ordered_sharing(&id, asking.as_mut()).await;
    // The write reaches the part while the owner is still held and the
    // sharer already waits on its preflight.
    configure(&mut writer, json!({"enabled":false})).await;
    held.release();
    assert_eq!(
        asking.await,
        json!({"accepted": false, "reason": "disabled"})
    );
    assert_eq!(
        owner.await.unwrap(),
        json!({"accepted": false, "reason": "disabled"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    no_suggestion_notice(&mut other, "offer").await;
    no_suggestion_notice(&mut other, "cleared").await;
    fixture.end().await;
}

/// A request that shares a preflight which refuses for compaction answers
/// `compaction_due`, exactly as its owner does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sharer_answers_compaction_due_when_the_shared_preflight_refuses() {
    let held = Arc::new(Held::holding_tiny());
    let fixture =
        serve_compaction_fixture_with(Reply::Text("run the tests".into()), held.clone()).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;
    let mut other = attach(&fixture, &id).await;

    held.hold();
    let asked = (id.clone(), turn.clone());
    let owner = tokio::spawn(async move { suggest(&mut client, &asked.0, &asked.1).await });
    held.entered().await;
    let mut asking = Box::pin(suggest(&mut other, &id, &turn));
    ordered_sharing(&id, asking.as_mut()).await;
    held.release();
    assert_eq!(
        asking.await,
        json!({"accepted": false, "reason": "compaction_due"})
    );
    assert_eq!(
        owner.await.unwrap(),
        json!({"accepted": false, "reason": "compaction_due"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    no_suggestion_notice(&mut other, "offer").await;
    no_suggestion_notice(&mut other, "cleared").await;
    fixture.end().await;
}

/// A request that shares a preflight which starts is accepted once the model
/// call starts: one call for the turn and one offer, and both callers answer
/// accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sharer_is_accepted_when_the_shared_preflight_starts() {
    let held = Arc::new(Held::default());
    let fixture =
        serve_compaction_fixture_with(Reply::Text("run the tests".into()), held.clone()).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;
    let mut other = attach(&fixture, &id).await;

    held.hold();
    let asked = (id.clone(), turn.clone());
    let owner = tokio::spawn(async move { suggest(&mut client, &asked.0, &asked.1).await });
    held.entered().await;
    let mut asking = Box::pin(suggest(&mut other, &id, &turn));
    ordered_sharing(&id, asking.as_mut()).await;
    held.release();
    assert_eq!(asking.await, json!({"accepted": true}));
    assert_eq!(owner.await.unwrap(), json!({"accepted": true}));
    let offer = suggestion_notice(&mut other, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "run the tests");
    assert_eq!(fixture.model.suggestions().len(), 1, "one model call");
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissal_and_a_new_input_cancel_a_request() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    let gate = fixture.model.holds_next();
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    fixture.model.wait_suggestions(1).await;
    respond(&mut client, &id, &turn, "dismiss").await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(cleared["params"]["data"]["reason"], "dismissed");
    fixture.model.wait_cancellations(1).await;
    drop(gate);

    let gate = fixture.model.holds_next();
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    fixture.model.wait_suggestions(2).await;
    submit(&mut client, &id, "a new message").await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(cleared["params"]["data"]["reason"], "new_input");
    fixture.model.wait_cancellations(2).await;
    drop(gate);
    assert_eq!(fixture.model.suggestions().len(), 2, "one model call each");
    fixture.end().await;
}

/// A request cancelled while it returns cannot settle the next request for
/// the same turn: the predecessor's turn id matches, its invocation does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_request_cannot_settle_a_new_one_for_the_same_turn() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    // A finishes its model call and waits at the barrier, so its result is
    // still returning when B is accepted for the same turn.
    let completion = prompt_suggestion::testing::hold_next_completion(&id);
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    completion.entered().await;
    fixture
        .model
        .set_reply(Reply::Text("check the logs".into()));
    respond(&mut client, &id, &turn, "dismiss").await;
    let gate = fixture.model.holds_next();
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    // A returns after B took the slot: it changes nothing and announces
    // nothing, so B's offer is the only one.
    completion.release();
    completion.passed().await;
    gate.notify_one();
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "check the logs");
    assert_eq!(fixture.model.suggestions().len(), 2, "one model call each");
    fixture.end().await;
}

/// Turning the switch off while a request waits in its usage check starts no
/// model call and refuses the request with `disabled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_during_the_usage_check_starts_no_request() {
    let held = Arc::new(Held::default());
    let fixture =
        serve_compaction_fixture_with(Reply::Text("run the tests".into()), held.clone()).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;
    let mut other = attach(&fixture, &id).await;

    held.hold();
    let asked = (id.clone(), turn.clone());
    let request = tokio::spawn(async move { suggest(&mut client, &asked.0, &asked.1).await });
    held.entered().await;
    configure(&mut other, json!({"enabled":false})).await;
    held.release();
    assert_eq!(
        request.await.unwrap(),
        json!({"accepted": false, "reason": "disabled"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    fixture.end().await;
}

/// Dismissing while a request waits in its usage check cancels it: it starts
/// no model call and answers `dismissed`, so no offer follows the dismissal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dismissing_during_the_usage_check_starts_no_request() {
    let held = Arc::new(Held::default());
    let fixture =
        serve_compaction_fixture_with(Reply::Text("run the tests".into()), held.clone()).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;
    let mut other = attach(&fixture, &id).await;

    held.hold();
    let asked = (id.clone(), turn.clone());
    let request = tokio::spawn(async move { suggest(&mut client, &asked.0, &asked.1).await });
    held.entered().await;
    respond(&mut other, &id, &turn, "dismiss").await;
    held.release();
    assert_eq!(
        request.await.unwrap(),
        json!({"accepted": false, "reason": "dismissed"})
    );
    assert!(fixture.model.suggestions().is_empty(), "no model call");
    fixture.end().await;
}

/// A dismissal while the model call runs settles the request with `cleared`,
/// and the returning request emits nothing: no offer follows its clearing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissed_request_offers_nothing_after_its_clearing() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    let completion = prompt_suggestion::testing::hold_next_completion(&id);
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    completion.entered().await;
    respond(&mut client, &id, &turn, "dismiss").await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(cleared["params"]["data"]["reason"], "dismissed");
    completion.release();
    completion.passed().await;
    // A round trip drains anything the returning request sent.
    assert_ok(
        &client
            .rpc(
                "session.history",
                json!({"sessionId":id,"branchId":"main","after":0,"limit":1}),
            )
            .await,
    );
    assert!(
        client.notices.iter().all(|notice| {
            notice["method"] != "session.notice"
                || notice["params"]["source"] != "PromptSuggestion"
                || notice["params"]["kind"] != "offer"
        }),
        "an offer followed its clearing: {:?}",
        client.notices
    );
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_reply_clears_invalid() {
    let fixture = serve_fixture(Reply::Text(String::new())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    for reply in [
        "run the tests\nand then commit",
        "/clear",
        "@src/main.rs",
        "!ls",
        "<none>",
        "no suggestion",
        "I'll run the tests",
        "add a test",
        "",
    ] {
        fixture.model.set_reply(Reply::Text(reply.into()));
        assert_eq!(
            suggest(&mut client, &id, &turn).await,
            json!({"accepted": true})
        );
        let cleared = suggestion_notice(&mut client, "cleared").await;
        assert_eq!(
            cleared["params"]["data"]["reason"], "invalid",
            "{reply:?} was offered"
        );
    }
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reader_keeps_a_reply_the_same_however_it_is_chunked() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true,"maxChars":80})).await;

    fixture.model.set_reply(Reply::Chunks(vec![
        "run the tests\n".into(),
        "and commit".into(),
    ]));
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(
        cleared["params"]["data"]["reason"], "invalid",
        "a line break followed by text is never a reply"
    );

    fixture.model.set_reply(Reply::Text("x".repeat(81)));
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(
        cleared["params"]["data"]["reason"], "invalid",
        "one character over maxChars is never a reply"
    );

    let exact = "y".repeat(80);
    fixture.model.set_reply(Reply::Text(exact.clone()));
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], json!(exact));

    fixture
        .model
        .set_reply(Reply::Text("run the tests\n".into()));
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "run the tests");
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn twenty_unused_offers_stop_offering_until_an_accept() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let mut turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    for _ in 0..19 {
        assert_eq!(
            suggest(&mut client, &id, &turn).await,
            json!({"accepted": true})
        );
        suggestion_notice(&mut client, "offer").await;
        respond(&mut client, &id, &turn, "dismiss").await;
        suggestion_notice(&mut client, "cleared").await;
    }
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    suggestion_notice(&mut client, "offer").await;
    respond(&mut client, &id, &turn, "accept").await;
    suggestion_notice(&mut client, "cleared").await;

    for index in 0..20 {
        assert_eq!(
            suggest(&mut client, &id, &turn).await,
            json!({"accepted": true})
        );
        suggestion_notice(&mut client, "offer").await;
        if index % 2 == 0 {
            respond(&mut client, &id, &turn, "dismiss").await;
            suggestion_notice(&mut client, "cleared").await;
        } else {
            submit(&mut client, &id, "another message").await;
            let cleared = suggestion_notice(&mut client, "cleared").await;
            assert_eq!(
                cleared["params"]["data"]["reason"], "new_input",
                "a new input clears the shown offer"
            );
            turn = latest_turn(&mut client, &id).await;
        }
    }
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "throttled"})
    );
    assert_eq!(
        fixture.model.suggestions().len(),
        40,
        "no request while throttled"
    );
    fixture.end().await;
}

/// An accept before the limit starts the count over, and so does turning the
/// switch off and on: the pause a reached limit causes can be ended without
/// ending the Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accept_and_a_turn_on_reset_the_unused_count() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    for _ in 0..5 {
        assert_eq!(
            suggest(&mut client, &id, &turn).await,
            json!({"accepted": true})
        );
        suggestion_notice(&mut client, "offer").await;
        respond(&mut client, &id, &turn, "dismiss").await;
        suggestion_notice(&mut client, "cleared").await;
    }
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    suggestion_notice(&mut client, "offer").await;
    respond(&mut client, &id, &turn, "accept").await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(cleared["params"]["data"]["reason"], "accepted");

    // Twenty more unused offers reach the limit; the accept made room for
    // all of them.
    for _ in 0..20 {
        assert_eq!(
            suggest(&mut client, &id, &turn).await,
            json!({"accepted": true})
        );
        suggestion_notice(&mut client, "offer").await;
        respond(&mut client, &id, &turn, "dismiss").await;
        suggestion_notice(&mut client, "cleared").await;
    }
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": false, "reason": "throttled"})
    );
    let requests = fixture.model.suggestions().len();
    assert_eq!(requests, 26, "one model call per accepted request");

    let seen = prompt_suggestion::testing::settings_applied_count(&id);
    configure(&mut client, json!({"enabled":false})).await;
    configure(&mut client, json!({"enabled":true})).await;
    // The change listener reads the document in its own task and applies it
    // under the state lock: once its record names the on document, the count
    // is zero before a request can read it.
    prompt_suggestion::testing::settings_applied_after(&id, seen, true, 80).await;
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "run the tests");
    assert_eq!(fixture.model.suggestions().len(), requests + 1);
    fixture.end().await;
}

/// A `maxChars` rewrite leaves a shown offer alone, because it was valid when
/// the request was accepted; turning the switch off withdraws it with
/// `disabled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_max_chars_change_keeps_the_offer_and_disabling_clears_it() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true,"maxChars":80})).await;

    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    let offer = suggestion_notice(&mut client, "offer").await;
    assert_eq!(offer["params"]["data"]["text"], "run the tests");

    let seen = prompt_suggestion::testing::settings_applied_count(&id);
    configure(&mut client, json!({"enabled":true,"maxChars":120})).await;
    // The change listener reads the document in its own task and applies it
    // under the state lock: once its record names the new maxChars, a
    // withdrawal would have happened under that lock.
    prompt_suggestion::testing::settings_applied_after(&id, seen, true, 120).await;
    respond(&mut client, &id, &turn, "accept").await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(
        cleared["params"]["data"]["reason"], "accepted",
        "the offer survived a maxChars change"
    );

    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    suggestion_notice(&mut client, "offer").await;
    configure(&mut client, json!({"enabled":false,"maxChars":120})).await;
    let cleared = suggestion_notice(&mut client, "cleared").await;
    assert_eq!(cleared["params"]["data"]["reason"], "disabled");
    fixture.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_settings_source_is_user_only_and_range_checked() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, _) = create(&fixture, true).await;
    let read = client
        .rpc(
            "settings.read",
            json!({"source":"prompt-suggestion","scope":"user"}),
        )
        .await;
    assert_ok(&read);
    assert_eq!(
        read["result"]["document"],
        json!({"enabled": false, "maxChars": 80})
    );
    let revision = read["result"]["revision"].clone();
    for document in [
        json!({"enabled":true,"maxChars":5}),
        json!({"enabled":true,"maxChars":201}),
        json!({"enabled":true,"unknown":true}),
    ] {
        let refused = client
            .rpc(
                "settings.update",
                json!({"source":"prompt-suggestion","scope":"user",
                    "expectedRevision":revision,"document":document}),
            )
            .await;
        assert!(
            refused["error"].is_object(),
            "{document} was accepted: {refused}"
        );
    }
    let project = client
        .rpc(
            "settings.update",
            json!({"source":"prompt-suggestion","scope":{"project":0},
                "expectedRevision":revision,"document":{"enabled":true,"maxChars":80}}),
        )
        .await;
    assert!(project["error"].is_object(), "project scope was accepted");
    fixture.end().await;
}

/// A fake provider serving the three protocols on one port and recording
/// every request body.
#[derive(Default)]
struct Endpoint {
    bodies: std::sync::Mutex<Vec<(String, Value)>>,
}

fn sse(data: Value) -> String {
    format!("data: {data}\n\n")
}

/// The text one protocol's request asks for, whatever the shape.
fn requested_text(kind: &str, body: &Value) -> String {
    let messages = match kind {
        "anthropic" | "openai-compatible" => body["messages"].clone(),
        _ => body["input"].clone(),
    };
    messages
        .as_array()
        .and_then(|messages| messages.last())
        .map(|message| match &message["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Null => message["arguments"].as_str().unwrap_or("").to_owned(),
            _ => String::new(),
        })
        .unwrap_or_default()
}

fn reply_text(kind: &str, body: &Value) -> String {
    let asked = requested_text(kind, body);
    if asked.contains("You predict the next message") {
        "run the tests".into()
    } else {
        "done".into()
    }
}

async fn provider(
    State(endpoint): State<Arc<Endpoint>>,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let kind = match uri.path() {
        "/v1/messages" => "anthropic",
        "/v1/responses" => "openai-responses",
        _ => "openai-compatible",
    };
    endpoint
        .bodies
        .lock()
        .unwrap()
        .push((kind.to_owned(), body.clone()));
    let text = reply_text(kind, &body);
    let payload = match kind {
        "anthropic" => [
            json!({"type":"message_start","message":{"id":"msg_fixture","type":"message","role":"assistant","model":"fixture","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":11,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ]
        .iter()
        .map(|data| format!("event: {}\ndata: {data}\n\n", data["type"].as_str().unwrap()))
        .collect::<String>(),
        "openai-compatible" => {
            sse(json!({"id":"chat","choices":[{"index":0,"delta":{"reasoning_content":"considering"},"finish_reason":null}]}))
                + &sse(json!({"id":"chat","choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]}))
                + "data: [DONE]\n\n"
        }
        _ => [
            json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":"fixture"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}),
            json!({"type":"response.content_part.added","item_id":"msg-1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
            json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":0,"content_index":0,"delta":text}),
            json!({"type":"response.output_text.done","item_id":"msg-1","output_index":0,"content_index":0,"text":text}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","phase":"commentary","content":[]}}),
            json!({"type":"response.completed","response":{"id":"resp-1","created_at":1,"model":"fixture","status":"completed","usage":{"input_tokens":5,"output_tokens":3,"total_tokens":8}}}),
        ]
        .iter()
        .map(|data| sse(data.clone()))
        .collect::<String>(),
    };
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        payload,
    )
}

struct Prompts {
    child: tokio::process::Child,
    socket: std::path::PathBuf,
    endpoint: Arc<Endpoint>,
    base: String,
    _server: tokio::task::JoinHandle<()>,
}

async fn prompts(directory: &std::path::Path) -> Prompts {
    use std::process::Stdio;
    let endpoint = Arc::new(Endpoint::default());
    let router = Router::new()
        .route("/v1/messages", post(provider))
        .route("/v1/chat/completions", post(provider))
        .route("/v1/responses", post(provider))
        .with_state(endpoint.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let socket = directory.join("ipc/server.sock");
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory.join("ipc"))
            .unwrap();
    }
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
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut output, &mut line)
        .await
        .unwrap();
    assert!(!line.is_empty(), "server startup failed");
    Prompts {
        child,
        socket,
        endpoint,
        base,
        _server: server,
    }
}

/// The conversation one body carries: the protocol's system, tools, messages
/// and the configuration members the cache depends on.
struct Wire {
    system: Value,
    tools: Value,
    messages: Vec<Value>,
    config: Value,
}

/// `value` without the automatic cache-breakpoint marker, which aimux places
/// on the request's last user or tool message and so moves when the
/// suggestion appends its instruction.
fn has_breakpoint(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.contains_key("cache_control") || map.values().any(has_breakpoint),
        Value::Array(items) => items.iter().any(has_breakpoint),
        _ => false,
    }
}

fn without_breakpoints(mut value: Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("cache_control");
                for value in map.values_mut() {
                    strip(value);
                }
            }
            Value::Array(items) => {
                for item in items {
                    strip(item);
                }
            }
            _ => {}
        }
    }
    strip(&mut value);
    value
}

fn wire(kind: &str, body: &Value) -> Wire {
    match kind {
        "anthropic" => Wire {
            system: body["system"].clone(),
            tools: body["tools"].clone(),
            messages: body["messages"].as_array().cloned().unwrap_or_default(),
            config: json!({
                "maxTokens": body["max_tokens"],
                "temperature": body["temperature"],
                "thinking": body["thinking"],
                "toolChoice": body["tool_choice"],
                "serviceTier": body["service_tier"],
            }),
        },
        "openai-compatible" => Wire {
            system: Value::Null,
            tools: body["tools"].clone(),
            messages: body["messages"].as_array().cloned().unwrap_or_default(),
            config: json!({
                "maxTokens": body["max_tokens"],
                "temperature": body["temperature"],
                "reasoningEffort": body["reasoning_effort"],
                "toolChoice": body["tool_choice"],
                "serviceTier": body["service_tier"],
            }),
        },
        _ => Wire {
            system: body["instructions"].clone(),
            tools: body["tools"].clone(),
            messages: body["input"].as_array().cloned().unwrap_or_default(),
            config: json!({
                "maxOutputTokens": body["max_output_tokens"],
                "temperature": body["temperature"],
                "reasoning": body["reasoning"],
                "toolChoice": body["tool_choice"],
                "serviceTier": body["service_tier"],
            }),
        },
    }
}

/// The suggestion body carries the turn's own system, tools and earlier
/// messages, and the same configuration, for every protocol.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wire_prefix_is_the_turns_own_for_every_protocol() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory =
        std::path::PathBuf::from("/tmp").join(format!("ps-wire-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory.join("home"))
        .unwrap();
    std::fs::set_permissions(
        directory.join("home"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::create_dir_all(directory.join("work")).unwrap();
    let mut server = prompts(&directory).await;
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
    let caps = dim_agent_types::CatalogCapabilities::with_limits(32_000, 4_096);
    for (selection, kind) in ["anthropic", "openai-compatible", "openai-responses"]
        .into_iter()
        .enumerate()
    {
        let provider = uuid::Uuid::new_v4().to_string();
        assert_ok(
            &client
                .rpc(
                    "provider.create",
                    json!({"providerId":provider,"displayName":"Fixture",
                        "connection":{"driverKind":kind,"baseUrl":server.base,
                            "auth":{"type":"anonymous"}},
                        "models":[{"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps}],
                        "enabled":true}),
                )
                .await,
        );
        let mut select = json!({"providerId":provider,"modelId":"alpha"});
        if selection > 0 {
            select["expectedVersion"] = json!(selection);
        }
        assert_ok(&client.rpc("provider.select", select).await);
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":directory.join("work"),
                    "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
            )
            .await;
        assert!(
            created["error"].is_null(),
            "{kind} session.create: {created}"
        );
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
        let submitted = client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"add a test"}),
            )
            .await;
        assert!(submitted["error"].is_null(), "{kind} submit: {submitted}");
        let idle = client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await;
        assert!(idle["error"].is_null(), "{kind} idle: {idle}");
        let turn = latest_turn(&mut client, &id).await;
        configure(&mut client, json!({"enabled":true,"maxChars":80})).await;
        assert_eq!(
            suggest(&mut client, &id, &turn).await,
            json!({"accepted": true}),
            "{kind}"
        );
        let offer = suggestion_notice(&mut client, "offer").await;
        assert_eq!(offer["params"]["data"]["text"], "run the tests", "{kind}");

        let submitted = client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"and the next one"}),
            )
            .await;
        assert!(submitted["error"].is_null(), "{kind} submit: {submitted}");
        let idle = client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await;
        assert!(idle["error"].is_null(), "{kind} idle: {idle}");

        let bodies: Vec<Value> = server
            .endpoint
            .bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|(sent, _)| sent == kind)
            .map(|(_, body)| body.clone())
            .collect();
        assert_eq!(
            bodies.len(),
            3,
            "{kind}: the turn's call, the suggestion and the next turn"
        );
        let turn_wire = wire(kind, &bodies[0]);
        let suggestion = wire(kind, &bodies[1]);
        let next_wire = wire(kind, &bodies[2]);
        assert_eq!(
            serialized(&suggestion.system),
            serialized(&turn_wire.system),
            "{kind}: system"
        );
        assert_eq!(
            serialized(&suggestion.tools),
            serialized(&turn_wire.tools),
            "{kind}: tools"
        );
        assert_eq!(
            serialized(&suggestion.config),
            serialized(&turn_wire.config),
            "{kind}: configuration"
        );
        assert_eq!(
            suggestion.messages.len(),
            turn_wire.messages.len() + 2,
            "{kind}: exactly two messages follow"
        );
        let anthropic = kind == "anthropic";
        for (index, message) in turn_wire.messages.iter().enumerate() {
            let (theirs, ours) = if anthropic {
                (
                    without_breakpoints(message.clone()),
                    without_breakpoints(suggestion.messages[index].clone()),
                )
            } else {
                (message.clone(), suggestion.messages[index].clone())
            };
            assert_eq!(
                serialized(ours),
                serialized(theirs),
                "{kind}: message {index} differs"
            );
        }
        let prefix = turn_wire.messages.len();
        let tail = &suggestion.messages[prefix..];
        assert!(
            serialized(&tail[0]).contains("done"),
            "{kind}: the assistant message follows: {}",
            serialized(&tail[0])
        );
        assert!(
            serialized(&tail[1]).contains("You predict the next message"),
            "{kind}: the instruction follows: {}",
            serialized(&tail[1])
        );
        let assistant = if anthropic {
            let ours = without_breakpoints(tail[0].clone());
            let theirs = without_breakpoints(next_wire.messages[prefix].clone());
            (ours, theirs)
        } else {
            (tail[0].clone(), next_wire.messages[prefix].clone())
        };
        assert_eq!(
            serialized(&assistant.0),
            serialized(&assistant.1),
            "{kind}: the assistant message is the one history builds"
        );
        if anthropic {
            assert!(
                turn_wire.messages.last().is_some_and(has_breakpoint),
                "the turn's last message carries the tail breakpoint"
            );
            assert!(
                has_breakpoint(&tail[1]),
                "the instruction carries the tail breakpoint"
            );
            assert!(
                suggestion.messages[..prefix + 1]
                    .iter()
                    .all(|message| !has_breakpoint(message)),
                "no other message carries a breakpoint"
            );
            assert!(
                next_wire.messages[..prefix]
                    .iter()
                    .all(|message| !has_breakpoint(message))
            );
        }
    }
    let _ = server.child.kill().await;
    server._server.abort();
    let _ = std::fs::remove_dir_all(&directory);
}

/// Unloading the Session part ends its tasks before it settles: the held
/// model call is cancelled and the unload waits for the request task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_the_part_waits_for_its_request_task() {
    let fixture = serve_fixture(Reply::Text("run the tests".into())).await;
    let (mut client, id) = create(&fixture, true).await;
    submit(&mut client, &id, "add a test").await;
    let turn = latest_turn(&mut client, &id).await;
    configure(&mut client, json!({"enabled":true})).await;

    let completion = prompt_suggestion::testing::hold_next_completion(&id);
    let _gate = fixture.model.holds_next();
    assert_eq!(
        suggest(&mut client, &id, &turn).await,
        json!({"accepted": true})
    );
    fixture.model.wait_suggestions(1).await;
    // Nothing holds the Session, so the idle-unload pass may release it
    // while the request is still running.
    assert_ok(&client.rpc("session.detach", json!({"sessionId":id})).await);

    let agent = fixture.server.agent().unwrap();
    let sid = id.clone();
    let mut unloading = tokio::spawn(async move {
        // The idle-unload pass: release the instance as soon as it is idle.
        loop {
            match agent.get_session(&sid) {
                None => break,
                Some(session) => {
                    let _ = session.unload_if_idle().await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    // The endpoint's held call is cancelled: its connection closes.
    fixture.model.wait_cancellations(1).await;
    // The request task returns from the cancelled call and waits at the
    // barrier, so it is still running while the unload proceeds.
    completion.entered().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut unloading)
            .await
            .is_err(),
        "the unload settled while its request task was still running"
    );
    completion.release();
    completion.passed().await;
    tokio::time::timeout(Duration::from_secs(10), unloading)
        .await
        .expect("the unload settles once the request task ends")
        .expect("the unload task ran");
    let record = prompt_suggestion::testing::unload_record(&id)
        .expect("the part records what its tracker held when it unloaded");
    assert!(
        record.live >= 1,
        "the request task was still running when the unload closed the tracker: {record:?}"
    );
    assert_eq!(
        record.remaining, 0,
        "no task of the old part remains after its unload: {record:?}"
    );
    fixture.end().await;
}
