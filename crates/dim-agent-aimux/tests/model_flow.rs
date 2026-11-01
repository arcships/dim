use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
    routing::{any, post},
};
use dim_agent_aimux::{AimuxModels, AimuxPlugin, ConfiguredModelsPlugin, config::ModelConfigFile};
use dim_agent_capabilities::{adapters::MockTools, defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use futures_util::StreamExt;
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Endpoint {
    requests: Arc<Mutex<Vec<Value>>>,
    authorizations: Arc<Mutex<Vec<Option<String>>>>,
}
struct TestServer {
    endpoint: Endpoint,
    url: String,
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}
impl TestServer {
    async fn start() -> Self {
        let endpoint = Endpoint::default();
        let router = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(endpoint.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let join = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .unwrap();
        });
        Self {
            endpoint,
            url,
            cancel,
            join,
        }
    }
    fn models(&self) -> Arc<AimuxModels> {
        Arc::new(AimuxModels::from_config(configuration(&self.url)).unwrap())
    }
    async fn close(self) {
        self.cancel.cancel();
        self.join.await.unwrap();
    }
}
fn configuration(url: &str) -> ModelConfigFile {
    serde_json::from_value(json!({"defaultModel":{"providerId":"local","modelId":"test-model"},"providers":[{"id":"local","kind":"openai-compatible","baseUrl":url,"models":[{"id":"test-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":4096,"temperature":0}}]}]})).unwrap()
}
fn frame(delta: Value, finish: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id":"remote-id","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}
async fn respond(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    endpoint.requests.lock().unwrap().push(request.clone());
    endpoint.authorizations.lock().unwrap().push(
        headers
            .get("authorization")
            .map(|value| value.to_str().unwrap().to_owned()),
    );
    let messages = request["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    let prompt = last["content"].as_str().unwrap_or("");
    if prompt.starts_with("reject401") {
        return Response::builder()
            .status(401)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"error":{"message":"Expired credential","code":"token_expired"}}"#,
            ))
            .unwrap();
    }
    if prompt == "slowheaders" {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let (data, hold) = if last["role"] == "tool" {
        (
            frame(json!({"content":"tool completed"}), json!("stop")) + "data: [DONE]\n\n",
            false,
        )
    } else if prompt == "tool" || prompt == "unauthorized" || prompt == "length" {
        let name = if prompt == "unauthorized" {
            "forbidden"
        } else {
            "echo"
        };
        let mut data = frame(
            json!({"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":name,"arguments":"{\"text\":"}}]}),
            Value::Null,
        );
        data += &frame(
            json!({"tool_calls":[{"index":0,"function":{"arguments":"\"result text\"}"}}]}),
            Value::Null,
        );
        data += &frame(
            json!({}),
            json!(if prompt == "length" {
                "length"
            } else {
                "tool_calls"
            }),
        );
        data += "data: [DONE]\n\n";
        (data, false)
    } else if prompt == "wait" {
        (frame(json!({"content":"started"}), Value::Null), true)
    } else if prompt == "truncated" {
        (frame(json!({"content":"partial"}), Value::Null), false)
    } else {
        let mut data = frame(json!({"reasoning_content":"thinking"}), Value::Null);
        data += &frame(json!({"content":"hello"}), json!("stop"));
        data += &format!(
            "data: {}\n\n",
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":2}}})
        );
        data += "data: [DONE]\n\n";
        (data, true)
    };
    let body = Body::from_stream(async_stream::stream! {
        yield Ok::<_, Infallible>(data);
        if hold { std::future::pending::<()>().await; }
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}
fn builder(models: Arc<AimuxModels>, tools: Arc<dyn ToolDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig {
        model: models.default_model().clone(),
        ..Default::default()
    });
    defaults::install_with_process_model(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_value(Arc::new(ToolHost(tools)));
    dim_agent_aimux::install(&mut builder);
    builder
}
/// The host's model plugins are process rows of its loader.
async fn model_agent(models: Arc<AimuxModels>, custom: Option<AgentBuilder>) -> MinimalHost {
    let mut builder = custom.unwrap_or_else(|| builder(models.clone(), Arc::new(MockTools)));
    builder
        .plugin(|_| Ok(Box::new(AimuxPlugin)))
        .plugin(move |_| Ok(Box::new(ConfiguredModelsPlugin(models.clone()))));
    MinimalHost::start(&builder).await.unwrap()
}
fn session_config() -> SessionConfig {
    SessionConfig {
        model: ModelSelection {
            provider_id: "local".into(),
            model_id: "test-model".into(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        },
        system_prompt: "system instructions".into(),
        tools: Some(vec!["echo".into()]),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_requests_use_connection_snapshots_and_release_retained_streams_on_close() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let first = TestServer::start().await;
        let second = TestServer::start().await;
        let root = rutis::Ctx::root().unwrap();
        root.plugin(AimuxPlugin).await.unwrap();
        let host = root.get::<ModelHost>().unwrap();
        let request = |text: &str| Arc::new(ModelRequest::new("logical-provider", "test-model", vec![Message::text(Role::User, text)]));
        let connection = |server: &TestServer, key: &str| ModelConnection::Http(HttpModelConnection {
 api_key: None,
            network: Default::default(),
            protocol: ModelProtocol::OpenaiCompatible, base_url: server.url.clone(),
            bearer: Some(key.to_owned()), headers: Default::default(), preset: None,
        });
        for (server, key) in [(&first, "first-secret"), (&second, "second-secret")] {
            let mut stream = host.0.invoke(request("text"), connection(server, key), CancellationToken::new()).await.unwrap();
            while let Some(chunk) = stream.next().await { chunk.unwrap(); }
            assert_eq!(*server.endpoint.authorizations.lock().unwrap(), [Some(format!("Bearer {key}"))]);
            assert!(!server.endpoint.requests.lock().unwrap()[0].to_string().contains(key));
        }
        let failure = host.0.invoke(request("reject401"), connection(&second, "expired-secret"), CancellationToken::new()).await.err().unwrap();
        assert!(matches!(&failure, HostError::Failed { kind: HostErrorKind::Http { status: Some(401), code: Some(code) }, .. } if code == "token_expired"));
        assert!(!failure.to_string().contains("expired-secret"));
        let unused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = unused.local_addr().unwrap();
        drop(unused);
        let failure = host.0.invoke(request("text"), ModelConnection::Http(HttpModelConnection {
 api_key: None,
            network: Default::default(),
            protocol: ModelProtocol::OpenaiCompatible,
            base_url: format!("http://user:url-secret@{address}/v1?api-key=query-secret"),
            headers: Default::default(), bearer: None, preset: None,
        }), CancellationToken::new()).await.err().unwrap();
        assert!(!failure.to_string().contains("query-secret"));
        assert!(!failure.to_string().contains("url-secret"));
        let failure = host.0.invoke(request("text"), ModelConnection::Http(HttpModelConnection {
 api_key: None,
            network: Default::default(),
            protocol: ModelProtocol::OpenaiCompatible, base_url: first.url.clone(),
            headers: Default::default(), bearer: Some("header-secret\n".into()), preset: None,
        }), CancellationToken::new()).await.err().unwrap();
        assert!(!failure.to_string().contains("header-secret"));
        let mut unpolled = host.0.invoke(request("wait"), connection(&first, "unpolled"), CancellationToken::new()).await.unwrap();
        let mut partial = host.0.invoke(request("wait"), connection(&second, "partial"), CancellationToken::new()).await.unwrap();
        assert!(matches!(partial.next().await.unwrap().unwrap(), ModelChunk::MessageStart { .. }));
        assert!(matches!(partial.next().await.unwrap().unwrap(), ModelChunk::Text(_)));
        root.shutdown().await.unwrap();
        for stream in [&mut unpolled, &mut partial] {
            assert!(matches!(stream.next().await, Some(Err(HostError::Failed { kind: HostErrorKind::OwnerClosed, .. }))));
            assert!(stream.next().await.is_none());
        }
        assert!(matches!(host.0.invoke(request("text"), connection(&first, "old"), CancellationToken::new()).await,
            Err(HostError::Failed { kind: HostErrorKind::OwnerClosed, .. })));
        let next = rutis::Ctx::root().unwrap();
        next.plugin(AimuxPlugin).await.unwrap();
        let mut stream = next.get::<ModelHost>().unwrap().0.invoke(request("text"), connection(&first, "new"), CancellationToken::new()).await.unwrap();
        while let Some(chunk) = stream.next().await { chunk.unwrap(); }
        next.shutdown().await.unwrap();
        first.close().await; second.close().await;
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_http_text_usage_tools_and_continuation_follow_agent_events() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = TestServer::start().await;
        let agent = model_agent(server.models(), None).await;
        let session = agent.create_session(session_config()).await.unwrap();
        session.submit("text".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::Thinking(data) if data.delta == "thinking")));
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::Text(data) if data.delta == "hello")));
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::ModelUsage(data) if data.usage.prompt_tokens == 10. && data.usage.total_tokens == 13. && data.usage.cache_read_tokens == Some(2.))));
        let call = events.iter().find_map(|e| if let EventKind::ModelCall(call) = &e.kind { Some(call) } else { None }).unwrap();
        let metadata = call.request.metadata.as_ref().unwrap();
        assert_eq!(metadata.session_id.as_deref(), Some(session.id()));
        assert_eq!(metadata.turn_id.as_deref(), Some(call.turn_id.as_str()));
        assert_eq!(metadata.extensions["requestId"], call.request_id);
        assert!(metadata.run_id.is_none());
        session.submit("tool".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::ToolResult(data) if data.results[0].result.content == ToolContent::Text("result text".into()))));
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::Text(data) if data.delta == "tool completed")));
        assert!(matches!(events.last().unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })));
        let requests = server.endpoint.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        for request in &requests {
            assert!(!request.to_string().contains("cache_control"), "{request}");
        }
        assert_eq!(requests[0]["model"], "test-model");
        assert_eq!(requests[0]["temperature"].as_f64(), Some(0.));
        let system = requests[0]["messages"][0]["content"]
            .as_str()
            .expect("the request's system text");
        assert!(
            system.starts_with("system instructions\n\n# How to read your input"),
            "{system:?}"
        );
        assert!(
            system.ends_with("they do not renew any permission."),
            "{system:?}"
        );
        assert_eq!(requests[2]["messages"].as_array().unwrap().last().unwrap()["content"], "result text");

        let mut explicit = session_config();
        explicit.model.config = Some(ModelConfig { temperature: Some(0.375), ..Default::default() });
        explicit.model.metadata = Some(serde_json::from_value(json!({
            "sessionId": "stale-session", "turnId": "stale-turn", "requestId": "stale-request",
            "runId": "caller-run", "traceId": "caller-trace", "experiment": "selected"
        })).unwrap());
        let selected = agent.create_session(explicit).await.unwrap();
        selected.submit("text".into(), InputMode::Queue, "main").await.unwrap();
        selected.idle("main").await.unwrap();
        let events = selected.history("main", None).await.unwrap();
        let call = events.iter().find_map(|e| if let EventKind::ModelCall(call) = &e.kind { Some(call) } else { None }).unwrap();
        assert_eq!(call.request.config.as_ref().unwrap().temperature, Some(0.375));
        assert_eq!(call.request.config.as_ref().unwrap().max_tokens, None, "An explicit model configuration must not inherit catalog fields");
        let metadata = call.request.metadata.as_ref().unwrap();
        assert_eq!(metadata.session_id.as_deref(), Some(selected.id()));
        assert_eq!(metadata.turn_id.as_deref(), Some(call.turn_id.as_str()));
        assert_eq!(metadata.extensions["requestId"], call.request_id);
        assert_eq!(metadata.run_id.as_deref(), Some("caller-run"));
        assert_eq!(metadata.trace_id.as_deref(), Some("caller-trace"));
        assert_eq!(metadata.extensions["experiment"], "selected");
        let requests = server.endpoint.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[3]["temperature"].as_f64(), Some(0.375));
        assert!(requests[3].get("max_tokens").is_none());
        agent.close().await.unwrap(); server.close().await;
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_releases_native_request_and_keeps_other_branches_usable() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = TestServer::start().await; let agent = model_agent(server.models(), None).await;
        let session = agent.create_session(session_config()).await.unwrap();
        session.submit("wait".into(), InputMode::Queue, "main").await.unwrap();
        // Streaming text is progress until its block ends (agent-029).
        while !session.get::<dim_agent_runtime::ModelProgress>("main").unwrap().is_some_and(|progress| !progress.text.is_empty()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let branch = session.branch("main".into(), 0, BranchOptions { prompt: "text".into(), source: BranchSource::Host { operation_id: "branch".into() } }).await.unwrap();
        session.stop("main", "user stop".into()).await.unwrap();
        session.idle(&branch).await.unwrap();
        assert!(session.history("main", None).await.unwrap().iter().any(|e| matches!(&e.kind, EventKind::ModelOutputEnd(data) if matches!(data.outcome, ModelOutputOutcome::Cancelled { .. }))));
        assert!(session.history(&branch, None).await.unwrap().iter().any(|e| matches!(&e.kind, EventKind::Text(data) if data.delta == "hello")));
        session.submit("text".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        agent.close().await.unwrap(); server.close().await;
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_native_outputs_do_not_execute_tools_or_invent_usage() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = TestServer::start().await; let agent = model_agent(server.models(), None).await;
        for prompt in ["unauthorized", "truncated", "length"] {
            let session = agent.create_session(session_config()).await.unwrap();
            session.submit(prompt.into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = session.history("main", None).await.unwrap();
            assert!(events.iter().any(|e| matches!(&e.kind, EventKind::ModelOutputEnd(data) if matches!(data.outcome, ModelOutputOutcome::Failed { .. }))), "{prompt}: {events:?}");
            // Calls a failed output started are closed, never dispatched (agent-029).
            assert!(!events.iter().any(|e| matches!(&e.kind, EventKind::ToolCall(_) | EventKind::ModelUsage(_)) || matches!(&e.kind, EventKind::ToolResult(data) if data.call_event_id.is_some())), "{prompt}: {events:?}");
            session.close().await.unwrap();
        }
        agent.close().await.unwrap(); server.close().await;
    }).await.unwrap();
}

#[tokio::test]
async fn model_configuration_rejects_unknown_models_before_session_start() {
    let server = TestServer::start().await;
    let agent = model_agent(server.models(), None).await;
    let mut config = session_config();
    config.model.model_id = "not-registered".into();
    assert!(agent.create_session(config).await.is_err());
    assert!(agent.sessions().is_empty());
    assert!(server.endpoint.requests.lock().unwrap().is_empty());
    agent.close().await.unwrap();
    server.close().await;
}

#[test]
fn a_model_entry_without_window_facts_fails_loading_and_names_the_model() {
    let configuration = |context_window: Option<u64>, max_output_tokens: Option<u64>| {
        serde_json::from_value::<ModelConfigFile>(json!({
            "defaultModel": {"providerId": "local", "modelId": "test-model"},
            "providers": [{"id": "local", "kind": "openai-compatible",
                "baseUrl": "http://127.0.0.1:9/v1",
                "models": [{"id": "test-model", "contextWindow": context_window,
                    "maxOutputTokens": max_output_tokens, "config": {"maxTokens": 4096}}]}]
        }))
        .unwrap()
    };
    for (context_window, max_output_tokens, field) in [
        (None, Some(4096), "contextWindow"),
        (Some(0), Some(4096), "contextWindow"),
        (Some(128_000), None, "maxOutputTokens"),
        (Some(128_000), Some(0), "maxOutputTokens"),
    ] {
        let error = AimuxModels::from_config(configuration(context_window, max_output_tokens))
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains(&format!("local.test-model.{field}")),
            "{error}"
        );
    }
}

struct OverrideRequest {
    remove_tools: bool,
    timeline: rutis::host::Key<Timeline>,
}
impl rutis::host::DynPlugin for OverrideRequest {
    fn name(&self) -> &str {
        "test.final-model-request"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let remove_tools = self.remove_tools;
            timeline.before(ctx, self.name(), &["model/call"], move |mut event| {
                let EventKind::ModelCall(call) = &mut event.kind else {
                    unreachable!()
                };
                let request = Arc::make_mut(&mut call.request);
                request.provider_id = "selected".into();
                request.model_id = "selected-model".into();
                request.system = Some(Arc::new(SystemPrompt::Text("final system".into())));
                request.config = Some(ModelConfig {
                    max_tokens: Some(17.),
                    temperature: Some(0.),
                    ..Default::default()
                });
                if remove_tools {
                    request.tools = Some(Arc::new(Vec::new()));
                }
                Ok(event)
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_committed_request_controls_model_settings_and_tool_admission() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let original = TestServer::start().await; let selected = TestServer::start().await;
        let mut config = configuration(&original.url);
        config.providers.push(dim_agent_aimux::config::ProviderConfig {
            id: "selected".into(), kind: dim_agent_aimux::config::ProviderKind::OpenaiCompatible,
            api_key_env: None, base_url: selected.url.clone(), headers: Default::default(), compatible: None,
            models: vec![dim_agent_aimux::config::ModelEntry { id: "selected-model".into(), context_window: Some(128_000), max_output_tokens: Some(4096), config: ModelConfig { max_tokens: Some(99.), temperature: Some(0.8), ..Default::default() } }],
        });
        let models = Arc::new(AimuxModels::from_config(config).unwrap());
        for remove_tools in [false, true] {
            let mut builder = builder(models.clone(), Arc::new(MockTools));
            builder.require_feature(Feature::new("test.final-model-request").branch("test.final-model-request", move |names| Ok(Box::new(OverrideRequest { remove_tools, timeline: names.key()? }))));
            let agent = model_agent(models.clone(), Some(builder)).await; let session = agent.create_session(session_config()).await.unwrap();
            session.submit("tool".into(), InputMode::Queue, "main").await.unwrap();
            session.idle("main").await.unwrap();
            let events = session.history("main", None).await.unwrap();
            if remove_tools {
                assert!(!events.iter().any(|event| matches!(event.kind, EventKind::ToolCall(_) | EventKind::ToolResult(_))));
                assert!(events.iter().any(|event| matches!(&event.kind, EventKind::ModelOutputEnd(end) if matches!(end.outcome, ModelOutputOutcome::Failed { .. }))));
            } else {
                assert!(events.iter().any(|event| matches!(&event.kind, EventKind::Text(text) if text.delta == "tool completed")));
            }
            let requests = std::mem::take(&mut *selected.endpoint.requests.lock().unwrap());
            assert_eq!(requests.len(), if remove_tools { 1 } else { 2 });
            for request in requests {
                assert_eq!(request["model"], "selected-model");
                assert_eq!(request["messages"][0]["content"], "final system");
                assert_eq!(request["max_tokens"], 17);
                assert_eq!(request["temperature"].as_f64(), Some(0.));
                assert_eq!(request.get("tools").is_none(), remove_tools);
            }
            agent.close().await.unwrap();
        }
        assert!(original.endpoint.requests.lock().unwrap().is_empty());
        original.close().await; selected.close().await;
    }).await.unwrap();
}

static RECORDING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds the process recorder for one test, resets it and removes the file
/// afterwards.
struct Recording {
    directory: std::path::PathBuf,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl Recording {
    async fn start() -> Self {
        let lock = RECORDING.lock().await;
        let directory = std::env::temp_dir().join(format!(
            "dim-recording-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dim_agent_aimux::recording::set_request_recording(Some(&directory)).unwrap();
        Self {
            directory,
            _lock: lock,
        }
    }

    fn restart(&self) {
        dim_agent_aimux::recording::set_request_recording(None).unwrap();
        dim_agent_aimux::recording::set_request_recording(Some(&self.directory)).unwrap();
    }

    fn lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.directory.join("recordings.jsonl"))
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn wait(&self, count: usize, matches: impl Fn(&Value) -> bool) -> Vec<Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let lines: Vec<Value> = self
                .lines()
                .into_iter()
                .filter(|line| matches(line))
                .collect();
            if lines.len() >= count {
                return lines;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the recording was not written: {lines:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    fn assert_private(&self) {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::fs::metadata(&self.directory).unwrap();
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        let file = std::fs::metadata(self.directory.join("recordings.jsonl")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = dim_agent_aimux::recording::set_request_recording(None);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn overlapping_recorders_append_complete_jsonl_lines() {
    use aimux_core::recording::{JsonlRecorder, OutcomeRecord, OutcomeStatus, Recorder};

    let directory =
        std::env::temp_dir().join(format!("dim-recording-overlap-{}", uuid::Uuid::new_v4()));
    let recorders: Vec<_> = (0..2)
        .map(|_| JsonlRecorder::try_new(&directory).unwrap())
        .collect();
    let options = aimux_core::options::CallOptions::new(
        aimux_core::language_model_message::convert_to_language_model_prompt(
            &[aimux_core::message::ModelMessage::user("x".repeat(16_384))],
            None,
        ),
    );
    for index in 0..128 {
        for (writer, recorder) in recorders.iter().enumerate() {
            let id = format!("writer-{writer}-{index}");
            recorder.record_input(&id, &options, "openai", "test-model");
            recorder.record_outcome(
                &id,
                &OutcomeRecord {
                    status: OutcomeStatus::Success,
                    ..OutcomeRecord::default()
                },
            );
            recorder.record_transport_closed(&id);
        }
    }
    for recorder in &recorders {
        recorder.try_flush().unwrap();
    }
    drop(recorders);
    let content = std::fs::read_to_string(directory.join("recordings.jsonl")).unwrap();
    assert!(content.ends_with('\n'));
    let records: Vec<Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 256);
    let ids: std::collections::HashSet<_> = records
        .iter()
        .map(|record| record["call_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 256);
    for record in records {
        assert_eq!(record["outcome"]["status"], "success");
        assert_eq!(record["complete"], true);
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_requests_write_their_wire_exchange() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let recording = Recording::start().await;
        let server = TestServer::start().await;
        let agent = model_agent(server.models(), None).await;
        let session = agent.create_session(session_config()).await.unwrap();
        session
            .submit("text".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();

        let url = format!("{}/chat/completions", server.url);
        let lines = recording
            .wait(1, |line| {
                line["exchanges"][0]["request"]["url"].as_str() == Some(url.as_str())
            })
            .await;
        let recorded = &lines[0];
        let call_id = recorded["call_id"].as_str().unwrap();
        assert_eq!(
            recording
                .lines()
                .iter()
                .filter(|line| line["call_id"] == call_id)
                .count(),
            1,
            "one line per call, including the transport close"
        );
        let exchange = &recorded["exchanges"][0];
        assert_eq!(exchange["response"]["status"].as_u64(), Some(200));
        assert!(
            exchange["request"]["body"]
                .as_str()
                .unwrap()
                .contains("\"messages\"")
        );
        assert!(exchange["timing"]["latency_ms"].is_number());
        assert_eq!(recorded["provider"]["provider"], "openai");
        assert_eq!(recorded["provider"]["model_id"], "test-model");
        assert_eq!(recorded["outcome"]["status"], "success");
        assert!(recorded["complete"].as_bool().unwrap());
        assert!(recorded["input"]["prompt"].is_array());

        recording.restart();
        session
            .submit("again".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let recorded = recording
            .wait(2, |line| {
                line["exchanges"][0]["request"]["url"].as_str() == Some(url.as_str())
            })
            .await;
        assert_eq!(recorded.len(), 2, "the enable/disable cycle records again");
        let calls: std::collections::HashSet<_> = recorded
            .iter()
            .map(|line| line["call_id"].as_str())
            .collect();
        assert_eq!(calls.len(), 2);
        for call in &recorded {
            assert_eq!(call["outcome"]["status"], "success");
            assert!(call["complete"].as_bool().unwrap());
        }
        #[cfg(unix)]
        recording.assert_private();

        agent.close().await.unwrap();
        server.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorded_failures_and_cancellations_close_their_record() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let recording = Recording::start().await;
        let server = TestServer::start().await;
        let url = format!("{}/chat/completions", server.url);
        let root = rutis::Ctx::root().unwrap();
        root.plugin(AimuxPlugin).await.unwrap();
        let host = root.get::<ModelHost>().unwrap();
        let request = |text: &str| {
            Arc::new(ModelRequest::new(
                "logical-provider",
                "test-model",
                vec![Message::text(Role::User, text)],
            ))
        };
        let connection = |key: &str| {
            ModelConnection::Http(HttpModelConnection {
                api_key: None,
                network: Default::default(),
                protocol: ModelProtocol::OpenaiCompatible,
                base_url: server.url.clone(),
                bearer: Some(key.to_owned()),
                headers: Default::default(),
                preset: None,
            })
        };

        let failure = host
            .0
            .invoke(
                request("reject401 recorded"),
                connection("expired-secret"),
                CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(
            failure,
            HostError::Failed {
                kind: HostErrorKind::Http {
                    status: Some(401),
                    ..
                },
                ..
            }
        ));
        let lines = recording
            .wait(1, |line| {
                line["exchanges"][0]["request"]["url"].as_str() == Some(url.as_str())
                    && line["input"]["prompt"]
                        .to_string()
                        .contains("reject401 recorded")
            })
            .await;
        assert_eq!(lines.len(), 1, "one line for a failed call");
        assert_eq!(lines[0]["outcome"]["status"], "error");
        assert_eq!(lines[0]["exchanges"][0]["response"]["status"], 401);
        assert!(lines[0]["complete"].as_bool().unwrap());

        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stop.cancel();
        });
        let failure = host
            .0
            .invoke(request("slowheaders"), connection("key"), cancel)
            .await
            .err()
            .unwrap();
        assert!(matches!(failure, HostError::Cancelled(_)), "{failure:?}");
        let lines = recording
            .wait(1, |line| {
                line["exchanges"][0]["request"]["url"].as_str() == Some(url.as_str())
                    && line["input"]["prompt"].to_string().contains("slowheaders")
            })
            .await;
        assert_eq!(lines.len(), 1, "one line for a cancelled call");
        assert_eq!(lines[0]["outcome"]["status"], "cancelled");
        assert!(lines[0]["exchanges"][0]["response"].is_null());
        assert!(lines[0]["complete"].as_bool().unwrap());

        server.close().await;
    })
    .await
    .unwrap();
}

fn output_failure(events: &[Arc<AgentEvent>]) -> Option<String> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::ModelOutputEnd(data) => match &data.outcome {
            ModelOutputOutcome::Failed { error, .. } => Some(error.clone()),
            _ => None,
        },
        _ => None,
    })
}

#[derive(Clone)]
struct RedirectState {
    hits: Arc<Mutex<Vec<RedirectHit>>>,
    redirect: Option<(StatusCode, String)>,
}
#[derive(Clone, Debug)]
struct RedirectHit {
    method: Method,
    path: String,
    body: Option<Value>,
}
struct RedirectServer {
    state: RedirectState,
    origin: String,
    url: String,
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}
impl RedirectServer {
    async fn start(status: StatusCode, location: &str) -> Self {
        Self::launch(Some((status, location.to_owned()))).await
    }
    async fn serving() -> Self {
        Self::launch(None).await
    }
    async fn launch(redirect: Option<(StatusCode, String)>) -> Self {
        let state = RedirectState {
            hits: Arc::default(),
            redirect,
        };
        let router = Router::new()
            .route("/v1/chat/completions", any(serve_redirect))
            .route("/v1b/chat/completions", any(serve_redirect))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/v1");
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let join = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .unwrap();
        });
        Self {
            state,
            origin,
            url,
            cancel,
            join,
        }
    }
    fn hits(&self) -> Vec<RedirectHit> {
        self.state.hits.lock().unwrap().clone()
    }
    fn models(&self) -> Arc<AimuxModels> {
        Arc::new(AimuxModels::from_config(configuration(&self.url)).unwrap())
    }
    async fn close(self) {
        self.cancel.cancel();
        self.join.await.unwrap();
    }
}
async fn serve_redirect(
    State(state): State<RedirectState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    state.hits.lock().unwrap().push(RedirectHit {
        method,
        path: uri.path().to_owned(),
        body: serde_json::from_slice(&body).ok(),
    });
    match (&state.redirect, uri.path()) {
        (Some((status, location)), "/v1/chat/completions") => Response::builder()
            .status(*status)
            .header("location", location)
            .body(Body::empty())
            .unwrap(),
        _ => Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(
                frame(json!({"content": "hello"}), json!("stop")) + "data: [DONE]\n\n",
            ))
            .unwrap(),
    }
}
async fn redirect_failure(base_url: &str, bearer: &str) -> HostError {
    let root = rutis::Ctx::root().unwrap();
    root.plugin(AimuxPlugin).await.unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let request = Arc::new(ModelRequest::new(
        "local",
        "test-model",
        vec![Message::text(Role::User, "text")],
    ));
    let connection = ModelConnection::Http(HttpModelConnection {
        api_key: None,
        network: Default::default(),
        protocol: ModelProtocol::OpenaiCompatible,
        base_url: base_url.to_owned(),
        bearer: Some(bearer.to_owned()),
        headers: Default::default(),
        preset: None,
    });
    let error = match host
        .0
        .invoke(request, connection, CancellationToken::new())
        .await
    {
        Err(error) => error,
        Ok(mut stream) => loop {
            match stream.next().await {
                Some(Err(error)) => break error,
                Some(Ok(_)) => {}
                None => panic!("the model stream ended without the expected failure"),
            }
        },
    };
    root.shutdown().await.unwrap();
    error
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_origin_307_or_308_redirect_completes_the_turn_at_its_target() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for status in [
            StatusCode::TEMPORARY_REDIRECT,
            StatusCode::PERMANENT_REDIRECT,
        ] {
            let server = RedirectServer::start(status, "/v1b/chat/completions").await;
            let agent = model_agent(server.models(), None).await;
            let session = agent.create_session(session_config()).await.unwrap();
            session
                .submit("text".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = session.history("main", None).await.unwrap();
            assert_eq!(output_failure(&events), None, "{status}: the turn failed");
            assert!(
                events.iter().any(
                    |event| matches!(&event.kind, EventKind::Text(data) if data.delta == "hello")
                ),
                "{status}"
            );
            assert!(
                matches!(
                    events.last().unwrap().kind,
                    EventKind::TurnEnd(TurnEndData {
                        outcome: TurnOutcome::Completed { .. },
                        ..
                    })
                ),
                "{status}"
            );
            let hits = server.hits();
            assert_eq!(hits.len(), 2, "{status}: {hits:?}");
            assert_eq!(hits[0].path, "/v1/chat/completions");
            assert_eq!(hits[1].path, "/v1b/chat/completions");
            assert_eq!(hits[1].method, Method::POST);
            assert_eq!(hits[1].body.as_ref().unwrap()["model"], "test-model");
            agent.close().await.unwrap();
            server.close().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_origin_301_redirect_is_not_followed_and_names_its_target() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server =
            RedirectServer::start(StatusCode::MOVED_PERMANENTLY, "/v1b/chat/completions").await;
        let moved = format!("{}/v1b/chat/completions", server.origin);
        let error = redirect_failure(&server.url, "301-secret").await;
        let message = error.to_string();
        assert!(message.contains(&moved), "{message}");
        assert!(message.contains("HTTP 301"), "{message}");
        assert!(message.contains("GET without its body"), "{message}");
        assert!(!message.contains("301-secret"), "{message}");
        let hits = server.hits();
        assert_eq!(hits.len(), 1, "the target was contacted: {hits:?}");
        assert_eq!(hits[0].path, "/v1/chat/completions");
        server.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cross_origin_redirect_is_not_followed_and_names_its_target() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let other = RedirectServer::serving().await;
        let moved = format!("{}/chat/completions", other.url);
        let server = RedirectServer::start(StatusCode::TEMPORARY_REDIRECT, &moved).await;
        let error = redirect_failure(&server.url, "cross-origin-secret").await;
        let message = error.to_string();
        assert!(message.contains(&moved), "{message}");
        assert!(message.contains("HTTP 307"), "{message}");
        assert!(message.contains("different origin"), "{message}");
        assert!(!message.contains("cross-origin-secret"), "{message}");
        assert!(other.hits().is_empty(), "the other origin was contacted");
        server.close().await;
        other.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_origin_redirect_chain_stops_at_the_limit() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server =
            RedirectServer::start(StatusCode::TEMPORARY_REDIRECT, "/v1/chat/completions").await;
        let error = redirect_failure(&server.url, "chain-secret").await;
        let message = error.to_string();
        assert!(message.contains("redirect limit"), "{message}");
        assert!(message.contains("HTTP 307"), "{message}");
        assert_eq!(server.hits().len(), 11, "ten hops were followed, no more");
        server.close().await;
    })
    .await
    .unwrap();
}

#[path = "model_flow/native.rs"]
mod native;
