use super::*;
use dim_agent_runtime::{Agent, CollaborationRole, Session};
use dim_agent_types::{
    AgentEvent, CollaborationDelivery, CollaborationOutcome, ContentBlock, EventKind, InputBlock,
    InputMessageData, MessageContent, ToolContent,
};
use rutis::{BoxFuture, CordisError, Ctx, Effect};
use std::ops::RangeInclusive;
use std::sync::OnceLock;

struct CaptureToolRegistry {
    scope: dim_agent_runtime::SessionScope,
    registry: Arc<OnceLock<Arc<dim_agent_capabilities::tool_registry::ToolRegistry>>>,
}

impl TypedPlugin for CaptureToolRegistry {
    type Deps = InSession<dim_agent_capabilities::tool_registry::ToolRegistry>;

    fn name(&self) -> &str {
        "collaboration-test.capture-tool-registry"
    }

    fn apply<'a>(
        &'a self,
        _: &'a Ctx,
        InSession(registry): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let _ = self.registry.set(registry);
            Ok(Effect::Done)
        })
    }
}
impl Capability for CaptureToolRegistry {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

#[derive(Default)]
struct CountingTimelineStore {
    memory: dim_agent_runtime::MemoryTimelineStore,
    largest_read: AtomicUsize,
}

impl dim_agent_runtime::TimelineStore for CountingTimelineStore {
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a dim_agent_runtime::SessionConfig,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.rollback_session(id)
    }
    fn session_config<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_runtime::SessionConfig>>,
    > {
        self.memory.session_config(id)
    }
    fn branches<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Vec<dim_agent_runtime::StoredBranch>>,
    > {
        self.memory.branches(id)
    }
    fn head<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: dim_agent_runtime::TimelineKey,
        events: Vec<dim_agent_runtime::StoredEvent>,
        publish: dim_agent_runtime::PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        self.memory.append(key, events, publish)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Vec<Vec<dim_agent_runtime::StoredEvent>>>,
    > {
        for (_, range) in &segments {
            let width = range.end().saturating_sub(*range.start()).saturating_add(1) as usize;
            self.largest_read.fetch_max(width, Ordering::SeqCst);
        }
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_runtime::TimelineTrace>>,
    > {
        self.memory.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        snapshot: dim_agent_types::RecoverySnapshot,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_types::RecoverySnapshot>>,
    > {
        self.memory.latest_snapshot(key, through, interpreter)
    }
}

async fn launch_team() -> (RunningServer, Agent) {
    launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await
}

struct CaptureMemberConfig {
    scope: dim_agent_runtime::SessionScope,
    configs: Arc<std::sync::Mutex<Vec<Arc<dim_agent_runtime::SessionConfig>>>>,
}
impl TypedPlugin for CaptureMemberConfig {
    type Deps = InSession<dim_agent_runtime::SessionConfig>;
    fn name(&self) -> &str {
        "capture-member-config"
    }
    fn apply<'a>(
        &'a self,
        _: &'a Ctx,
        InSession(config): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            self.configs.lock().unwrap().push(config);
            Ok(Effect::Done)
        })
    }
}
impl Capability for CaptureMemberConfig {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn product_collaboration_adopts_profile_and_default_capabilities() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let home = std::env::temp_dir().join(format!("dim-team-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let configs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = configs.clone();
        let (server, agent) = launch_with(Arc::new(MockTools), ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        }, move |builder| {
            builder.session_plugin(move |scope| CaptureMemberConfig { scope: *scope, configs: observed.clone() });
        }).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let capabilities = dim_agent_types::ModelCapabilities {
            context_window: 32000,
            max_output_tokens: 128,
            ..Default::default()
        };
        let models = ["alpha", "beta"].map(|id| json!({"modelId":id,"displayName":id,"status":"active",
            "capabilities":capabilities}));
        assert_ok(&client.rpc("provider.save", json!({"provider":{
            "providerId":"team-fixture","displayName":"Team fixture","driverKind":"openai-compatible",
            "baseUrl":"http://localhost/v1","auth":{"type":"anonymous"},"models":models,"enabled":true
        }})).await);
        assert_ok(&client.rpc("provider.select", json!({"providerId":"team-fixture","modelId":"alpha"})).await);
        let lead = leader(&mut client, allowed()).await;
        let lead_session = agent.get_session(&lead).unwrap();
        for (step, (model, expected)) in [(Some("beta"), "beta"), (None, "alpha")].into_iter().enumerate() {
            let mut profile = json!({"name":"worker","description":"fixture","systemPrompt":"work","tools":["agent"]});
            if let Some(model) = model {
                profile["model"] = json!({"providerId":"team-fixture","modelId":model});
            }
            say(&mut client, &lead, tool(json!({"action":"request","profile":profile,"prompt":"hello"})), "queue").await;
            returned(&mut client, &lead, &lead_session, step + 1).await;
            let result = last_ok(&events(&lead_session).await);
            assert!(result["sessionId"].is_string());
            let config = configs.lock().unwrap().last().unwrap().clone();
            assert_eq!(config.collaboration_role, CollaborationRole::Member);
            let selected = &config.model;
            assert_eq!(selected.model_id, expected);
            let snapshot = selected.capability_snapshot.as_ref().expect("product member must adopt capabilities");
            assert_eq!(snapshot.target.model_id, expected);
            assert_eq!(snapshot.capabilities.context_window, 32000);
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }).await.unwrap();
}

struct LargeReportModel;
impl dim_agent_capabilities::host::ModelDriver for LargeReportModel {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        use dim_agent_capabilities::host::{ModelChunk, ModelStopReason, ModelStream};
        use dim_agent_types::{MessageContent, Role};
        let last = request.messages.last().unwrap();
        let delegated = matches!(&last.content, MessageContent::Text(text) if text.starts_with("<dim-agent-message "));
        if delegated || last.role == Role::ToolResult {
            Box::pin(async move {
                let text = if delegated {
                    format!("{}<exact>&tail", "界".repeat(400_000))
                } else {
                    "Result received".into()
                };
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(ModelChunk::MessageStart {
                        message_id: "report".into(),
                    }),
                    Ok(ModelChunk::Text(text)),
                    Ok(ModelChunk::MessageStop {
                        message_id: "report".into(),
                        reason: ModelStopReason::EndTurn,
                    }),
                ])) as ModelStream)
            })
        } else {
            dim_agent_capabilities::adapters::EchoModel.invoke(request, connection, cancel)
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_report_readback_survives_member_close_and_is_branch_local() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin, history::HistoryRenderer, host::*,
    };
    use dim_agent_runtime::BranchOptions;
    use dim_agent_types::{BranchSource, InputMode, MessageContent};
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        let store = Arc::new(CountingTimelineStore::default());
        builder.timeline_store(store.clone());
        defaults::install(&mut builder);
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(ModelHost(Arc::new(LargeReportModel))))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .session_plugin(DefaultPrepareModelPlugin::new);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_app_server::collaboration::install(&mut builder);
        let captured_registry = Arc::new(OnceLock::new());
        builder.session_plugin({
            let captured_registry = captured_registry.clone();
            move |scope| CaptureToolRegistry {
                scope: *scope,
                registry: captured_registry.clone(),
            }
        });
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(std::path::PathBuf::from(format!(
                    "/tmp/dim-report-{}/s",
                    uuid::Uuid::new_v4()
                ))),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let agent = server.agent().unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        let lead = leader(&mut client, allowed()).await;
        let session = agent.get_session(&lead).unwrap();
        let agent_id = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
            .as_str()
            .unwrap()
            .to_owned();
        say(
            &mut client,
            &lead,
            tool(json!({"action":"request","agentId":agent_id,"prompt":"Produce a large report"})),
            "queue",
        )
        .await;
        returned(&mut client, &lead, &session, 1).await;
        let timeline = events(&session).await;
        let (result_id, member) = timeline
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::CollaborationResult(data) => {
                    Some((event.id.clone(), data.session_id.clone()))
                }
                _ => None,
            })
            .unwrap();
        let wake = timeline
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::InputMessage(data)
                    if report(data).is_some() =>
                {
                    Some(data)
                }
                _ => None,
            })
            .unwrap();
        assert!(wake.user.is_none());
        // The input carries a bounded preview; the full result stays readable.
        assert!(serde_json::to_vec(wake).unwrap().len() < 17 * 1024);
        let projected = dim_agent_capabilities::history::project_history(&timeline).unwrap();
        let preview = projected
            .iter()
            .find_map(|entry| match &entry.message.content {
                MessageContent::Text(text) if text.starts_with("<dim-agent-report ") => Some(text),
                _ => None,
            })
            .unwrap();
        assert!(preview.matches('界').count() > 5_000);
        assert!(preview.matches('界').count() < 400_000);
        assert!(preview.contains("Preview truncated at 16 KiB"));
        assert!(preview.contains(&format!("resultId \"{result_id}\"")));
        say(
            &mut client,
            &lead,
            tool(json!({"action":"close","agentId":agent_id,"sessionId":member})),
            "queue",
        )
        .await;
        idle(&mut client, &lead).await;
        assert_eq!(last_ok(&events(&session).await)["sessionId"], member);
        let read = tool(json!({"action":"result","resultId":result_id}));
        let registry = captured_registry.get().expect("leader ToolRegistry");
        store.largest_read.store(0, Ordering::SeqCst);
        let direct = registry
            .contributed("agent")
            .expect("agent tool")
            .handler
            .execute(
                ToolCall {
                    id: "direct-result-read".into(),
                    name: "agent".into(),
                    input: serde_json::from_value(
                        json!({"action":"result","resultId":result_id}),
                    )
                    .unwrap(),
                },
                ToolExecutionContext {
                    turn_id: "direct-result-read".into(),
                    request_id: "direct-result-read".into(),
                    branch_id: "main".into(),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            store.largest_read.load(Ordering::SeqCst) <= 1,
            "reading one collaboration result must not scan unrelated branch history"
        );
        let ToolContent::Text(direct) = direct.content else {
            panic!("collaboration result must be text")
        };
        let direct: Value = serde_json::from_str(&direct).unwrap();
        assert_eq!(direct["resultId"], result_id);
        assert_eq!(
            direct["result"]["text"],
            format!("{}<exact>&tail", "界".repeat(400_000))
        );

        // Retain the normal RPC/model flow independently of the direct read probe.
        say(&mut client, &lead, read.clone(), "queue").await;
        idle(&mut client, &lead).await;
        let result = last_ok(&events(&session).await);
        assert_eq!(result["resultId"], result_id);
        assert_eq!(
            result["result"]["text"],
            format!("{}<exact>&tail", "界".repeat(400_000))
        );
        let child = session
            .branch(
                "main".into(),
                session.sequence("main").unwrap(),
                BranchOptions {
                    prompt: read,
                    source: BranchSource::Host {
                        operation_id: "read-other-branch".into(),
                    },
                },
            )
            .await
            .unwrap();
        session.idle(&child).await.unwrap();
        let denied = last_error(&session.local(&child, ..).await.unwrap());
        assert!(!denied.is_empty(), "a result from main must be denied on child");

        session
            .submit(
                tool(json!({"action":"request","agentId":agent_id,"prompt":"Produce a child report"})),
                InputMode::Queue,
                &child,
            )
            .await
            .unwrap();
        poll("child result", async || {
            session
                .local(&child, ..)
                .await
                .unwrap()
                .iter()
                .find_map(|event| match &event.kind {
                    EventKind::CollaborationResult(data) => {
                        Some((event.id.clone(), data.session_id.clone()))
                    }
                    _ => None,
                })
        })
        .await;
        session.idle(&child).await.unwrap();
        let child_events = session.local(&child, ..).await.unwrap();
        let (child_result_id, child_member) = child_events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::CollaborationResult(data) => {
                    Some((event.id.clone(), data.session_id.clone()))
                }
                _ => None,
            })
            .unwrap();
        session
            .submit(
                tool(json!({"action":"close","agentId":agent_id,"sessionId":child_member})),
                InputMode::Queue,
                &child,
            )
            .await
            .unwrap();
        session.idle(&child).await.unwrap();
        session
            .submit(
                tool(json!({"action":"result","resultId":child_result_id})),
                InputMode::Queue,
                &child,
            )
            .await
            .unwrap();
        session.idle(&child).await.unwrap();
        let child_result = last_ok(&session.local(&child, ..).await.unwrap());
        assert_eq!(child_result["resultId"], child_result_id);
        assert_eq!(
            child_result["result"]["text"],
            format!("{}<exact>&tail", "界".repeat(400_000))
        );
        session
            .submit(
                tool(json!({"action":"result","resultId":"missing"})),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(last_error(&events(&session).await).contains("No collaboration result"));
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    })
    .await
    .unwrap();
}

/// A leader Session of the default Agent; members inherit its policy.
async fn leader(client: &mut Client, policy: Value) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": ["echo", "wait", "agent"],
                "permissionPolicy": policy}),
        )
        .await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

fn allowed() -> Value {
    json!({"allowedTools": ["echo", "wait", "agent"]})
}

async fn say(client: &mut Client, session: &str, text: String, mode: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId": session, "branchId": "main", "text": text, "mode": mode}),
            )
            .await,
    );
}

async fn idle(client: &mut Client, session: &str) {
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": session, "branchId": "main"}),
            )
            .await,
    );
}

fn tool(input: Value) -> String {
    format!("tool:agent {input}")
}

async fn events(session: &Session) -> Vec<Arc<AgentEvent>> {
    session.history("main", None).await.unwrap()
}

/// Content of every agent tool result, in Timeline order.
fn results(events: &[Arc<AgentEvent>]) -> Vec<(bool, String)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results.clone()),
            _ => None,
        })
        .flatten()
        .map(|result| match result.result.content {
            ToolContent::Text(text) => (result.result.is_error == Some(true), text),
            other => panic!("unexpected content {other:?}"),
        })
        .collect()
}
fn last_ok(events: &[Arc<AgentEvent>]) -> Value {
    let (error, text) = results(events).pop().unwrap();
    assert!(!error, "{text}");
    serde_json::from_str(&text).unwrap()
}
fn last_error(events: &[Arc<AgentEvent>]) -> String {
    let (error, text) = results(events).pop().unwrap();
    assert!(error, "{text}");
    text
}

fn agent_results(events: &[Arc<AgentEvent>]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => report(data),
            _ => None,
        })
        .filter_map(|result_id| {
            events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::ModelCall(data) => Some(&data.request.messages),
                    _ => None,
                })
                .flatten()
                .find_map(|message| match &message.content {
                    MessageContent::Text(text)
                        if text.starts_with("<dim-agent-report ") && text.contains(result_id) =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                })
        })
        .collect()
}

/// Each operation event has exactly one later `delivered`; each delivered
/// request exactly one later `result`. Collaboration facts stay out of the
/// model context.
fn assert_pairing(events: &[Arc<AgentEvent>]) {
    for (index, event) in events.iter().enumerate() {
        let request = match &event.kind {
            EventKind::CollaborationRequest(_) => true,
            EventKind::CollaborationCancel(_)
            | EventKind::CollaborationClose(_)
            | EventKind::CollaborationMessage(_) => false,
            _ => continue,
        };
        let delivered: Vec<_> = events
            .iter()
            .filter_map(|other| match &other.kind {
                EventKind::CollaborationDelivered(data) if data.op_event_id == event.id => {
                    Some((other.sequence, data.delivery.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(delivered.len(), 1, "delivered of {event:?}");
        assert!(delivered[0].0 > event.sequence);
        let expected =
            usize::from(request && matches!(delivered[0].1, CollaborationDelivery::Ok { .. }));
        let results: Vec<_> = events[index..]
            .iter()
            .filter(|other| {
                matches!(&other.kind,
                EventKind::CollaborationResult(data) if data.request_event_id == event.id)
            })
            .filter(|other| other.sequence > delivered[0].0)
            .collect();
        assert_eq!(results.len(), expected, "result of {event:?}");
    }
    for event in events {
        if let EventKind::ModelCall(data) = &event.kind {
            let request = serde_json::to_string(&data.request).unwrap();
            assert!(!request.contains("op_event_id") && !request.contains("request_event_id"));
        }
    }
}

async fn assert_no_collaboration_facts(session: &Session) {
    assert!(
        !events(session)
            .await
            .iter()
            .any(|event| event.kind.key().starts_with("collaboration/"))
    );
}

/// Polls `check` until it yields a value; fails at the 10s deadline instead
/// of hanging.
async fn poll<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = check().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("deadline: {what}"))
}

/// Releases an idle Session instance, as a host may; the server itself
/// keeps instances loaded.
pub(super) async fn unload(agent: &Agent, id: &str) {
    poll("idle unload", async || match agent.get_session(id) {
        None => Some(()),
        Some(session) => {
            let _ = session.unload_if_idle().await;
            None
        }
    })
    .await
}

/// Waits for `count` result inputs, then for the leader to take them.
async fn returned(client: &mut Client, lead: &str, session: &Session, count: usize) {
    poll("result inputs", async || {
        (events(session)
            .await
            .iter()
            .filter(|event| {
                matches!(&event.kind,
            EventKind::InputMessage(data) if report(data).is_some())
            })
            .count()
            >= count)
            .then_some(())
    })
    .await;
    idle(client, lead).await;
}

/// The member of the `count`-th successful request, once delivered.
async fn next_member(session: &Session, count: usize) -> String {
    poll("delivered request", async || {
        results(&events(session).await)
            .into_iter()
            .filter_map(|(_, text)| serde_json::from_str::<Value>(&text).ok())
            .filter(|value| value["status"] == "running")
            .map(|value| value["sessionId"].as_str().unwrap().to_owned())
            .nth(count)
    })
    .await
}

/// Waits until the Session reports a running turn.
async fn running(client: &mut Client, session: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let listed = client.rpc("session.list", json!({})).await;
            let info = listed["result"]
                .as_array()
                .unwrap()
                .iter()
                .find(|info| info["sessionId"] == session)
                .unwrap()
                .clone();
            if info["running"].as_bool().unwrap() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("deadline: running turn");
}

fn result_of(events: &[Arc<AgentEvent>], member: &str) -> Vec<CollaborationOutcome> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::CollaborationResult(data) if data.session_id == member => {
                Some(data.outcome.clone())
            }
            _ => None,
        })
        .collect()
}

/// Leader request/result, existing-member request, refusals, member
/// message/read/list and the role-specific tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_delegates_and_team_members_communicate() {
    let (server, agent) = launch_team().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let rejected = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "collaborationRole": "member"}),
        )
        .await;
    assert_eq!(rejected["error"]["code"], -32602, "{rejected}");

    let lead = leader(&mut client, allowed()).await;
    let lead_session = agent.get_session(&lead).unwrap();
    assert_eq!(
        lead_session.config().collaboration_role,
        CollaborationRole::Leader
    );
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": lead}))
            .await,
    );
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();

    // A new member returns immediately; its result arrives as a later input.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": "hello"})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 1).await;
    let timeline = events(&lead_session).await;
    let (_, first) = results(&timeline)[0].clone();
    let first: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["status"], "running");
    assert_eq!(first["agentId"], default_agent.as_str());
    let member = first["sessionId"].as_str().unwrap().to_owned();
    let member_session = agent.get_session(&member).unwrap();
    // Operations reach a peer as its own ordinary events only.
    assert_no_collaboration_facts(&member_session).await;
    // Tree clients see the leader's collaboration facts live.
    let mut names = vec![];
    while names.len() < 3 {
        let notice = client.notice("session.event").await;
        let entry = &notice["params"];
        if entry["sessionId"] == lead.as_str() && entry["event"]["type"] == "collaboration" {
            names.push(entry["event"]["name"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(names, ["request", "delivered", "result"]);
    // Later assertions drive Sessions directly without draining this client.
    // Stop the live subscription once its delivery contract is verified.
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId": lead}))
            .await,
    );
    // Clients of the leader's protocol see the member Session.
    let listed = client.rpc("session.list", json!({})).await;
    assert!(
        listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|info| info["sessionId"] == member.as_str())
    );
    assert_eq!(
        member_session.config().collaboration_role,
        CollaborationRole::Member
    );
    assert!(
        member_session
            .config()
            .tools
            .iter()
            .any(|tool| tool == "agent")
    );
    let delivered = agent_results(&timeline);
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].starts_with(&format!(
        "<dim-agent-report agent=\"{default_agent}\" session=\"{member}\">\nStatus: completed.\nResult preview:\nEcho: hello\n"
    )));
    assert!(delivered[0].contains("Full result: agent tool, action \"result\", resultId \""));
    assert!(delivered[0].ends_with(".\n</dim-agent-report>"));
    // The leader continues with the result input.
    assert!(timeline.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.delta.starts_with("Echo: <dim-agent-report"))));

    // The same member takes another request.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": default_agent, "sessionId": member,
            "prompt": "again"}),
        ),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 2).await;
    let timeline = events(&lead_session).await;
    assert_eq!(last_ok(&timeline)["status"], "running");
    assert!(
        agent_results(&timeline)[1].contains("Status: completed.\nResult preview:\nEcho: again\n")
    );

    // A working member is busy; unknown members and Agents are refused.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": default_agent, "sessionId": member,
            "prompt": "tool:wait {\"millis\":60000}"}),
        ),
        "queue",
    )
    .await;
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": default_agent, "sessionId": member,
            "prompt": "later"}),
        ),
        "queue",
    )
    .await;
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": default_agent, "sessionId": "nobody",
            "prompt": "x"}),
        ),
        "queue",
    )
    .await;
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": "missing", "prompt": "x"})),
        "queue",
    )
    .await;
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "profile": {}, "prompt": "x"})),
        "queue",
    )
    .await;
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "prompt": "x", "profile": {"name": "bad",
            "description": "", "systemPrompt": "", "tools": ["nope"]}}),
        ),
        "queue",
    )
    .await;
    // The member keeps working until the leader cancels it.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "cancel", "agentId": default_agent, "sessionId": member})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 3).await;
    let timeline = events(&lead_session).await;
    let refusals: Vec<String> = results(&timeline)
        .into_iter()
        .filter(|(error, _)| *error)
        .map(|(_, text)| text.split(':').next().unwrap().to_owned())
        .collect();
    assert_eq!(
        refusals,
        [
            "busy",
            "team_member_not_found",
            "agent_not_found",
            "invalid_input",
            "invalid_input"
        ]
    );
    // Profile tools are checked at the tool boundary: no operation event.
    let (_, profile_refusal) = results(&timeline)
        .into_iter()
        .rfind(|(error, _)| *error)
        .unwrap();
    assert!(
        profile_refusal.contains("tool nope is not provided by the process"),
        "{profile_refusal}"
    );
    assert!(!timeline.iter().any(|event| matches!(&event.kind,
        EventKind::CollaborationRequest(data)
            if matches!(data.target, dim_agent_types::CollaborationTarget::Temporary { .. }))));
    assert_eq!(last_ok(&timeline)["status"], "cancelled");
    assert!(agent_results(&timeline)[2].contains("Status: cancelled."));

    let read_after = lead_session.sequence("main").unwrap();
    // Leader list shows the Team.
    say(&mut client, &lead, tool(json!({"action": "list"})), "queue").await;
    idle(&mut client, &lead).await;
    let listed = last_ok(&events(&lead_session).await);
    assert_eq!(listed["role"], "leader");
    assert_eq!(
        listed["agents"],
        json!([{"agentId": default_agent, "name": "default", "description": ""}])
    );
    let participants = listed["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 2);
    assert_eq!(participants[0]["role"], "leader");
    assert_eq!(participants[0]["sessionId"], lead.as_str());
    assert_eq!(participants[1]["role"], "member");
    assert_eq!(participants[1]["sessionId"], member.as_str());
    assert_eq!(participants[1]["status"], "idle");

    // The member messages its leader, reads it and lists the Team; it
    // cannot request.
    let before = events(&lead_session)
        .await
        .iter()
        .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .count();
    member_session
        .submit(
            tool(
                json!({"action": "message", "agentId": default_agent, "sessionId": lead,
                "content": "note for leader"}),
            ),
            dim_agent_types::InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    member_session.idle("main").await.unwrap();
    let member_timeline = events(&member_session).await;
    let sent = last_ok(&member_timeline);
    assert_eq!(sent["status"], "delivered");
    assert_eq!(sent["mode"], "append");
    let leader_inputs = events(&lead_session)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(leader_inputs.len(), before + 1);
    assert!(agent_message(&leader_inputs[before]) == Some("note for leader"));
    assert_eq!(
        leader_inputs[before].mode,
        Some(dim_agent_types::InputMode::Append)
    );
    let facts = async |session: &Session| {
        events(session)
            .await
            .iter()
            .filter(|event| event.kind.key().starts_with("collaboration/"))
            .count()
    };
    let before = facts(&member_session).await;
    for input in [
        // Read recent activity from the last observed batch boundary.
        json!({"action": "read", "agentId": default_agent, "sessionId": lead, "after": read_after, "limit": 200}),
        json!({"action": "list"}),
        json!({"action": "request", "agentId": default_agent, "prompt": "x"}),
        json!({"action": "inspect", "agentId": default_agent}),
    ] {
        member_session
            .submit(tool(input), dim_agent_types::InputMode::Queue, "main")
            .await
            .unwrap();
        member_session.idle("main").await.unwrap();
    }
    let member_results = results(&events(&member_session).await);
    let read: Value = serde_json::from_str(&member_results[member_results.len() - 4].1).unwrap();
    assert_eq!(read["sessionId"], lead.as_str());
    assert!(read["nextCursor"].as_u64().unwrap() > 0);
    let items = read["events"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["type"] == "input" && item.to_string().contains("note for leader"))
    );
    assert!(
        items
            .iter()
            .any(|item| item["type"] == "tool_call" && item["name"] == "agent")
    );
    assert!(
        items
            .iter()
            .any(|item| item["type"] == "turn_end" && item["status"] == "completed")
    );
    let listed: Value = serde_json::from_str(&member_results[member_results.len() - 3].1).unwrap();
    assert_eq!(listed["role"], "member");
    assert!(listed.get("agents").is_none());
    assert_eq!(listed["participants"].as_array().unwrap().len(), 2);
    // Members neither delegate nor inspect Agents; refusals write no event.
    for (error, text) in &member_results[member_results.len() - 2..] {
        assert!(*error && text.starts_with("invalid_input"), "{text}");
    }
    assert_eq!(facts(&member_session).await, before);

    // Leader steer message reaches the member as a steer input.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "message", "agentId": default_agent, "sessionId": member,
            "content": "change course", "mode": "steer"}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    member_session.idle("main").await.unwrap();
    assert_eq!(last_ok(&events(&lead_session).await)["mode"], "steer");
    assert!(
        events(&member_session)
            .await
            .iter()
            .any(|event| matches!(&event.kind,
        EventKind::InputMessage(data) if agent_message(data) == Some("change course")
            && data.mode == Some(dim_agent_types::InputMode::Steer)))
    );

    assert_pairing(&events(&lead_session).await);
    assert_pairing(&events(&member_session).await);
    let member_tools = events(&member_session)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.tools.clone(),
            _ => None,
        })
        .unwrap();
    let schema = serde_json::to_value(
        &member_tools
            .iter()
            .find(|t| t.name == "agent")
            .unwrap()
            .input_schema,
    )
    .unwrap();
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["message", "read", "list"])
    );

    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn temporary_agents_stay_private_during_concurrent_create_and_close() {
    let (server, agent) = launch_team().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(&mut client, allowed()).await;
    let lead_session = agent.get_session(&lead).unwrap();
    let foreign = leader(&mut client, allowed()).await;
    let foreign_session = agent.get_session(&foreign).unwrap();
    let done = CancellationToken::new();
    let finished = done.clone();
    let reader = tokio::spawn(async move {
        let mut reads = 0;
        while !finished.is_cancelled() || reads == 0 {
            foreign_session
                .submit(
                    tool(json!({"action":"list"})),
                    dim_agent_types::InputMode::Queue,
                    "main",
                )
                .await
                .unwrap();
            foreign_session.idle("main").await.unwrap();
            let listed = last_ok(&events(&foreign_session).await);
            // Positive control: the public default remains visible; private
            // Agents may never appear, including stale snapshots during close.
            let agents = listed["agents"].as_array().unwrap();
            assert_eq!(agents.len(), 1, "{listed}");
            assert_eq!(agents[0]["name"], "default");
            reads += 1;
        }
        reads
    });
    for request in 1..=8 {
        say(
            &mut client,
            &lead,
            tool(json!({"action":"request", "prompt":"hello", "profile": {
                "name":"private", "description":"private profile", "systemPrompt":"", "tools":[]
            }})),
            "queue",
        )
        .await;
        returned(&mut client, &lead, &lead_session, request).await;
        let started = last_ok(&events(&lead_session).await);
        assert_ok(
            &client
                .rpc("session.close", json!({"sessionId":started["sessionId"]}))
                .await,
        );
        poll("temporary Agent removed", async || {
            let listed = client.rpc("agent.list", json!({})).await;
            (listed["result"].as_array().unwrap().len() == 1).then_some(())
        })
        .await;
    }
    done.cancel();
    assert!(reader.await.unwrap() > 0);
    server.shutdown();
    server.join().await.unwrap();
}

/// cancel ends a request cancelled and stops other member work as idle,
/// close removes the member but keeps its Session, a temporary Agent closes
/// with its last Session, and an externally closed member fails its request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_cancel_close_and_leave_the_team() {
    let (server, agent) = launch_team().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(&mut client, allowed()).await;
    let lead_session = agent.get_session(&lead).unwrap();
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let long = "tool:wait {\"millis\":30000}";

    // Temporary Agent: cancel, then close.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "prompt": long, "profile": {"name": "helper",
            "description": "temporary", "systemPrompt": "", "tools": ["wait"]}}),
        ),
        "queue",
    )
    .await;
    let started = poll("temporary request", async || {
        let timeline = events(&lead_session).await;
        (!results(&timeline).is_empty()).then(|| last_ok(&timeline))
    })
    .await;
    let temporary = started["agentId"].as_str().unwrap().to_owned();
    let member = started["sessionId"].as_str().unwrap().to_owned();
    assert_ne!(temporary, default_agent);
    let agents = client.rpc("agent.list", json!({})).await;
    assert_eq!(agents["result"].as_array().unwrap().len(), 2);
    say(
        &mut client,
        &lead,
        tool(json!({"action": "cancel", "agentId": temporary, "sessionId": member})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 1).await;
    let timeline = events(&lead_session).await;
    assert_eq!(last_ok(&timeline)["status"], "cancelled");
    assert!(matches!(
        result_of(&timeline, &member)[..],
        [CollaborationOutcome::Cancelled { .. }]
    ));
    assert!(agent_results(&timeline)[0].contains("Status: cancelled."));
    // Cancellation preserves Team addressing and the temporary Agent's privacy.
    say(&mut client, &lead, tool(json!({"action": "list"})), "queue").await;
    idle(&mut client, &lead).await;
    let listed = last_ok(&events(&lead_session).await);
    let participants = listed["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 2);
    assert!(
        participants
            .iter()
            .any(|entry| entry["sessionId"] == member)
    );
    let foreign = leader(&mut client, allowed()).await;
    let foreign_session = agent.get_session(&foreign).unwrap();
    say(
        &mut client,
        &foreign,
        tool(json!({"action": "inspect", "agentId": temporary})),
        "queue",
    )
    .await;
    idle(&mut client, &foreign).await;
    assert!(last_error(&events(&foreign_session).await).starts_with("agent_not_found"));
    // A steer message starts a member turn outside
    // any request; cancel still stops it and reports idle.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "message", "agentId": temporary, "sessionId": member,
            "content": long, "mode": "steer"}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    running(&mut client, &member).await;
    say(
        &mut client,
        &lead,
        tool(json!({"action": "cancel", "agentId": temporary, "sessionId": member})),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    assert_eq!(last_ok(&events(&lead_session).await)["status"], "idle");
    // cancel is delivered once the member's stop has settled its turn, so
    // the cancelled turn is already on the member Timeline.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "read", "agentId": temporary, "sessionId": member})),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let read = last_ok(&events(&lead_session).await);
    let ends: Vec<&Value> = read["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "turn_end")
        .collect();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[1]["status"], "cancelled");
    idle(&mut client, &member).await;
    // A temporary member is reused through its own address only.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": temporary, "sessionId": member,
            "prompt": "again"}),
        ),
        "queue",
    )
    .await;
    poll("second result", async || {
        (result_of(&events(&lead_session).await, &member).len() >= 2).then_some(())
    })
    .await;
    idle(&mut client, &lead).await;
    assert!(matches!(
        result_of(&events(&lead_session).await, &member)[1],
        CollaborationOutcome::Completed
    ));
    // Another leader neither sees nor uses this Team's temporary Agent.
    let other = leader(&mut client, allowed()).await;
    let other_session = agent.get_session(&other).unwrap();
    say(
        &mut client,
        &other,
        tool(json!({"action": "request", "agentId": temporary, "prompt": "new"})),
        "queue",
    )
    .await;
    say(
        &mut client,
        &other,
        tool(json!({"action": "list"})),
        "queue",
    )
    .await;
    say(
        &mut client,
        &other,
        tool(json!({"action": "inspect", "agentId": temporary})),
        "queue",
    )
    .await;
    idle(&mut client, &other).await;
    let other_results = results(&events(&other_session).await);
    assert!(other_results[2].0 && other_results[2].1.starts_with("agent_not_found"));
    assert!(
        other_results[0].0 && other_results[0].1.starts_with("agent_not_found"),
        "{}",
        other_results[0].1
    );
    let other_list: Value = serde_json::from_str(&other_results[1].1).unwrap();
    assert!(
        other_list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent["agentId"] != temporary.as_str())
    );
    // Its creator reads the full profile on demand rather than in list.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "inspect", "agentId": temporary})),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let inspected = last_ok(&events(&lead_session).await);
    assert_eq!(inspected["profile"]["name"], "helper");
    assert_eq!(inspected["profile"]["tools"], json!(["wait"]));
    say(
        &mut client,
        &lead,
        tool(json!({"action": "close", "agentId": temporary, "sessionId": member})),
        "queue",
    )
    .await;
    say(&mut client, &lead, tool(json!({"action": "list"})), "queue").await;
    idle(&mut client, &lead).await;
    let timeline = events(&lead_session).await;
    let tail: Vec<Value> = results(&timeline)
        .into_iter()
        .rev()
        .take(2)
        .map(|(_, text)| serde_json::from_str(&text).unwrap())
        .collect();
    assert_eq!(tail[1]["status"], "closed");
    assert_eq!(tail[0]["participants"], json!([]));
    assert!(
        tail[0]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent["agentId"] == temporary.as_str())
    );
    assert_eq!(result_of(&timeline, &member).len(), 2);
    // Leaving the Team keeps the member Session and its temporary Agent.
    let agents = client.rpc("agent.list", json!({})).await;
    assert_eq!(agents["result"].as_array().unwrap().len(), 2);
    let listed = client.rpc("session.list", json!({})).await;
    assert!(
        listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|info| info["sessionId"] == member.as_str())
    );
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": member}))
            .await,
    );
    // Its last Session closing closes the temporary Agent.
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": member}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.rpc("agent.list", json!({})).await["result"]
            .as_array()
            .unwrap()
            .len()
            != 1
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("temporary Agent closed with its last Session");

    // Closing a working member settles its request before the relation goes;
    // the member Session of a persistent Agent stays.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": long})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 2).await;
    say(
        &mut client,
        &lead,
        tool(json!({"action": "close", "agentId": default_agent, "sessionId": member})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 3).await;
    let timeline = events(&lead_session).await;
    assert_eq!(last_ok(&timeline)["status"], "closed");
    assert!(matches!(
        result_of(&timeline, &member)[..],
        [CollaborationOutcome::Cancelled { .. }]
    ));
    assert!(agent.get_session(&member).is_some());
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "message", "agentId": default_agent, "sessionId": member,
            "content": "still there?"}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    assert!(last_error(&events(&lead_session).await).starts_with("team_member_not_found"));

    // A member closed from outside ends its request as failed.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": long})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 3).await;
    agent.get_session(&member).unwrap().close().await.unwrap();
    returned(&mut client, &lead, &lead_session, 4).await;
    let timeline = events(&lead_session).await;
    assert_eq!(
        result_of(&timeline, &member),
        [CollaborationOutcome::Failed {
            reason: "member closed".into()
        }]
    );
    assert!(
        agent_results(&timeline)
            .last()
            .unwrap()
            .contains("Status: failed.\nResult preview:\nmember closed")
    );
    assert_pairing(&timeline);

    // Reloading Agents settles a Team with work in flight.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": long})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 4).await;
    tokio::time::timeout(Duration::from_secs(10), server.restart("Agents"))
        .await
        .expect("Agents reload settled the Team")
        .unwrap();
    assert!(server.agent().is_some());
    assert_eq!(
        result_of(&events(&lead_session).await, &member),
        [CollaborationOutcome::Failed {
            reason: dim_agent_runtime::CAPABILITY_RELOADING.into(),
        }]
    );
    // The registry follows the provider's new generation (#241): the next
    // request reaches it without a Session reload, and host tools keep working.
    say(&mut client, &lead, tool(json!({"action": "list"})), "queue").await;
    idle(&mut client, &lead).await;
    assert_eq!(last_ok(&events(&lead_session).await)["role"], "leader");
    say(
        &mut client,
        &lead,
        "tool:echo {\"text\":\"still running\"}".into(),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let (error, text) = results(&events(&lead_session).await).pop().unwrap();
    assert!(!error && text.contains("still running"), "{text}");

    server.shutdown();
    server.join().await.unwrap();
}

/// A leader stop covers only requests of its active turn and returns no
/// result input for them; closing the leader settles its requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_stop_and_close_settle_requests() {
    let (server, agent) = launch_team().await;
    let mut client = Client::new_version(&server, false, 3).await;
    // `agent` needs approval, which holds the leader's turn open.
    let lead = leader(
        &mut client,
        json!({"preset": "workspace-write", "allowedTools": ["echo", "wait"]}),
    )
    .await;
    let lead_session = agent.get_session(&lead).unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": lead}))
            .await,
    );
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    /// Allows the pending agent call; `steer` is submitted while it waits.
    async fn approve(client: &mut Client, lead: &str, steer: Option<&str>) {
        let request = client.notice("approval.request").await;
        let id = request["params"]["requestId"].as_str().unwrap();
        if let Some(text) = steer {
            say(client, lead, text.into(), "steer").await;
        }
        assert_ok(
            &client
                .rpc(
                    "approval.respond",
                    json!({"sessionId": lead, "branchId": "main", "requestId": id,
                        "decision": "allow"}),
                )
                .await,
        );
    }

    // An earlier turn's request keeps working through the stop.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent,
            "prompt": "tool:wait {\"millis\":60000}"})),
        "queue",
    )
    .await;
    approve(&mut client, &lead, None).await;
    let earlier = next_member(&lead_session, 0).await;
    // The steer joins the active turn after the agent tool result.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent,
            "prompt": "tool:wait {\"millis\":30000}"})),
        "queue",
    )
    .await;
    approve(&mut client, &lead, Some("tool:wait {\"millis\":30000}")).await;
    let covered = next_member(&lead_session, 1).await;
    poll("steered wait call", async || {
        events(&lead_session)
            .await
            .iter()
            .any(|event| {
                matches!(&event.kind,
            EventKind::ToolCall(data) if data.calls[0].name == "wait")
            })
            .then_some(())
    })
    .await;
    // Stop covers only the active turn. Delivered requests are detached work
    // of the leader: both members keep working until stopped from outside.
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId": lead, "branchId": "main"}),
            )
            .await,
    );
    let earlier_session = agent.get_session(&earlier).unwrap();
    assert!(earlier_session.state("main").unwrap().running);
    let stopped = async |member: &str| {
        events(&agent.get_session(member).unwrap())
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
    };
    assert!(!stopped(&covered).await);
    assert!(!stopped(&earlier).await);
    for member in [&covered, &earlier] {
        agent
            .get_session(member)
            .unwrap()
            .stop("main", "stopped from outside".into())
            .await
            .unwrap();
    }
    // Their reports arrive after the stop: like any later input, they drive
    // a leader turn.
    returned(&mut client, &lead, &lead_session, 2).await;
    let timeline = events(&lead_session).await;
    for member in [&covered, &earlier] {
        assert_eq!(
            result_of(&timeline, member),
            [CollaborationOutcome::Cancelled {
                reason: "stopped from outside".into()
            }]
        );
    }
    let reports = timeline
        .iter()
        .filter(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
                if report(data).is_some())
        })
        .count();
    assert_eq!(reports, 2);
    lead_session.idle("main").await.unwrap();

    // Closing the leader settles its in-flight request without an input.
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": default_agent, "sessionId": earlier,
            "prompt": "tool:wait {\"millis\":30000}"}),
        ),
        "queue",
    )
    .await;
    approve(&mut client, &lead, None).await;
    let _ = next_member(&lead_session, 2).await;
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": lead}))
            .await,
    );
    // The reply follows complete cleanup: the closed Timeline is final.
    let closed = lead_session.local("main", ..).await.unwrap();
    assert_eq!(
        result_of(&closed, &earlier).last(),
        Some(&CollaborationOutcome::Cancelled {
            reason: "leader closed".into()
        })
    );
    assert_eq!(agent_results(&closed).len(), 2);
    assert_pairing(&closed);
    let member = agent.get_session(&earlier).unwrap();
    member.idle("main").await.unwrap();
    assert!(!member.state("main").unwrap().running);
    assert_no_collaboration_facts(&member).await;

    server.shutdown();
    server.join().await.unwrap();
}

/// Mock tools plus `hold`, which returns once the test adds a permit.
struct GateTools(Arc<tokio::sync::Semaphore>);
impl ToolDriver for GateTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = MockTools.definitions();
        tools.push(dim_agent_capabilities::tool_registry::definition(
            "hold",
            "Returns once the test releases it.",
            json!({"type": "object"}),
        ));
        tools
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        if call.name != "hold" {
            let mock: &'static MockTools = &MockTools;
            return mock.execute(call, context, cancel);
        }
        Box::pin(async move {
            tokio::select! {
                permit = self.0.acquire() => {
                    permit.unwrap().forget();
                    Ok(ToolResult::text("released"))
                }
                _ = cancel.cancelled() => Err(
                    dim_agent_capabilities::host::HostError::Cancelled("hold cancelled".into()),
                ),
            }
        })
    }
}

/// Messages to a working member join its running turn at the next tool
/// result; a request's work spans the member's turns until it is idle and
/// returns the last turn's output. `read` pages through a Timeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_work_absorbs_messages_and_spans_turns() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    // The default Agent's members can hold.
    let mut config = ServerConfig::default();
    config.session_defaults.tools.push("hold".into());
    let (server, agent) = launch_with_agent(Arc::new(GateTools(gate.clone())), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(
        &mut client,
        json!({"allowedTools": ["echo", "wait", "agent", "hold"]}),
    )
    .await;
    let lead_session = agent.get_session(&lead).unwrap();
    let holder = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let holds = async |session: &Session| {
        events(session)
            .await
            .iter()
            .filter(|event| {
                matches!(&event.kind,
                EventKind::ToolCall(data) if data.calls[0].name == "hold")
            })
            .count()
    };

    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": holder, "prompt": "tool:hold {}"})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 0).await;
    let member_session = agent.get_session(&member).unwrap();
    let message = |content: &str, mode: &str| {
        tool(
            json!({"action": "message", "agentId": holder, "sessionId": member,
            "content": content, "mode": mode}),
        )
    };
    poll("first hold", async || {
        (holds(&member_session).await == 1).then_some(())
    })
    .await;
    // A steer joins the running turn after the held tool returns.
    let steer = "tool:hold {\"text\":\"steered\"}";
    say(&mut client, &lead, message(steer, "steer"), "queue").await;
    idle(&mut client, &lead).await;
    assert_eq!(last_ok(&events(&lead_session).await)["mode"], "steer");
    gate.add_permits(1);
    poll("steered hold", async || {
        (holds(&member_session).await == 2).then_some(())
    })
    .await;
    // So does an append; a host queue input waits for the turn to end and
    // opens another turn inside the same work.
    say(
        &mut client,
        &lead,
        message("appended note", "append"),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    assert_eq!(last_ok(&events(&lead_session).await)["mode"], "append");
    member_session
        .submit(
            "follow-up".into(),
            dim_agent_types::InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    gate.add_permits(1);
    returned(&mut client, &lead, &lead_session, 1).await;

    let timeline = events(&member_session).await;
    let starts: Vec<(usize, String)> = timeline
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match &event.kind {
            EventKind::TurnStart(data) => Some((at, data.turn_id.clone())),
            _ => None,
        })
        .collect();
    let ends: Vec<usize> = timeline
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(&event.kind, EventKind::TurnEnd(data)
                if matches!(data.outcome, dim_agent_types::TurnOutcome::Completed { .. }))
        })
        .map(|(at, _)| at)
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(ends.len(), 2);
    let consumed = |text: &str| {
        let input = timeline
            .iter()
            .find(|event| {
                matches!(&event.kind, EventKind::InputMessage(data)
                if agent_message(data) == Some(text)
                || matches!(data.user.as_ref().map(|user| user.content.as_slice()),
                    Some([ContentBlock::Text { text: content }]) if content == text))
            })
            .unwrap();
        timeline
            .iter()
            .position(|event| {
                matches!(&event.kind,
                EventKind::InputConsumed(data) if data.input_id == input.id)
            })
            .unwrap()
    };
    let tool_results = |range: std::ops::Range<usize>| {
        timeline[range]
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
            .count()
    };
    // Each message joins the first turn right after the tool result it
    // arrived during.
    for (text, after_results) in [(steer, 1), ("appended note", 2)] {
        let at = consumed(text);
        assert!(
            starts[0].0 < at && at < ends[0],
            "{text} joins the first turn"
        );
        assert_eq!(tool_results(starts[0].0..at), after_results, "{text}");
    }
    // The queued input opens the second turn, which consumes it.
    let follow = consumed("follow-up");
    assert!(ends[0] < starts[1].0 && starts[1].0 < follow);
    assert!(timeline.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.delta == "Echo: appended note")));

    // The result is the last turn's output.
    let result = events(&lead_session)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationResult(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(result.outcome, CollaborationOutcome::Completed);
    assert_eq!(result.text, "Echo: follow-up");
    assert_eq!(result.last_turn_id.as_deref(), Some(starts[1].1.as_str()));
    assert!(
        agent_results(&events(&lead_session).await)[0]
            .contains("Status: completed.\nResult preview:\nEcho: follow-up\n")
    );

    // Paging with `after` and `limit` along `nextCursor` yields the full
    // read: nothing repeated, nothing skipped.
    let read = |after: u64, limit: usize| {
        tool(
            json!({"action": "read", "agentId": holder, "sessionId": member,
            "after": after, "limit": limit}),
        )
    };
    for limit in [0, 201, usize::MAX] {
        say(&mut client, &lead, read(0, limit), "queue").await;
        idle(&mut client, &lead).await;
        let error = last_error(&events(&lead_session).await);
        assert!(error.starts_with("invalid_input"), "{error}");
        assert!(
            error.contains("1 and 200") && error.contains("nextCursor"),
            "{error}"
        );
    }
    say(&mut client, &lead, read(0, 200), "queue").await;
    idle(&mut client, &lead).await;
    let history = events(&lead_session).await;
    let schema = history
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data
                .request
                .tools
                .as_ref()
                .unwrap()
                .iter()
                .find(|tool| tool.name == "agent")
                .map(|tool| serde_json::to_value(&tool.input_schema).unwrap()),
            _ => None,
        })
        .unwrap();
    assert_eq!(schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(schema["properties"]["limit"]["maximum"], 200);
    assert_eq!(schema["properties"]["limit"]["default"], 200);
    let full = last_ok(&history);
    let end = member_session.sequence("main").unwrap();
    assert_eq!(full["nextCursor"], end);
    let mut paged = vec![];
    let mut after = 0;
    let mut pages = 0;
    let mut split_call = false;
    loop {
        say(&mut client, &lead, read(after, 1), "queue").await;
        idle(&mut client, &lead).await;
        let page = last_ok(&events(&lead_session).await);
        let next = page["nextCursor"].as_u64().unwrap();
        let items = page["events"].as_array().unwrap();
        for item in items {
            let sequence = item["sequence"].as_u64().unwrap();
            assert!(after < sequence && sequence <= next, "{item}");
        }
        // A result whose call sits on an earlier page still names its tool.
        split_call |= items.iter().any(|item| {
            item["type"] == "tool_result"
                && !items
                    .iter()
                    .any(|other| other["type"] == "tool_call" && other["callId"] == item["callId"])
        });
        paged.extend(items.iter().cloned());
        if next == after {
            assert!(items.is_empty());
            break;
        }
        after = next;
        pages += 1;
        assert!(pages <= end, "paging advances");
    }
    assert!(pages > 1);
    assert!(
        split_call,
        "a page must start with a result of an earlier call"
    );
    assert_eq!(after, end);
    assert_eq!(&paged, full["events"].as_array().unwrap());

    assert_pairing(&events(&lead_session).await);
    assert_no_collaboration_facts(&member_session).await;
    server.shutdown();
    server.join().await.unwrap();
}

/// Echoes like the mock host, except that a request whose last message asks
/// for `partial-call` starts an `echo` call and streams nothing more until it
/// is cancelled, leaving a call that only started.
struct PartialCallModel;
impl dim_agent_capabilities::host::ModelDriver for PartialCallModel {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: tokio_util::sync::CancellationToken,
    ) -> dim_agent_capabilities::host::HostFuture<'_, dim_agent_capabilities::host::ModelStream>
    {
        use dim_agent_capabilities::host::{HostError, ModelChunk, ModelStream};
        // Only the prompt delegated to the member starts the partial call; the
        // leader's command, the member's report and read results also carry it.
        let partial = request.messages.last().is_some_and(|message| {
            let text = serde_json::to_string(message).unwrap();
            message.role == dim_agent_types::Role::User
                && text.contains("<dim-agent-message ")
                && text.contains("partial-call")
        });
        if !partial {
            return dim_agent_capabilities::host::ModelDriver::invoke(
                &dim_agent_capabilities::adapters::EchoModel,
                request,
                connection,
                cancel,
            );
        }
        Box::pin(async move {
            let started = futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "partial".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "unfinished".into(),
                    name: "echo".into(),
                }),
            ]);
            // Usage facts put the start more than one search step before
            // the result that closes it.
            let usage = futures_util::stream::iter((0..PARTIAL_USAGE).map(|_| {
                Ok(ModelChunk::Usage(
                    serde_json::from_value(
                        json!({"promptTokens": 0, "completionTokens": 0, "totalTokens": 0}),
                    )
                    .unwrap(),
                ))
            }));
            let started = futures_util::StreamExt::chain(started, usage);
            let cancelled = futures_util::stream::once(async move {
                cancel.cancelled().await;
                Err(HostError::Cancelled("test".into()))
            });
            Ok(Box::pin(futures_util::StreamExt::chain(started, cancelled)) as ModelStream)
        })
    }
}

/// More than one search step, and more than a read page holds.
const PARTIAL_USAGE: u64 = 300;

/// A result closing undispatched calls (ADR agent-029) names them from their
/// `step/tool-use-start`, however far before the read page it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn undispatched_results_name_calls_from_their_starts() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    let config = ServerConfig {
        unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
            "dim-server-test-{}/server.sock",
            uuid::Uuid::new_v4()
        ))),
        ..Default::default()
    };
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(Arc::new(PartialCallModel))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    dim_agent_capabilities::defaults::tool_host(
        &mut builder,
        Arc::new(dim_agent_capabilities::adapters::MockTools),
    );
    dim_app_server::collaboration::install(&mut builder);
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(
        &mut client,
        json!({"allowedTools": ["echo", "wait", "agent"]}),
    )
    .await;
    let lead_session = agent.get_session(&lead).unwrap();
    let holder = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": holder, "prompt": "partial-call"})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 0).await;
    let member_session = agent.get_session(&member).unwrap();
    let start = poll("member call start", async || {
        let events = events(&member_session).await;
        let usage = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ModelUsage(_)))
            .count() as u64;
        (usage == PARTIAL_USAGE)
            .then(|| {
                events
                    .into_iter()
                    .find(|event| matches!(event.kind, EventKind::ToolUseStart(_)))
            })
            .flatten()
    })
    .await;
    // Stopping the output closes the started call without dispatching it.
    member_session.stop("main", "stopped".into()).await.unwrap();
    let through = poll("published facts", async || {
        let events = events(&member_session).await;
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::ToolResult(data) if data.call_event_id.is_none()))
            .then(|| events.last().unwrap().sequence)
    })
    .await;
    // The member's report reaches the leader before the reads.
    poll("member report", async || {
        (agent_results(&events(&lead_session).await).len() == 1).then_some(())
    })
    .await;
    idle(&mut client, &lead).await;
    // A page starting at the result, hundreds of events after the start.
    let after = start.sequence + PARTIAL_USAGE - 1;
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "read", "agentId": holder, "sessionId": member,
            "after": after, "limit": 200}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let read = last_ok(&events(&lead_session).await);
    assert!(read["nextCursor"].as_u64().unwrap() >= through);
    let closed: Vec<_> = read["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "tool_result" && item["callId"] == "unfinished")
        .collect();
    assert_eq!(closed.len(), 1, "{read}");
    assert_eq!(closed[0]["name"], "echo");
    assert_eq!(closed[0]["status"], "error");
    server.shutdown();
    server.join().await.unwrap();
}

/// Collaboration facts reach tree clients (v2 and later) with the leader's
/// events; the v1 main-branch view leaves them out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn collaboration_facts_publish_to_tree_protocols_only() {
    let (server, agent) = launch_team().await;
    let mut tree = Client::new_version(&server, false, 3).await;
    let default_agent = tree.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    for version in [1, 2] {
        let mut client = Client::new_version(&server, false, version).await;
        let lead = leader(&mut client, allowed()).await;
        let lead_session = agent.get_session(&lead).unwrap();
        assert_ok(
            &client
                .rpc("session.attach", json!({"sessionId": lead}))
                .await,
        );
        // v1 addresses main implicitly; v2 names the branch.
        let mut submit = json!({"sessionId": lead, "text":
            tool(json!({"action": "request", "agentId": default_agent, "prompt": "hello"}))});
        let mut wait = json!({"sessionId": lead});
        if version == 2 {
            submit["branchId"] = json!("main");
            wait["branchId"] = json!("main");
        }
        assert_ok(&client.rpc("session.submit", submit).await);
        poll("result input", async || {
            (agent_results(&events(&lead_session).await).len() == 1).then_some(())
        })
        .await;
        assert_ok(&client.rpc("session.idle", wait).await);
        let facts = events(&lead_session)
            .await
            .iter()
            .filter(|event| event.kind.key().starts_with("collaboration/"))
            .count();
        assert_eq!(facts, 3);
        // Closing ends the subscription after every broadcast event.
        assert_ok(
            &client
                .rpc("session.close", json!({"sessionId": lead}))
                .await,
        );
        client.notice("session.closed").await;
        let received: Vec<_> = client
            .notices
            .iter()
            .filter(|notice| notice["method"] == "session.event")
            .map(|notice| &notice["params"])
            .collect();
        assert!(!received.is_empty());
        if version == 1 {
            assert!(
                received
                    .iter()
                    .all(|entry| entry["event"]["type"] != "collaboration"),
                "{received:?}"
            );
        } else {
            let names: Vec<_> = received
                .iter()
                .filter(|entry| entry["event"]["type"] == "collaboration")
                .map(|entry| entry["event"]["name"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(names, ["request", "delivered", "result"]);
        }
    }
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_member_can_unload_be_read_without_loading_and_work_again() {
    let directory = std::env::temp_dir().join(format!("dim-team-store-{}", uuid::Uuid::new_v4()));
    let (server, agent) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, true, 3).await;
    let lead = leader(&mut client, allowed()).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":lead}))
            .await,
    );
    let session = agent.get_session(&lead).unwrap();
    let agent_id = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    say(
        &mut client,
        &lead,
        tool(json!({"action":"request","agentId":agent_id,"prompt":"hello"})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &session, 1).await;
    let member = next_member(&session, 0).await;
    unload(&agent, &member).await;
    let (result_id, expected_result) = events(&session)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationResult(data) => Some((event.id.clone(), data.text.clone())),
            _ => None,
        })
        .unwrap();
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId":lead}))
            .await,
    );
    unload(&agent, &lead).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":lead}))
            .await,
    );
    let session = agent.get_session(&lead).unwrap();
    say(
        &mut client,
        &lead,
        tool(json!({"action":"result","resultId":result_id})),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let restored = last_ok(&events(&session).await);
    assert_eq!(restored["resultId"], result_id);
    assert_eq!(restored["result"]["text"], expected_result);

    say(
        &mut client,
        &lead,
        tool(json!({"action":"read","agentId":agent_id,"sessionId":member})),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let read = last_ok(&events(&session).await);
    assert!(!read["events"].as_array().unwrap().is_empty());
    assert!(
        agent.get_session(&member).is_none(),
        "history reads do not load a member"
    );
    say(
        &mut client,
        &lead,
        tool(json!({"action":"request","agentId":agent_id,"sessionId":member,"prompt":"again"})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &session, 2).await;
    assert!(matches!(
        result_of(&events(&session).await, &member).last(),
        Some(CollaborationOutcome::Completed)
    ));
    // Ordinary client restoration, without another collaboration lookup,
    // must also restore the member's close observation.
    unload(&agent, &member).await;
    server.restart("Agents").await.unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":member}))
            .await,
    );
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":member}))
            .await,
    );
    // Reattach the leader's tools to the current provider generation.
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId":lead}))
            .await,
    );
    unload(&agent, &lead).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":lead}))
            .await,
    );
    let session = agent.get_session(&lead).unwrap();
    poll("restored member relation removed", async || {
        say(&mut client, &lead, tool(json!({"action":"list"})), "queue").await;
        idle(&mut client, &lead).await;
        let listed = last_ok(&events(&session).await);
        listed["participants"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    })
    .await;
    // The leader's ordinary restoration must rearm its watcher too.
    say(
        &mut client,
        &lead,
        tool(json!({"action":"request", "agentId":agent_id, "prompt":"third"})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &session, 3).await;
    let member = last_ok(&events(&session).await)["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let member_session = agent.get_session(&member).unwrap();
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId":lead}))
            .await,
    );
    unload(&agent, &lead).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":lead}))
            .await,
    );
    assert_ok(&client.rpc("session.close", json!({"sessionId":lead})).await);
    poll("restored leader relations removed", async || {
        member_session
            .submit(
                tool(json!({"action":"list"})),
                dim_agent_types::InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        member_session.idle("main").await.unwrap();
        last_ok(&events(&member_session).await)["participants"]
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    })
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

type Hold = Arc<
    dyn Fn(Arc<dim_agent_runtime::Timeline>, Arc<AgentEvent>) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// Runs `hold` on each event of the leader Session before Collaboration's
/// listener sees it, so a test can delay the leader's dispatch at an event.
struct DispatchGate {
    leader: OnceLock<String>,
    hold: Hold,
    /// The leader's latest Timeline service.
    timeline: std::sync::Mutex<Option<Arc<dim_agent_runtime::Timeline>>>,
}
impl DispatchGate {
    fn new(hold: Hold) -> Arc<Self> {
        Arc::new(Self {
            leader: OnceLock::new(),
            hold,
            timeline: Default::default(),
        })
    }
}
struct DispatchGatePlugin {
    scope: dim_agent_runtime::BranchScope,
    gate: Arc<DispatchGate>,
}
impl TypedPlugin for DispatchGatePlugin {
    type Deps = (
        InBranch<dim_agent_runtime::Dispatcher>,
        InBranch<dim_agent_runtime::Timeline>,
        InSession<dim_agent_runtime::SessionIdentity>,
    );
    fn name(&self) -> &str {
        "collaboration-test.dispatch-gate"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(dispatcher), InBranch(timeline), InSession(identity)): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let session = identity.session_id.clone();
            let gate = self.gate.clone();
            if gate.leader.get() == Some(&session) {
                *gate.timeline.lock().unwrap() = Some(timeline.clone());
            }
            dispatcher.subscribe_all(ctx, "collaboration-test.dispatch-gate", move |event| {
                let held = (gate.leader.get() == Some(&session))
                    .then(|| (gate.hold)(timeline.clone(), event));
                Box::pin(async move {
                    if let Some(held) = held {
                        held.await;
                    }
                    Ok(())
                })
            })?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for DispatchGatePlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

/// `launch_team` with the gate's listener ahead of Collaboration's.
async fn launch_gated(gate: Arc<DispatchGate>, config: ServerConfig) -> (RunningServer, Agent) {
    launch_gated_with(gate, config, |_| {}).await
}

/// Holds the creation of the next Session other than the leader until
/// released, once armed.
#[derive(Default)]
struct MemberHold {
    leader: OnceLock<String>,
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
struct MemberHoldPlugin {
    scope: dim_agent_runtime::SessionScope,
    hold: Arc<MemberHold>,
}
impl TypedPlugin for MemberHoldPlugin {
    type Deps = InSession<dim_agent_runtime::SessionIdentity>;
    fn name(&self) -> &str {
        "collaboration-test.member-hold"
    }
    fn apply<'a>(
        &'a self,
        _: &'a Ctx,
        InSession(identity): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let session = identity.session_id.clone();
            if self
                .hold
                .leader
                .get()
                .is_some_and(|leader| *leader != session)
                && self.hold.armed.swap(false, Ordering::SeqCst)
            {
                self.hold.entered.notify_one();
                self.hold.release.notified().await;
            }
            Ok(Effect::Done)
        })
    }
}
impl Capability for MemberHoldPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

async fn launch_gated_with(
    gate: Arc<DispatchGate>,
    mut config: ServerConfig,
    extra: impl FnOnce(&mut AgentBuilder),
) -> (RunningServer, Agent) {
    config.unix_path = Some(std::path::PathBuf::from("/tmp").join(format!(
        "dim-server-test-{}/server.sock",
        uuid::Uuid::new_v4()
    )));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder.timeline_plugin(move |scope| DispatchGatePlugin {
        scope: *scope,
        gate: gate.clone(),
    });
    dim_app_server::collaboration::install(&mut builder);
    extra(&mut builder);
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    (server, agent)
}

/// Committed events of the gated branch hold one of `kind`.
async fn committed_has(timeline: &dim_agent_runtime::Timeline, kind: &str) -> bool {
    let through = timeline.committed();
    timeline
        .local(..=through)
        .await
        .unwrap()
        .iter()
        .any(|event| event.kind.key() == kind)
}

async fn hold_until(timeline: &dim_agent_runtime::Timeline, kind: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !committed_has(timeline, kind).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("deadline: {kind}"));
    // Past the writer's own bookkeeping after its append returns. Only the
    // old behavior needs this margin to show; the fixed one is decided by
    // the event position whatever the timing.
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// A turn stop committed before a request's `delivered` covers the request,
/// even when the delivery's append returns before the stop is handled: the
/// "delivered" mark follows the `delivered` event's position.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_before_delivered_covers_the_request_however_late_it_is_handled() {
    let request_held = Arc::new(tokio::sync::Notify::new());
    let request_release = Arc::new(tokio::sync::Notify::new());
    let gate = DispatchGate::new({
        let (held, release) = (request_held.clone(), request_release.clone());
        Arc::new(move |timeline, event| {
            let (held, release) = (held.clone(), release.clone());
            Box::pin(async move {
                match &event.kind {
                    EventKind::CollaborationRequest(_) => {
                        held.notify_one();
                        release.notified().await;
                    }
                    EventKind::StopInterrupt(_) => {
                        hold_until(&timeline, "collaboration/delivered").await;
                    }
                    _ => {}
                }
            })
        })
    });
    let (server, agent) = launch_gated(gate.clone(), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(&mut client, allowed()).await;
    gate.leader.set(lead.clone()).unwrap();
    let lead_session = agent.get_session(&lead).unwrap();
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": "hello"})),
        "queue",
    )
    .await;
    request_held.notified().await;
    let stop = tokio::spawn({
        let session = lead_session.clone();
        async move { session.stop("main", "stopped".into()).await }
    });
    poll("stop committed", async || {
        events(&lead_session)
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
            .then_some(())
    })
    .await;
    request_release.notify_one();
    stop.await.unwrap().unwrap();
    let member = poll("delivered member", async || {
        events(&lead_session)
            .await
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::CollaborationDelivered(data) => match &data.delivery {
                    CollaborationDelivery::Ok { session_id, .. } => Some(session_id.clone()),
                    _ => None,
                },
                _ => None,
            })
    })
    .await;
    poll("request result", async || {
        (!result_of(&events(&lead_session).await, &member).is_empty()).then_some(())
    })
    .await;
    lead_session.idle("main").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let timeline = events(&lead_session).await;
    let position = |key: &str| {
        timeline
            .iter()
            .find(|event| event.kind.key() == key)
            .unwrap()
            .sequence
    };
    assert!(position("stop/interrupt") < position("collaboration/delivered"));
    // Covered by the stop: its result does not return to the leader.
    let reports = timeline
        .iter()
        .filter(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
                if report(data).is_some())
        })
        .count();
    assert_eq!(reports, 0);
    assert_pairing(&timeline);

    server.shutdown();
    server.join().await.unwrap();
}

/// A cancel committed before the request's `result` reports `cancelled`,
/// even when the result is written before the cancel is delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_before_the_result_reports_cancelled_however_late_it_is_delivered() {
    let gate = DispatchGate::new(Arc::new(|timeline, event| {
        Box::pin(async move {
            if matches!(event.kind, EventKind::CollaborationCancel(_)) {
                hold_until(&timeline, "collaboration/result").await;
            }
        })
    }));
    let (server, agent) = launch_gated(gate.clone(), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(&mut client, allowed()).await;
    gate.leader.set(lead.clone()).unwrap();
    let lead_session = agent.get_session(&lead).unwrap();
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent,
            "prompt": "tool:wait {\"millis\":1000}"})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 0).await;
    idle(&mut client, &lead).await;
    say(
        &mut client,
        &lead,
        tool(json!({"action": "cancel", "agentId": default_agent, "sessionId": member})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 1).await;
    let timeline = events(&lead_session).await;
    let cancel = timeline
        .iter()
        .find(|event| matches!(event.kind, EventKind::CollaborationCancel(_)))
        .unwrap();
    let result = timeline
        .iter()
        .find(|event| matches!(event.kind, EventKind::CollaborationResult(_)))
        .unwrap();
    assert!(cancel.sequence < result.sequence);
    let outcome = timeline
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationDelivered(data) if data.op_event_id == cancel.id => {
                match &data.delivery {
                    CollaborationDelivery::Ok { outcome, .. } => *outcome,
                    other => panic!("cancel refused: {other:?}"),
                }
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        outcome,
        dim_agent_types::CollaborationCancelOutcome::Cancelled
    );

    server.shutdown();
    server.join().await.unwrap();
}

/// The turns a stop covered are rebuilt from the leader's history: after a
/// reload, an operation of an interrupted turn is still refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operation_of_a_turn_interrupted_before_a_reload_is_refused() {
    let directory = std::env::temp_dir().join(format!("dim-team-store-{}", uuid::Uuid::new_v4()));
    let gate = DispatchGate::new(Arc::new(|_, _| Box::pin(async {})));
    let (server, agent) = launch_gated(
        gate.clone(),
        ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, true, 3).await;
    let lead = leader(&mut client, allowed()).await;
    gate.leader.set(lead.clone()).unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": lead}))
            .await,
    );
    let agent_id = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    say(
        &mut client,
        &lead,
        "tool:wait {\"millis\":30000}".into(),
        "queue",
    )
    .await;
    running(&mut client, &lead).await;
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId": lead, "branchId": "main"}),
            )
            .await,
    );
    let session = agent.get_session(&lead).unwrap();
    let turn = events(&session)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::TurnStart(data) => Some(data.turn_id.clone()),
            _ => None,
        })
        .unwrap();
    drop(session);
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId": lead}))
            .await,
    );
    unload(&agent, &lead).await;
    *gate.timeline.lock().unwrap() = None;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": lead}))
            .await,
    );
    let session = agent.get_session(&lead).unwrap();
    let timeline = gate
        .timeline
        .lock()
        .unwrap()
        .clone()
        .expect("reloaded leader");
    // A late operation of the interrupted turn.
    let op = timeline
        .append(
            EventKind::CollaborationRequest(dim_agent_types::CollaborationRequestData {
                turn_id: turn,
                request_id: "late".into(),
                call_id: "late".into(),
                target: dim_agent_types::CollaborationTarget::Agent { agent_id },
                prompt: "hello".into(),
            })
            .into(),
        )
        .unwrap();
    let delivery = poll("late delivered", async || {
        events(&session)
            .await
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::CollaborationDelivered(data) if data.op_event_id == op.id => {
                    Some(data.delivery.clone())
                }
                _ => None,
            })
    })
    .await;
    assert_eq!(
        delivery,
        CollaborationDelivery::Error {
            error: dim_agent_types::CollaborationErrorCode::Interrupted
        }
    );

    server.shutdown();
    server.join().await.unwrap();
}

/// A request covered by a stop handled before its input reaches the member
/// is refused, instead of leaving an input the member runs later without a
/// result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_covered_before_its_member_input_is_refused() {
    let gate = DispatchGate::new(Arc::new(|_, _| Box::pin(async {})));
    let hold = Arc::new(MemberHold::default());
    let (server, agent) = launch_gated_with(gate.clone(), ServerConfig::default(), {
        let hold = hold.clone();
        move |builder| {
            builder.session_plugin(move |scope| MemberHoldPlugin {
                scope: *scope,
                hold: hold.clone(),
            });
        }
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(&mut client, allowed()).await;
    gate.leader.set(lead.clone()).unwrap();
    hold.leader.set(lead.clone()).unwrap();
    let lead_session = agent.get_session(&lead).unwrap();
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    // The delivery stops while it creates its member.
    hold.armed.store(true, Ordering::SeqCst);
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": "hello"})),
        "queue",
    )
    .await;
    hold.entered.notified().await;
    // Stop returns once Loop dispatched the interrupt: Collaboration has
    // handled it too, before the delivery writes the member input.
    lead_session.stop("main", "stopped".into()).await.unwrap();
    hold.release.notify_one();
    let delivery = poll("delivered", async || {
        events(&lead_session)
            .await
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::CollaborationDelivered(data) => Some(data.delivery.clone()),
                _ => None,
            })
    })
    .await;
    assert_eq!(
        delivery,
        CollaborationDelivery::Error {
            error: dim_agent_types::CollaborationErrorCode::Interrupted
        }
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A Team on a disposable native store, so a host can release its idle
/// Sessions; the default Agent's members can `hold` until the test adds a
/// permit to the returned gate.
async fn launch_holding_team() -> (
    RunningServer,
    Agent,
    Arc<tokio::sync::Semaphore>,
    std::path::PathBuf,
) {
    let directory = std::env::temp_dir().join(format!("dim-team-store-{}", uuid::Uuid::new_v4()));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    config.session_defaults.tools.push("hold".into());
    let (server, agent) = launch_with_agent(Arc::new(GateTools(gate.clone())), config).await;
    (server, agent, gate, directory)
}

/// True when the idle Session is still kept from a host's release, which
/// then can only be a hold: its main branch is idle, yet the unload refuses.
async fn held(session: &Session) -> bool {
    session.idle("main").await.unwrap();
    !session.unload_if_idle().await.unwrap()
}

/// Idle unload R4: each open request holds its leader, so an idle leader
/// that no client attaches is not released while a member works. A request
/// refused `busy` keeps no hold, and a cancelled request releases only its
/// own. Once the last result has woken it, the leader unloads, as do its
/// members, and the restored leader keeps every result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_requests_hold_their_leader_until_the_last_result() {
    let (server, agent, gate, directory) = launch_holding_team().await;
    let mut client = Client::new_version(&server, false, 3).await;
    // Driven by calls only: no client attaches to the leader.
    let lead = leader(
        &mut client,
        json!({"allowedTools": ["echo", "wait", "agent", "hold"]}),
    )
    .await;
    let lead_session = agent.get_session(&lead).unwrap();
    let holder = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = tool(json!({"action": "request", "agentId": holder, "prompt": "tool:hold {}"}));
    say(&mut client, &lead, request.clone(), "queue").await;
    let cancelled = next_member(&lead_session, 0).await;
    say(&mut client, &lead, request, "queue").await;
    let completed = next_member(&lead_session, 1).await;
    assert!(held(&lead_session).await);
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": holder, "sessionId": cancelled,
            "prompt": "again"}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    assert!(last_error(&events(&lead_session).await).starts_with("busy"));
    // Cancelling one request releases its hold, not the other's.
    say(
        &mut client,
        &lead,
        tool(json!({"action": "cancel", "agentId": holder, "sessionId": cancelled})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 1).await;
    assert!(held(&lead_session).await);
    // The last result wakes the leader; after that turn nothing holds it.
    gate.add_permits(1);
    returned(&mut client, &lead, &lead_session, 2).await;
    drop(lead_session);
    unload(&agent, &lead).await;
    unload(&agent, &cancelled).await;
    unload(&agent, &completed).await;
    // The unload ended no work: the restored leader has both results.
    idle(&mut client, &lead).await;
    let timeline = events(&agent.get_session(&lead).unwrap()).await;
    assert_eq!(
        result_of(&timeline, &cancelled),
        [CollaborationOutcome::Cancelled {
            reason: "cancelled by leader".into()
        }]
    );
    assert_eq!(
        result_of(&timeline, &completed),
        [CollaborationOutcome::Completed]
    );
    assert_pairing(&timeline);

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// Idle unload R4: a request that ends without its member's own result
/// releases the leader too: when the member is closed from outside, when a
/// reload of Collaboration settles it, and when the leader closes, which its
/// hold never delays. Each settled member then unloads too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_settlement_of_a_request_releases_its_leader() {
    let (server, agent, _gate, directory) = launch_holding_team().await;
    let mut client = Client::new_version(&server, false, 3).await;
    // Driven by calls only: no client attaches to the leader.
    let lead = leader(
        &mut client,
        json!({"allowedTools": ["echo", "wait", "agent", "hold"]}),
    )
    .await;
    let holder = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = tool(json!({"action": "request", "agentId": holder, "prompt": "tool:hold {}"}));

    // The member closes: its request fails and wakes the leader.
    say(&mut client, &lead, request.clone(), "queue").await;
    let lead_session = agent.get_session(&lead).unwrap();
    let closed = next_member(&lead_session, 0).await;
    assert!(held(&lead_session).await);
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": closed}))
            .await,
    );
    returned(&mut client, &lead, &lead_session, 1).await;
    assert_eq!(
        result_of(&events(&lead_session).await, &closed),
        [CollaborationOutcome::Failed {
            reason: "member closed".into()
        }]
    );
    drop(lead_session);
    unload(&agent, &lead).await;

    // A reload of Collaboration fails the open request.
    say(&mut client, &lead, request.clone(), "queue").await;
    let lead_session = agent.get_session(&lead).unwrap();
    let reloaded = next_member(&lead_session, 1).await;
    assert!(held(&lead_session).await);
    server.restart("Agents").await.unwrap();
    let settled = poll("reload result", async || {
        let outcomes = result_of(&events(&lead_session).await, &reloaded);
        (!outcomes.is_empty()).then_some(outcomes)
    })
    .await;
    assert_eq!(
        settled,
        [CollaborationOutcome::Failed {
            reason: dim_agent_runtime::CAPABILITY_RELOADING.into()
        }]
    );
    drop(lead_session);
    unload(&agent, &lead).await;

    // Closing the leader settles its open request without waiting for it.
    say(&mut client, &lead, request, "queue").await;
    let lead_session = agent.get_session(&lead).unwrap();
    let last = next_member(&lead_session, 2).await;
    assert!(held(&lead_session).await);
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": lead}))
            .await,
    );
    let closed_timeline = lead_session.local("main", ..).await.unwrap();
    assert_eq!(
        result_of(&closed_timeline, &last),
        [CollaborationOutcome::Cancelled {
            reason: "leader closed".into()
        }]
    );
    assert_pairing(&closed_timeline);
    unload(&agent, &last).await;
    unload(&agent, &reloaded).await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// Idle unload R4: a request whose new member is ready only after its leader
/// started closing finds no hold to take and is refused `closed`, as an
/// operation of a closing branch is; the member never receives it. The
/// leader's branch interrupt waits at the gate meanwhile, so Collaboration
/// has not handled the close when the member is ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closing_leader_gives_no_hold_and_refuses_the_request() {
    // The leader's Timeline, once its interrupt is held.
    let held_timeline = Arc::new(OnceLock::new());
    let interrupt_arrived = Arc::new(tokio::sync::Notify::new());
    let interrupt_release = Arc::new(tokio::sync::Notify::new());
    let gate = DispatchGate::new({
        let (held, arrived, release) = (
            held_timeline.clone(),
            interrupt_arrived.clone(),
            interrupt_release.clone(),
        );
        Arc::new(move |timeline, event| {
            let (held, arrived, release) = (held.clone(), arrived.clone(), release.clone());
            Box::pin(async move {
                if matches!(event.kind, EventKind::StopInterrupt(_)) {
                    let _ = held.set(timeline);
                    arrived.notify_one();
                    release.notified().await;
                }
            })
        })
    });
    let hold = Arc::new(MemberHold::default());
    let (server, agent) = launch_gated_with(gate.clone(), ServerConfig::default(), {
        let hold = hold.clone();
        move |builder| {
            builder.session_plugin(move |scope| MemberHoldPlugin {
                scope: *scope,
                hold: hold.clone(),
            });
        }
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(&mut client, allowed()).await;
    gate.leader.set(lead.clone()).unwrap();
    hold.leader.set(lead.clone()).unwrap();
    let lead_session = agent.get_session(&lead).unwrap();
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    hold.armed.store(true, Ordering::SeqCst);
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": default_agent, "prompt": "hello"})),
        "queue",
    )
    .await;
    hold.entered.notified().await;
    let closing = tokio::spawn(lead_session.close());
    interrupt_arrived.notified().await;
    hold.release.notify_one();
    let timeline = held_timeline.get().unwrap();
    poll("refused delivery", async || {
        committed_has(timeline, "collaboration/delivered")
            .await
            .then_some(())
    })
    .await;
    interrupt_release.notify_one();
    closing.await.unwrap().unwrap();

    let closed = lead_session.local("main", ..).await.unwrap();
    let delivery = closed
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationDelivered(data) => Some(data.delivery.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        delivery,
        CollaborationDelivery::Error {
            error: dim_agent_types::CollaborationErrorCode::Closed
        }
    );
    assert_pairing(&closed);
    let member = agent
        .sessions()
        .into_iter()
        .find(|session| session.id() != lead)
        .expect("the member Session was created");
    assert!(
        !events(&member)
            .await
            .iter()
            .any(|event| matches!(event.kind, EventKind::InputMessage(_)))
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// The result id a `dim-agent-report` input names.
fn report(data: &InputMessageData) -> Option<&str> {
    let [InputBlock::Raw { text, .. }] = data.blocks.as_slice() else {
        return None;
    };
    text.starts_with("<dim-agent-report ")
        .then(|| text.split_once("resultId \"")?.1.split_once('"'))
        .flatten()
        .map(|(id, _)| id)
}

/// The message a `dim-agent-message` input carries.
fn agent_message(data: &InputMessageData) -> Option<&str> {
    let [InputBlock::Raw { text, .. }] = data.blocks.as_slice() else {
        return None;
    };
    text.strip_prefix("<dim-agent-message ")?
        .split_once(":\n")?
        .1
        .strip_suffix("\n</dim-agent-message>")
}
