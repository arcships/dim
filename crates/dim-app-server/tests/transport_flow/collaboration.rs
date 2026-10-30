use super::*;
use dim_agent_capabilities::prompt_state::PromptSnapshot;
use dim_agent_runtime::{Agent, CollaborationRole, Session};
use dim_agent_types::{
    AgentEvent, CollaborationDelivery, CollaborationOutcome, ContentBlock, EventKind, InputBlock,
    InputMessageData, MessageContent, ToolContent,
};
use rutis::host::{BoxFuture, CtxHost};
use std::ops::RangeInclusive;
use std::sync::OnceLock;

struct CaptureToolRegistry {
    registry: Arc<OnceLock<Arc<dim_agent_capabilities::tool_registry::ToolRegistry>>>,
    tools: rutis::host::Key<dim_agent_capabilities::tool_registry::ToolRegistry>,
}

impl CaptureToolRegistry {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        registry: Arc<OnceLock<Arc<dim_agent_capabilities::tool_registry::ToolRegistry>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            registry,
            tools: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for CaptureToolRegistry {
    fn name(&self) -> &str {
        "collaboration-test.capture-tool-registry"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.tools.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.tools)?;
            let _ = self.registry.set(registry);
            Ok(rutis::host::Effect::Done)
        })
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
    fn release_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.release_session(id)
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
    configs: Arc<std::sync::Mutex<Vec<Arc<dim_agent_runtime::SessionConfig>>>>,

    config: rutis::host::Key<dim_agent_runtime::SessionConfig>,
}
impl CaptureMemberConfig {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        configs: Arc<std::sync::Mutex<Vec<Arc<dim_agent_runtime::SessionConfig>>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            configs,
            config: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for CaptureMemberConfig {
    fn name(&self) -> &str {
        "capture-member-config"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.config.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let config = ctx.require_at(&self.config)?;
            self.configs.lock().unwrap().push(config);
            Ok(rutis::host::Effect::Done)
        })
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
            builder.require_feature(Feature::new("capture-member-config").session(
                "capture-member-config",
                move |names| Ok(Box::new(CaptureMemberConfig::new(names, observed.clone())?)),
            ));
        }).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let capabilities = dim_agent_types::ModelCapabilities {
            context_window: 32000,
            max_output_tokens: 128,
            ..Default::default()
        };
        let models = ["alpha", "beta"].map(|id| json!({"modelId":id,"displayName":id,"status":"active",
            "capabilities":capabilities}));
        assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000003","displayName":"Team fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":models,"enabled":true})).await);
        assert_ok(&client.rpc("provider.select", json!({"providerId":"018ec73f-0000-7000-8000-000000000003","modelId":"alpha"})).await);
        let lead = leader(&mut client, allowed()).await;
        let lead_session = agent.get_session(&lead).unwrap();
        for (step, (model, expected)) in [(Some("beta"), "beta"), (None, "alpha")].into_iter().enumerate() {
            let mut profile = json!({"name":"worker","description":"fixture","systemPrompt":"work","tools":["agent"]});
            if let Some(model) = model {
                profile["model"] = json!({"providerId":"018ec73f-0000-7000-8000-000000000003","modelId":model});
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

        const PROVIDER: &str = "018ec73f-0000-7000-8000-000000000003";
        let model = |id: &str| json!({"providerId":PROVIDER,"modelId":id});
        for method in ["collaboration.model_policy", "collaboration.set_model_policy"] {
            assert!(client.features.as_array().unwrap().contains(&json!(method)));
        }
        let mut remote = Client::new_version(&server, true, 3).await;
        assert_eq!(remote.rpc("collaboration.model_policy", json!({})).await["error"]["data"]["appError"], "local_transport_required");
        let read = client.rpc("collaboration.model_policy", json!({})).await;
        assert_eq!(read["result"]["policy"], json!({"mode":"agent"}));
        let initial = read["result"]["revision"].as_str().unwrap().to_owned();
        for invalid in [
            json!({"mode":"agent","model":model("alpha")}),
            json!({"mode":"inherit","extra":true}),
            json!({"mode":"fixed"}),
            json!({"mode":"fixed","model":{"providerId":PROVIDER,"modelId":""}}),
            json!({"mode":"fixed","model":{"providerId":PROVIDER,"modelId":"alpha","extra":1}}),
            json!({"mode":"leader"}),
        ] {
            let rejected = client.rpc("collaboration.set_model_policy", json!({"expectedRevision":initial,"policy":invalid})).await;
            assert_eq!(rejected["error"]["code"], -32602, "{invalid}: {rejected}");
        }
        let set = async |client: &mut Client, revision: &str, policy: Value| {
            let reply = client.rpc("collaboration.set_model_policy", json!({"expectedRevision":revision,"policy":policy})).await;
            assert_ok(&reply);
            reply["result"]["revision"].as_str().unwrap().to_owned()
        };
        let fixed = set(&mut client, &initial, json!({"mode":"fixed","model":model("alpha")})).await;
        let mut other = Client::new_version(&server, false, 3).await;
        let stale = other.rpc("collaboration.set_model_policy", json!({"expectedRevision":initial,"policy":{"mode":"inherit"}})).await;
        assert_eq!(stale["error"]["data"]["appError"], "collaboration_model_policy_conflict", "{stale}");
        let reread = other.rpc("collaboration.model_policy", json!({})).await;
        assert_eq!(reread["result"], json!({"revision":fixed,"policy":{"mode":"fixed","model":model("alpha")}}));

        let created = client.rpc("agent.create", json!({"profile":{"name":"reviewer","description":"fixture",
            "systemPrompt":"review","tools":["agent"],"model":model("beta")}})).await;
        assert_ok(&created);
        let reviewer = created["result"]["agentId"].as_str().unwrap().to_owned();
        let temporary = json!({"name":"worker","description":"fixture","systemPrompt":"work","tools":["agent"],"model":model("beta")});
        let mut count = 2;
        let mut member_model = async |client: &mut Client, request: Value| {
            say(client, &lead, tool(request), "queue").await;
            count += 1;
            returned(client, &lead, &lead_session, count).await;
            last_ok(&events(&lead_session).await);
            configs.lock().unwrap().last().unwrap().model.clone()
        };
        for request in [
            json!({"action":"request","agentId":reviewer,"prompt":"hello"}),
            json!({"action":"request","profile":temporary,"prompt":"hello"}),
        ] {
            let selected = member_model(&mut client, request).await;
            assert_eq!(selected.model_id, "alpha");
            assert_eq!(selected.capability_snapshot.unwrap().target.model_id, "alpha");
        }
        let inherit = set(&mut client, &fixed, json!({"mode":"inherit"})).await;
        assert_ok(&client.rpc("session.model.update", json!({"sessionId":lead,"branchId":"main","providerId":PROVIDER,"modelId":"beta"})).await);
        let adopted = poll("adopted leader model", async || {
            lead_session.get::<PromptSnapshot>("main").unwrap()?.configuration.filter(|adopted| adopted.model_id == "beta")
        }).await;
        let selected = member_model(&mut client, json!({"action":"request","agentId":reviewer,"prompt":"hello"})).await;
        assert_eq!(lead_session.config().model.model_id, "alpha");
        assert_eq!((selected.provider_id.as_str(), selected.model_id.as_str()), (PROVIDER, "beta"));
        assert_eq!(selected.capability_snapshot, adopted.capability_snapshot);
        assert_eq!(selected.config, adopted.config);
        assert!(selected.metadata.as_ref().is_none_or(|metadata| metadata.session_id.is_none()
            && metadata.run_id.is_none() && metadata.trace_id.is_none() && metadata.turn_id.is_none()));
        let named = json!({"action":"request","profile":{"name":"worker","description":"fixture","systemPrompt":"work","tools":["agent"],"model":model("alpha")},"prompt":"hello"});
        assert_eq!(member_model(&mut client, named).await.model_id, "beta");
        let agent_mode = set(&mut client, &inherit, json!({"mode":"agent"})).await;
        let selected = member_model(&mut client, json!({"action":"request","agentId":reviewer,"prompt":"hello"})).await;
        assert_eq!(selected.model_id, "beta");
        set(&mut client, &agent_mode, json!({"mode":"fixed","model":model("missing")})).await;
        let agents = client.rpc("agent.list", json!({})).await["result"].as_array().unwrap().len();
        let members = configs.lock().unwrap().len();
        say(&mut client, &lead, tool(json!({"action":"request","profile":temporary,"prompt":"hello"})), "queue").await;
        idle(&mut client, &lead).await;
        assert!(last_error(&events(&lead_session).await).contains("member_create_failed"), "the failure detail is stored before collaboration/delivered so the waiting tool call observes it");
        assert_eq!(configs.lock().unwrap().len(), members);
        assert_eq!(client.rpc("agent.list", json!({})).await["result"].as_array().unwrap().len(), agents);
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
            .process_value(Arc::new(ModelHost(Arc::new(LargeReportModel))))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_app_server::collaboration::install(&mut builder);
        let captured_registry = Arc::new(OnceLock::new());
        builder.require_feature(Feature::new("collaboration-test.capture-tool-registry").session(
            "collaboration-test.capture-tool-registry",
            {
                let captured_registry = captured_registry.clone();
                move |names| {
                    Ok(Box::new(CaptureToolRegistry::new(names, captured_registry.clone())?))
                }
            },
        ));
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

/// The refusal an unaddressable Agent gets. An id no Agent has and an id of an
/// Agent the caller cannot see read the same, so the text says nothing about
/// hidden Agents.
fn agent_not_found() -> String {
    "agent_not_found: no visible Agent has this agentId; call list and use an exact \
     agents[].agentId, or give a profile to create a temporary Agent"
        .to_owned()
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

/// Releases an idle Session instance at once, as the idle unload pass does
/// after its grace period.
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
    assert!(
        !client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("collaboration.model_policy"))
    );
    assert_eq!(
        client.rpc("collaboration.model_policy", json!({})).await["error"]["code"],
        -32601
    );
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
    assert_no_collaboration_facts(&member_session).await;
    let mut names = vec![];
    while names.len() < 3 {
        let notice = client.notice("session.event").await;
        let entry = &notice["params"];
        if entry["sessionId"] == lead.as_str() && entry["event"]["type"] == "collaboration" {
            names.push(entry["event"]["name"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(names, ["request", "delivered", "result"]);
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId": lead}))
            .await,
    );
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
            .flatten()
            .any(|tool| tool == "agent")
    );
    let delivered = agent_results(&timeline);
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].starts_with(&format!(
        "<dim-agent-report agent=\"{default_agent}\" session=\"{member}\">\nStatus: completed.\nResult preview:\nEcho: hello\n"
    )));
    assert!(delivered[0].contains("Full result: agent tool, action \"result\", resultId \""));
    assert!(delivered[0].ends_with(".\n</dim-agent-report>"));
    assert!(timeline.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.delta.starts_with("Echo: <dim-agent-report"))));

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

    let sessions_before = client.rpc("session.list", json!({})).await["result"]
        .as_array()
        .unwrap()
        .len();
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
        tool(json!({"action": "request", "agentId": "default", "prompt": "x"})),
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
            "description": "", "systemPrompt": "", "tools": ["echo", "echo"]}}),
        ),
        "queue",
    )
    .await;
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
            "agent_not_found",
            "invalid_input",
            "invalid_input"
        ]
    );
    let not_found: Vec<String> = results(&timeline)
        .into_iter()
        .filter(|(error, text)| *error && text.starts_with("agent_not_found"))
        .map(|(_, text)| text)
        .collect();
    assert_eq!(not_found, [agent_not_found(), agent_not_found()]);
    assert_eq!(
        client.rpc("session.list", json!({})).await["result"]
            .as_array()
            .unwrap()
            .len(),
        sessions_before,
        "a refused request creates no member Session"
    );
    let unaddressable: Vec<String> = timeline
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::CollaborationRequest(data)
                if matches!(&data.target, dim_agent_types::CollaborationTarget::Agent { agent_id }
                    if agent_id == "default" || agent_id == "missing") =>
            {
                Some(event.id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(unaddressable.len(), 2);
    for op in &unaddressable {
        assert!(
            timeline.iter().any(|event| matches!(&event.kind,
                EventKind::CollaborationDelivered(data) if data.op_event_id == *op
                    && matches!(data.delivery, CollaborationDelivery::Error { .. }))),
            "{op} dispatched nothing and was delivered as its refusal"
        );
    }
    let (_, profile_refusal) = results(&timeline)
        .into_iter()
        .rfind(|(error, _)| *error)
        .unwrap();
    assert!(
        profile_refusal.contains("tool echo is listed twice"),
        "{profile_refusal}"
    );
    assert!(!timeline.iter().any(|event| matches!(&event.kind,
        EventKind::CollaborationRequest(data)
            if matches!(data.target, dim_agent_types::CollaborationTarget::Temporary { .. }))));
    assert_eq!(last_ok(&timeline)["status"], "cancelled");
    assert!(agent_results(&timeline)[2].contains("Status: cancelled."));

    let read_after = lead_session.sequence("main").unwrap();
    say(&mut client, &lead, tool(json!({"action": "list"})), "queue").await;
    idle(&mut client, &lead).await;
    let listed = last_ok(&events(&lead_session).await);
    assert_eq!(listed["role"], "leader");
    assert_eq!(
        listed["agents"],
        json!([{"agentId": default_agent, "name": "default", "description": ""}])
    );
    assert_ne!(
        listed["agents"][0]["agentId"], listed["agents"][0]["name"],
        "the Agent's name is not its id"
    );
    let participants = listed["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 2);
    assert_eq!(participants[0]["role"], "leader");
    assert_eq!(participants[0]["sessionId"], lead.as_str());
    assert_eq!(participants[1]["role"], "member");
    assert_eq!(participants[1]["sessionId"], member.as_str());
    assert_eq!(participants[1]["status"], "idle");

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
    for (error, text) in &member_results[member_results.len() - 2..] {
        assert!(*error && text.starts_with("invalid_input"), "{text}");
    }
    assert_eq!(facts(&member_session).await, before);

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
    assert_eq!(
        schema["properties"]["agentId"]["description"],
        json!(
            "An Agent's opaque id, copied exactly from list's agents[].agentId, or from \
             participants[].agentId for a team member. A name such as \"default\" is not an id."
        )
    );
    let leader_tools = events(&lead_session)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.tools.clone(),
            _ => None,
        })
        .unwrap();
    let guidance = &leader_tools
        .iter()
        .find(|t| t.name == "agent")
        .unwrap()
        .description;
    assert!(
        guidance.contains("agentId is copied exactly from list's agents[].agentId")
            && guidance.contains("is not an agentId"),
        "{guidance}"
    );
    let schema = serde_json::to_value(
        &leader_tools
            .iter()
            .find(|tool| tool.name == "agent")
            .unwrap()
            .input_schema,
    )
    .unwrap();
    for (action, required) in [
        ("request", json!(["action", "prompt", "agentId"])),
        ("cancel", json!(["action", "agentId", "sessionId"])),
        ("close", json!(["action", "agentId", "sessionId"])),
        (
            "message",
            json!(["action", "agentId", "sessionId", "content"]),
        ),
        ("read", json!(["action", "agentId", "sessionId"])),
        ("inspect", json!(["action", "agentId"])),
        ("result", json!(["action", "resultId"])),
        ("list", json!(["action"])),
    ] {
        let variant = schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variant| variant["properties"]["action"]["enum"][0] == action)
            .unwrap();
        assert_eq!(variant["required"], required, "{action}");
        for field in required.as_array().unwrap() {
            assert!(
                variant["properties"].get(field.as_str().unwrap()).is_some(),
                "{variant}"
            );
        }
    }
    assert!(
        schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|variant| variant["required"] == json!(["action", "prompt", "profile"]))
    );
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(!validator.is_valid(&json!({"action":"cancel","agentId":"exec-unknown"})));
    assert!(!validator.is_valid(&json!({"action":"list","agentId":"unexpected"})));
    assert!(!validator.is_valid(&json!({"action":"request","prompt":"work","agentId":default_agent,"profile":{"name":"worker","description":"","systemPrompt":"work","tools":[]}})));
    assert!(validator.is_valid(
        &json!({"action":"request","prompt":"work","agentId":default_agent,"sessionId":member})
    ));
    assert!(validator.is_valid(
        &json!({"action":"read","agentId":default_agent,"sessionId":member,"after":1,"limit":2})
    ));
    assert!(
        validator.is_valid(&json!({"action":"cancel","agentId":default_agent,"sessionId":member}))
    );
    assert!(validator.is_valid(&json!({"action":"request","prompt":"work","profile":{"name":"worker","description":"","systemPrompt":"work","tools":[]}})));
    let before = facts(&lead_session).await;
    say(
        &mut client,
        &lead,
        tool(json!({"action":"cancel","agentId":"exec-unknown"})),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let rejected = last_error(&events(&lead_session).await);
    assert!(
        rejected.contains("sessionId")
            && rejected.contains("exec_stop")
            && rejected.contains("no member action was performed"),
        "{rejected}"
    );
    assert_eq!(facts(&lead_session).await, before);

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
    let foreign_sessions = client.rpc("session.list", json!({})).await["result"]
        .as_array()
        .unwrap()
        .len();
    for agent_id in [temporary.clone(), "no-such-agent".to_owned()] {
        say(
            &mut client,
            &foreign,
            tool(json!({"action": "request", "agentId": agent_id, "prompt": "x"})),
            "queue",
        )
        .await;
        idle(&mut client, &foreign).await;
        assert_eq!(
            last_error(&events(&foreign_session).await),
            agent_not_found(),
            "an Agent this leader cannot see reads as one that does not exist"
        );
    }
    assert_eq!(
        client.rpc("session.list", json!({})).await["result"]
            .as_array()
            .unwrap()
            .len(),
        foreign_sessions,
        "a refused request creates no member Session"
    );
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
    let mut config = ServerConfig::default();
    config
        .session_defaults
        .tools
        .get_or_insert_default()
        .push("hold".into());
    let (server, agent) = launch_with_agent(Arc::new(GateTools(gate.clone())), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let lead = leader(
        &mut client,
        json!({"allowedTools": ["echo", "wait", "agent", "hold"]}),
    )
    .await;
    let lead_session = agent.get_session(&lead).unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":lead}))
            .await,
    );
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
    let registered = client
        .rpc("ui.attach", json!({"sessionId":lead,"kind":"tui"}))
        .await;
    assert_ok(&registered);
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":member}))
            .await,
    );
    let mut attached = std::collections::HashSet::new();
    while attached.len() != 2 {
        let notice = client.notice("session.state").await;
        let params = &notice["params"];
        if params["branchId"] == "main"
            && (params["sessionId"] == lead || params["sessionId"] == member)
        {
            attached.insert(params["sessionId"].as_str().unwrap().to_owned());
        }
    }
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":member,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&history);
    assert!(
        history["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["blocks"].as_array().is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block["text"].as_str().is_some_and(|text| {
                                text.contains("dim-agent-message") && text.contains("tool:hold {}")
                            })
                        })
                    })
            })
    );
    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId":member}))
            .await,
    );
    let interfaces = client.rpc("ui.status", json!({})).await;
    assert_ok(&interfaces);
    assert_eq!(
        interfaces["result"]["instances"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        interfaces["result"]["instances"][0]["uiId"],
        registered["result"]["uiId"]
    );
    assert_eq!(interfaces["result"]["instances"][0]["sessionId"], lead);
    running(&mut client, &member).await;
    let steer = "tool:hold {\"text\":\"steered\"}";
    let steering = message(steer, "steer");
    say(&mut client, &lead, steering.clone(), "queue").await;
    loop {
        let notice = client.notice("session.event").await;
        let params = &notice["params"];
        if params["sessionId"] == lead
            && params["branchId"] == "main"
            && params["event"]["type"] == "input"
            && params["event"]["name"] == "message"
            && params["event"]["data"]["user"]["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|block| block["text"] == steering))
        {
            break;
        }
    }
    idle(&mut client, &lead).await;
    assert_eq!(last_ok(&events(&lead_session).await)["mode"], "steer");
    gate.add_permits(1);
    poll("steered hold", async || {
        (holds(&member_session).await == 2).then_some(())
    })
    .await;
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
    for (text, after_results) in [(steer, 1), ("appended note", 2)] {
        let at = consumed(text);
        assert!(
            starts[0].0 < at && at < ends[0],
            "{text} joins the first turn"
        );
        assert_eq!(tool_results(starts[0].0..at), after_results, "{text}");
    }
    let follow = consumed("follow-up");
    assert!(ends[0] < starts[1].0 && starts[1].0 < follow);
    assert!(timeline.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.delta == "Echo: appended note")));

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
    assert!(
        paged
            .iter()
            .filter(|item| item["type"] == "tool_result")
            .all(|item| item["name"].is_string()),
        "a result names its call however far before the page it is"
    );
    assert_eq!(after, end);
    assert_eq!(&paged, full["events"].as_array().unwrap());

    assert_pairing(&events(&lead_session).await);
    assert_no_collaboration_facts(&member_session).await;
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_stop_reports_cancellation_and_a_later_steer_does_not_reopen_the_request() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    use std::os::unix::fs::PermissionsExt;

    let home = super::deletion::Store(
        std::path::PathBuf::from("/tmp").join(format!("dim-member-stop-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&home.0).unwrap();
    std::fs::set_permissions(&home.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let work = home.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: home.0.join("command-output"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    defaults::mock_model_hosts(&mut builder);
    dim_app_server::permission_rules::install(&mut builder);
    dim_app_server::collaboration::install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.0.clone()),
            database_path: Some(home.0.join("native.sqlite")),
            unix_path: Some(home.0.join("server.sock")),
            file_roots: vec![work.clone()],
            session_defaults: dim_agent_runtime::SessionConfig {
                cwd: work.to_string_lossy().into_owned(),
                tools: Some(vec!["exec".into(), "agent".into()]),
                permission_policy: dim_agent_types::PermissionPolicy {
                    preset: dim_agent_types::PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":work,"tools":["exec","agent"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created);
    let lead = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let leader = agent.get_session(&lead).unwrap();
    let holder = client.rpc("agent.list", json!({})).await;
    assert_ok(&holder);
    let holder = holder["result"][0]["agentId"].as_str().unwrap().to_owned();
    let command = format!(
        "tool:exec {}",
        json!({
        "command":"printf started > foreground-started; sleep 60",
        "runInBackground":true,"timeoutMs":-1})
    );
    say(
        &mut client,
        &lead,
        tool(json!({"action":"request","agentId":holder,"prompt":command})),
        "queue",
    )
    .await;
    let member = next_member(&leader, 0).await;
    let worker = agent.get_session(&member).unwrap();
    idle(&mut client, &lead).await;
    poll(
        "the real member foreground command has started",
        async || work.join("foreground-started").exists().then_some(()),
    )
    .await;
    let running = events(&worker).await;
    assert!(
        !running
            .iter()
            .any(|event| matches!(event.kind, EventKind::ExecStarted(_))),
        "the built-in member executor stays foreground even with runInBackground"
    );
    let requests = events(&leader).await;
    let operation = requests
        .iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::CollaborationRequest(_)))
        .unwrap()
        .id
        .clone();
    let leader_turns = requests
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .count();
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId":member,"branchId":"main",
        "reason":"stopped by the user"}),
            )
            .await,
    );
    returned(&mut client, &lead, &leader, 1).await;
    idle(&mut client, &member).await;
    let stopped = events(&worker).await;
    let reported = events(&leader).await;
    let cancelled = reported
        .iter()
        .find(|event| {
            matches!(&event.kind,
        EventKind::CollaborationResult(data) if data.request_event_id == operation
            && matches!(data.outcome, CollaborationOutcome::Cancelled { .. }))
        })
        .unwrap();
    let report_input = reported
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) if report(data) == Some(cancelled.id.as_str()) => {
                Some(data)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(report_input.mode, Some(dim_agent_types::InputMode::Steer));
    assert!(
        agent_results(&reported)
            .last()
            .unwrap()
            .contains("Status: cancelled.")
    );
    assert!(
        reported
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count()
            > leader_turns,
        "the unsuppressed cancellation report can start a leader turn after the stop batch"
    );
    assert!(
        !reported
            .iter()
            .any(|event| matches!(event.kind, EventKind::ExecStarted(_)))
    );
    let member_turns = stopped
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .count();
    let wakeup = format!(
        "tool:exec {}",
        json!({"command":"printf resumed > member-resumed","timeoutMs":1000})
    );
    say(
        &mut client,
        &lead,
        tool(json!({"action":"message","agentId":holder,
        "sessionId":member,"content":wakeup,"mode":"steer"})),
        "queue",
    )
    .await;
    poll(
        "a later Steer message starts real member work",
        async || work.join("member-resumed").exists().then_some(()),
    )
    .await;
    idle(&mut client, &member).await;
    idle(&mut client, &lead).await;
    let finished = events(&worker).await;
    let wake = finished
        .iter()
        .find(|event| {
            matches!(&event.kind,
        EventKind::InputMessage(data) if data.mode == Some(dim_agent_types::InputMode::Steer)
            && agent_message(data).is_some_and(|text| text.contains("member-resumed")))
        })
        .unwrap();
    assert!(finished.iter().any(|event| matches!(&event.kind,
        EventKind::InputConsumed(data) if data.input_id == wake.id)));
    assert!(
        finished
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count()
            > member_turns
    );
    let after = events(&leader).await;
    assert_eq!(
        after
            .iter()
            .filter(|event| matches!(event.kind, EventKind::CollaborationRequest(_)))
            .count(),
        1
    );
    assert_eq!(
        after
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::CollaborationResult(data) if data.request_event_id == operation))
            .count(),
        1
    );
    assert!(after.iter().any(|event| matches!(&event.kind,
        EventKind::CollaborationResult(data) if data.request_event_id == operation
            && matches!(data.outcome, CollaborationOutcome::Cancelled { .. }))));
    assert_eq!(
        result_of(&after, &member).len(),
        1,
        "a later turn does not reopen the cancelled delegation"
    );
    assert_pairing(&after);
    server.shutdown();
    server.join().await.unwrap();
}

/// Echoes like the mock host, except that a request whose last message asks
/// for `partial-call` or `many-calls` starts that many `echo` calls and
/// streams nothing more until it is cancelled, leaving calls that only
/// started.
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
        let calls = request
            .messages
            .last()
            .filter(|message| message.role == dim_agent_types::Role::User)
            .map(|message| serde_json::to_string(message).unwrap())
            .filter(|text| text.contains("<dim-agent-message "))
            .and_then(|text| {
                if text.contains("many-calls") {
                    Some(MANY_CALLS)
                } else if text.contains("partial-call") {
                    Some(UNFINISHED_CALLS)
                } else {
                    None
                }
            });
        let Some(calls) = calls else {
            return dim_agent_capabilities::host::ModelDriver::invoke(
                &dim_agent_capabilities::adapters::EchoModel,
                request,
                connection,
                cancel,
            );
        };
        Box::pin(async move {
            let mut started = vec![Ok(ModelChunk::MessageStart {
                message_id: "partial".into(),
            })];
            started.extend((0..calls).map(|at| {
                Ok(ModelChunk::ToolStart {
                    id: format!("unfinished-{at}"),
                    name: "echo".into(),
                })
            }));
            let started = futures_util::stream::iter(started);
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

/// The calls one request starts and leaves undispatched.
const UNFINISHED_CALLS: usize = 3;

/// More calls than one statement may name; the store reads them in chunks.
const MANY_CALLS: usize = 520;

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
        .process_value(Arc::new(ModelHost(Arc::new(PartialCallModel))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
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
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId": member, "branchId": "main", "reason": "stopped"}),
            )
            .await,
    );
    let through = poll("published facts", async || {
        let events = events(&member_session).await;
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::ToolResult(data) if data.call_event_id.is_none()))
            .then(|| events.last().unwrap().sequence)
    })
    .await;
    poll("member report", async || {
        (agent_results(&events(&lead_session).await).len() == 1).then_some(())
    })
    .await;
    idle(&mut client, &lead).await;
    let after = start.sequence + PARTIAL_USAGE - 1;
    let collaboration = server
        .service::<dim_app_server::collaboration::Collaboration>()
        .expect("the collaboration service");
    collaboration.take_reads();
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
        .filter(|item| item["type"] == "tool_result")
        .collect();
    assert_eq!(closed.len(), UNFINISHED_CALLS, "{read}");
    for (at, item) in closed.iter().enumerate() {
        assert_eq!(item["callId"], format!("unfinished-{at}"));
        assert_eq!(item["name"], "echo", "{item}");
        assert_eq!(item["status"], "error", "{item}");
    }
    assert_eq!(
        collaboration.take_reads(),
        2,
        "one page and one read of every start it names"
    );
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "read", "agentId": holder, "sessionId": member,
            "after": through, "limit": 200}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    assert_eq!(
        collaboration.take_reads(),
        1,
        "a page without results reads only itself"
    );
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": holder, "prompt": "many-calls"})),
        "queue",
    )
    .await;
    let many = next_member(&lead_session, 1).await;
    let many_session = agent.get_session(&many).unwrap();
    poll("every started call", async || {
        let events = events(&many_session).await;
        let started = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ToolUseStart(_)))
            .count();
        let usage = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ModelUsage(_)))
            .count() as u64;
        (started == MANY_CALLS && usage == PARTIAL_USAGE).then_some(())
    })
    .await;
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId": many, "branchId": "main", "reason": "stopped"}),
            )
            .await,
    );
    let closed_at = poll("published many-call result", async || {
        events(&many_session)
            .await
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(data) if data.call_event_id.is_none() => {
                    Some((event.sequence, data.results.len()))
                }
                _ => None,
            })
    })
    .await;
    assert_eq!(closed_at.1, MANY_CALLS);
    poll("two member reports", async || {
        (agent_results(&events(&lead_session).await).len() == 2).then_some(())
    })
    .await;
    idle(&mut client, &lead).await;
    collaboration.take_reads();
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "read", "agentId": holder, "sessionId": many,
            "after": closed_at.0 - 1, "limit": 200}),
        ),
        "queue",
    )
    .await;
    idle(&mut client, &lead).await;
    let page = last_ok(&events(&lead_session).await);
    let named: Vec<_> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "tool_result")
        .collect();
    assert_eq!(named.len(), MANY_CALLS, "{page}");
    for (at, item) in named.iter().enumerate() {
        assert_eq!(item["callId"], format!("unfinished-{at}"));
        assert_eq!(item["name"], "echo", "{item}");
    }
    assert_eq!(
        collaboration.take_reads(),
        2,
        "one page and one read of the starts"
    );
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
    gate: Arc<DispatchGate>,

    dispatcher: rutis::host::Key<dim_agent_runtime::Dispatcher>,
    timeline: rutis::host::Key<dim_agent_runtime::Timeline>,
    identity: rutis::host::Key<dim_agent_runtime::SessionIdentity>,
}
impl DispatchGatePlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, gate: Arc<DispatchGate>) -> rutis::Result<Self> {
        Ok(Self {
            gate,
            dispatcher: names.key()?,
            timeline: names.key()?,
            identity: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for DispatchGatePlugin {
    fn name(&self) -> &str {
        "collaboration-test.dispatch-gate"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.timeline.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
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
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// `launch_team` with a branch listener that can hold the leader's dispatch.
/// Its place among the branch's listeners is not fixed: the gate is a
/// required part and Collaboration's listener an optional one, mounted
/// concurrently, so Collaboration may receive an event before the gate holds
/// it. A hold delays every later event for every listener, because the
/// Dispatcher hands an event to all its listeners before the next.
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
    hold: Arc<MemberHold>,

    identity: rutis::host::Key<dim_agent_runtime::SessionIdentity>,
}
impl MemberHoldPlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, hold: Arc<MemberHold>) -> rutis::Result<Self> {
        Ok(Self {
            hold,
            identity: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for MemberHoldPlugin {
    fn name(&self) -> &str {
        "collaboration-test.member-hold"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
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
            Ok(rutis::host::Effect::Done)
        })
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
    builder.require_feature(
        Feature::new("collaboration-test.dispatch-gate")
            .branch("collaboration-test.dispatch-gate", move |names| {
                Ok(Box::new(DispatchGatePlugin::new(names, gate.clone())?))
            }),
    );
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
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// A turn stop committed before a request's `delivered` covers the request,
/// even when the delivery's append returns before the stop is handled: the
/// "delivered" mark follows the `delivered` event's position.
///
/// The order comes from the Timeline, not from the order of the branch's
/// listeners: the member's creation is held until the stop has committed, and
/// the gate holds an event of the test's own, committed after the request
/// and before the stop, until `delivered` has committed. Collaboration has
/// received the request by then and sees the stop only after `delivered`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_before_delivered_covers_the_request_however_late_it_is_handled() {
    let leader_timeline = Arc::new(OnceLock::new());
    let gate = DispatchGate::new({
        let leader_timeline = leader_timeline.clone();
        Arc::new(move |timeline, event| {
            let _ = leader_timeline.set(timeline.clone());
            Box::pin(async move {
                if event.kind.key() == "collaborationtest/barrier" {
                    hold_until(&timeline, "collaboration/delivered").await;
                }
            })
        })
    });
    let hold = Arc::new(MemberHold::default());
    let (server, agent) = launch_gated_with(gate.clone(), ServerConfig::default(), {
        let hold = hold.clone();
        move |builder| {
            builder.require_feature(
                Feature::new("collaboration-test.member-hold")
                    .session("collaboration-test.member-hold", move |names| {
                        Ok(Box::new(MemberHoldPlugin::new(names, hold.clone())?))
                    }),
            );
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
    leader_timeline
        .get()
        .expect("the gate saw the leader's request")
        .append(
            EventKind::Extension(
                dim_agent_types::ExtensionEvent::new("collaborationtest", "barrier", json!({}))
                    .unwrap(),
            )
            .into(),
        )
        .unwrap();
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
    hold.release.notify_one();
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
            builder.require_feature(
                Feature::new("collaboration-test.member-hold")
                    .session("collaboration-test.member-hold", move |names| {
                        Ok(Box::new(MemberHoldPlugin::new(names, hold.clone())?))
                    }),
            );
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
    launch_holding_team_unloading(ServerConfig::default().idle_unload_after, |_| {}).await
}
/// As [`launch_holding_team`], with `grace` as the idle unload grace period
/// and `extra` installed after the defaults.
async fn launch_holding_team_unloading(
    grace: Duration,
    extra: impl FnOnce(&mut AgentBuilder),
) -> (
    RunningServer,
    Agent,
    Arc<tokio::sync::Semaphore>,
    std::path::PathBuf,
) {
    let directory = std::env::temp_dir().join(format!("dim-team-store-{}", uuid::Uuid::new_v4()));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        idle_unload_after: grace,
        ..Default::default()
    };
    config
        .session_defaults
        .tools
        .get_or_insert_default()
        .push("hold".into());
    let (server, agent) = launch_with(Arc::new(GateTools(gate.clone())), config, extra).await;
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
    say(
        &mut client,
        &lead,
        tool(json!({"action": "cancel", "agentId": holder, "sessionId": cancelled})),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 1).await;
    assert!(held(&lead_session).await);
    gate.add_permits(1);
    returned(&mut client, &lead, &lead_session, 2).await;
    drop(lead_session);
    unload(&agent, &lead).await;
    unload(&agent, &cancelled).await;
    unload(&agent, &completed).await;
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

/// Waits until a Session created now has been released by the idle unload
/// pass: at least two passes have run since, each over every loaded Session.
async fn passes(client: &mut Client, agent: &Agent) {
    let id = leader(client, allowed()).await;
    unloaded(agent.get_session(&id).unwrap()).await;
}

/// Idle unload §13 items 4, 6 and 8, the Collaboration parts, with a grace
/// of 200 ms. An open request holds its leader with no lease past twice the
/// grace; when the member ends its work the result is recorded as
/// completed, the woken leader turn runs, and both Sessions unload. The
/// relation outlives the unload: the leader's next request to the member
/// restores both and reaches the same member. The leader's `read` of the
/// member, repeated while the Sessions unload, never fails or omits an event.
/// Nothing keeps a released instance of either alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_idle_unload_pass_keeps_a_leader_with_an_open_request() {
    let loads = super::sessions::idle_unload::Loads::default();
    let (server, agent, gate, directory) =
        launch_holding_team_unloading(Duration::from_millis(200), |builder| loads.install(builder))
            .await;
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
    say(
        &mut client,
        &lead,
        tool(json!({"action": "request", "agentId": holder, "prompt": "tool:hold {}"})),
        "queue",
    )
    .await;
    let member = next_member(&lead_session, 0).await;
    let member_session = agent.get_session(&member).unwrap();
    idle(&mut client, &lead).await;
    passes(&mut client, &agent).await;
    passes(&mut client, &agent).await;
    assert!(!lead_session.is_unloading());
    gate.add_permits(1);
    returned(&mut client, &lead, &lead_session, 1).await;
    assert_eq!(
        result_of(&events(&lead_session).await, &member),
        [CollaborationOutcome::Completed]
    );
    unloaded(lead_session).await;
    unloaded(member_session).await;

    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": holder, "sessionId": member,
            "prompt": "again"}),
        ),
        "queue",
    )
    .await;
    let lead_session = agent.get_session(&lead).unwrap();
    returned(&mut client, &lead, &lead_session, 2).await;
    assert_eq!(
        result_of(&events(&lead_session).await, &member),
        [
            CollaborationOutcome::Completed,
            CollaborationOutcome::Completed
        ]
    );
    assert_pairing(&events(&lead_session).await);
    drop(lead_session);

    let read = tool(json!({"action": "read", "agentId": holder, "sessionId": member}));
    let mut reads = vec![];
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let gone = agent.get_session(&member).is_none();
            say(&mut client, &lead, read.clone(), "queue").await;
            let lead_session = agent.get_session(&lead).unwrap();
            idle(&mut client, &lead).await;
            reads.push(last_ok(&events(&lead_session).await));
            if gone {
                break;
            }
        }
    })
    .await
    .expect("deadline: the member unloads");
    let first = &reads[0];
    assert!(
        first.to_string().contains("Echo: again"),
        "the read holds the second request: {first}"
    );
    for read in &reads {
        assert_eq!(read, first);
    }
    loads.released(&member).await;
    super::deletion::eventually("the leader unloads", || agent.get_session(&lead).is_none()).await;
    loads.released(&lead).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// A leader with two members on one temporary Agent, both unloaded. The
/// attached client holds the leader, so only the members unload. Returns
/// the leader, its Session, the temporary Agent and the two members.
async fn unloaded_temporary_members(
    client: &mut Client,
    agent: &Agent,
) -> (String, Session, String, String, String) {
    let lead = leader(client, allowed()).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": lead}))
            .await,
    );
    let lead_session = agent.get_session(&lead).unwrap();
    say(
        client,
        &lead,
        tool(
            json!({"action": "request", "prompt": "hello", "profile": {"name": "helper",
            "description": "temporary", "systemPrompt": "", "tools": []}}),
        ),
        "queue",
    )
    .await;
    returned(client, &lead, &lead_session, 1).await;
    let first = next_member(&lead_session, 0).await;
    let temporary = client.rpc("agent.list", json!({})).await["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["default"] == false)
        .unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    say(
        client,
        &lead,
        tool(json!({"action": "request", "agentId": temporary, "prompt": "hello"})),
        "queue",
    )
    .await;
    returned(client, &lead, &lead_session, 2).await;
    let second = next_member(&lead_session, 1).await;
    poll("both members unload", async || {
        let listed = client.rpc("agent.list", json!({})).await;
        listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent["agentId"] == temporary.as_str() && agent["sessions"] == 0)
            .then_some(())
    })
    .await;
    (lead, lead_session, temporary, first, second)
}

/// The Session ids of the leader's Team, as its `agent` tool lists them.
async fn team_of(client: &mut Client, lead: &str, lead_session: &Session) -> Vec<String> {
    say(client, lead, tool(json!({"action": "list"})), "queue").await;
    idle(client, lead).await;
    last_ok(&events(lead_session).await)["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|participant| participant["sessionId"].as_str().unwrap().to_owned())
        .collect()
}

/// Whether `agent.list` lists Agent `id`.
async fn listed(client: &mut Client, id: &str) -> bool {
    let listed = client.rpc("agent.list", json!({})).await;
    listed["result"]
        .as_array()
        .unwrap()
        .iter()
        .any(|agent| agent["agentId"] == id)
}

/// Idle unload §7: a temporary Agent closes with its last Session, loaded or
/// not. The leader creates two members on a temporary Agent, and both
/// unload. Closing one leaves the other unloaded, so the Agent stays and the
/// leader's next request restores that member. Closing it, now the last
/// Session, closes the Agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_a_member_keeps_a_temporary_agent_with_an_unloaded_member() {
    let (server, agent, _gate, directory) =
        launch_holding_team_unloading(Duration::from_millis(200), |_| {}).await;
    let mut client = Client::new_version(&server, true, 3).await;
    let (lead, lead_session, temporary, first, second) =
        unloaded_temporary_members(&mut client, &agent).await;

    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": second}))
            .await,
    );
    poll("the closed member leaves the Team", async || {
        (!team_of(&mut client, &lead, &lead_session)
            .await
            .contains(&second))
        .then_some(())
    })
    .await;
    assert!(
        listed(&mut client, &temporary).await,
        "the temporary Agent stays with its unloaded member"
    );
    say(
        &mut client,
        &lead,
        tool(
            json!({"action": "request", "agentId": temporary, "sessionId": first,
            "prompt": "again"}),
        ),
        "queue",
    )
    .await;
    returned(&mut client, &lead, &lead_session, 3).await;
    assert_eq!(
        result_of(&events(&lead_session).await, &first),
        [
            CollaborationOutcome::Completed,
            CollaborationOutcome::Completed
        ]
    );

    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": first}))
            .await,
    );
    poll("the temporary Agent closes", async || {
        (!listed(&mut client, &temporary).await).then_some(())
    })
    .await;
    drop(lead_session);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// S5c §3.3 and idle unload §7: deleting an unloaded Session ends it as a
/// business close does, without restoring it. The leader creates two
/// members on a temporary Agent, and both unload. Deleting one removes it
/// from the Team while the Agent stays for the other; deleting that last
/// Session closes the Agent. Neither delete restores its member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_unloaded_members_ends_their_relations_and_the_temporary_agent() {
    let loads = super::sessions::idle_unload::Loads::default();
    let (server, agent, _gate, directory) =
        launch_holding_team_unloading(Duration::from_millis(200), |builder| loads.install(builder))
            .await;
    let mut client = Client::new_version(&server, true, 3).await;
    let (lead, lead_session, temporary, first, second) =
        unloaded_temporary_members(&mut client, &agent).await;

    assert_ok(
        &client
            .rpc("session.delete", json!({"sessionId": second}))
            .await,
    );
    let team = poll("the deleted member leaves the Team", async || {
        let team = team_of(&mut client, &lead, &lead_session).await;
        (!team.contains(&second)).then_some(team)
    })
    .await;
    assert_eq!(team, [lead.clone(), first.clone()]);
    assert!(
        listed(&mut client, &temporary).await,
        "the temporary Agent stays with its unloaded member"
    );

    assert_ok(
        &client
            .rpc("session.delete", json!({"sessionId": first}))
            .await,
    );
    poll("the temporary Agent closes", async || {
        (!listed(&mut client, &temporary).await).then_some(())
    })
    .await;
    assert!(team_of(&mut client, &lead, &lead_session).await.is_empty());
    assert_eq!(loads.of(&first), (1, false));
    assert_eq!(loads.of(&second), (1, false));
    drop(lead_session);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// S5c §3.3 and idle unload §7 across a reload: S2's `sessions.delete` runs
/// to its own outcome across an Agents reload, which reloads Collaboration.
/// Two unloaded members of a temporary Agent are deleted while the store's
/// writer is held, and the writer is released during the reload, so both
/// deletes end with no Collaboration of their own generation listening.
/// Their relations still go, and the Agent, left without a Session, closes.
/// Neither member is restored.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unloaded_members_deleted_across_a_reload_end_with_their_temporary_agent() {
    let loads = super::sessions::idle_unload::Loads::default();
    let sessions = super::sessions::Sessions::default();
    let (server, agent, _gate, directory) =
        launch_holding_team_unloading(Duration::from_millis(200), |builder| {
            loads.install(builder);
            sessions.install(builder);
        })
        .await;
    let mut client = Client::new_version(&server, true, 3).await;
    let (lead, lead_session, temporary, first, second) =
        unloaded_temporary_members(&mut client, &agent).await;

    let lock = rusqlite::Connection::open(directory.join("dimcode.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let deleting = [&first, &second].map(|id| {
        let sessions = sessions.clone();
        let request = json!({"sessionId": id});
        tokio::spawn(async move { sessions.call("delete", request).await })
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(deleting.iter().all(|delete| !delete.is_finished()));
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        lock.execute_batch("COMMIT").unwrap();
    });
    restarted.unwrap();
    for delete in deleting {
        assert_eq!(delete.await.unwrap(), json!({"ok": true, "value": {}}));
    }
    poll("the temporary Agent closes", async || {
        (!listed(&mut client, &temporary).await).then_some(())
    })
    .await;
    assert!(team_of(&mut client, &lead, &lead_session).await.is_empty());
    assert_eq!(loads.of(&first), (1, false));
    assert_eq!(loads.of(&second), (1, false));
    drop(lead_session);
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
            builder.require_feature(
                Feature::new("collaboration-test.member-hold")
                    .session("collaboration-test.member-hold", move |names| {
                        Ok(Box::new(MemberHoldPlugin::new(names, hold.clone())?))
                    }),
            );
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
