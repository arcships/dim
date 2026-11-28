//! #2152: a line cut at 0 adopts the Session's initial configuration.
use super::*;
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel},
    context_methods::ContextMethodsPlugin,
    context_model::window_facts,
    history::HistoryRenderer,
    host::{
        CompactionInput, CompactionStrategy, CompactionStrategyHost, ContextModel,
        ContextModelHost, IdentityContext, LoadPrompt, ModelHost, PrepareContextHost,
        ResolveContextModel, StrategyDiagnostics, StrategyOutcome,
    },
};
use dim_agent_runtime::{ConfigureUpdate, PromptSourceTarget};
use dim_agent_types::{
    CompactionStrategyRecord, JsonObject, ModelSelection, PromptChange, PromptDelivery,
    PromptEntry, PromptScope,
};

struct Window;
impl ResolveContextModel for Window {
    fn resolve(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move { Ok(window_facts(16_000, 200., &request)) })
    }
}

/// A strategy that always answers `skipped`: the test needs the step to run,
/// not a replacement.
struct KeepsHistory;
impl CompactionStrategy for KeepsHistory {
    fn name(&self) -> &str {
        "keeps-history"
    }
    fn replacement_size(&self, _: &dyn ContextModel, _: f64) -> f64 {
        0.
    }
    fn compact(&self, _: CompactionInput, _: CancellationToken) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async {
            Ok(StrategyOutcome::Skipped {
                reason: "the fixture keeps every entry".into(),
                diagnostics: StrategyDiagnostics {
                    model_call_count: 0,
                    model_usage: None,
                    record: CompactionStrategyRecord {
                        kind: "keeps-history".into(),
                        details: JsonObject::new(),
                    },
                },
            })
        })
    }
}

/// The Session's prompt source, delivered as System text.
struct InitialSource;
impl LoadPrompt for InitialSource {
    fn load(
        &self,
        target: PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            Ok(Some(PromptEntry {
                source: target.source,
                version: "1".into(),
                body: "initial rules".into(),
                scope: target.scope,
                delivery: PromptDelivery::System,
                order: 0,
            }))
        })
    }
}

/// What a product Session adopts when a line opens: `SessionConfig.model`
/// with every prompt source registered then.
fn initial_configuration() -> ConfigureUpdate {
    ConfigureUpdate {
        sources: vec![PromptSourceTarget {
            source: "rules".into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
        }],
        model: Some(ModelSelection {
            provider_id: "mock".into(),
            model_id: "initial".into(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        }),
    }
}

fn context_builder(initial: Option<ConfigureUpdate>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_with_prompt(&mut builder, Arc::new(InitialSource), initial);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Window)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(
            KeepsHistory,
        )))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        )
        .require_feature(
            Feature::new("ContextMethods").session("ContextMethods", |names| {
                Ok(Box::new(ContextMethodsPlugin::new(names)?))
            }),
        );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder
}

async fn launch_context(initial: Option<ConfigureUpdate>) -> RunningServer {
    let mut builder = context_builder(initial);
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-server-test-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            ..ServerConfig::default()
        },
    )
    .await
    .unwrap()
}

async fn create_session(client: &mut Client) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": std::env::temp_dir(),
                "tools": ["branch"],
                "permissionPolicy": {"allowedTools": ["branch"]},
                "model": {"provider": "mock", "modelId": "initial"}
            }),
        )
        .await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

async fn configure_main(session: &dim_agent_runtime::Session, model: &str) {
    session
        .configure(
            ConfigureUpdate {
                model: Some(ModelSelection {
                    provider_id: "mock".into(),
                    model_id: model.into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                ..Default::default()
            },
            "main",
        )
        .await
        .unwrap();
}

async fn settle(client: &mut Client, session: &str, branch: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":branch,"text":"one turn"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":branch}),
            )
            .await,
    );
}

async fn open_line(client: &mut Client, session: &str) -> String {
    let opened = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":"main","at":0,"prompt":"work on the line"}),
        )
        .await;
    assert_ok(&opened);
    opened["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn model_of(client: &mut Client, session: &str, branch: &str) -> Value {
    let reply = client
        .rpc(
            "session.model.get",
            json!({"sessionId":session,"branchId":branch}),
        )
        .await;
    assert_ok(&reply);
    reply["result"]["configuration"].clone()
}

/// The System entries the line adopted into its own state.
fn adopted_system(history: &[Arc<dim_agent_types::AgentEvent>]) -> Vec<&PromptEntry> {
    history
        .iter()
        .flat_map(|event| match &event.kind {
            EventKind::PromptChanged(data) => data.changes.as_slice(),
            _ => &[],
        })
        .filter_map(|change| match change {
            PromptChange::Set { entry } if entry.delivery == PromptDelivery::System => Some(entry),
            _ => None,
        })
        .collect()
}

/// A line cut at 0 sits before its parent's adoption, so it adopts the
/// Session's initial model and System even when the parent changed afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_cut_at_zero_answers_context_usage_and_compacts() {
    let server = launch_context(Some(initial_configuration())).await;
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = create_session(&mut client).await;
    let session = agent.get_session(&sid).unwrap();
    // Order one: the parent changes its model before the line is cut.
    configure_main(&session, "parent").await;
    settle(&mut client, &sid, "main").await;
    let line = open_line(&mut client, &sid).await;
    // Order two: the parent changes again after the line opened.
    configure_main(&session, "later").await;
    settle(&mut client, &sid, &line).await;
    let model = model_of(&mut client, &sid, &line).await;
    assert_eq!(model["model"]["modelId"], "initial", "{model}");
    let history = chain_events(&session, &line, ..).await.unwrap();
    let systems = adopted_system(&history);
    assert!(
        systems
            .iter()
            .any(|entry| entry.source == "rules" && entry.body == "initial rules"),
        "the line adopts the Session's System source on its own Timeline: {systems:?}"
    );
    let usage = client
        .rpc(
            "session.context.usage",
            json!({"sessionId":sid,"branchId":line}),
        )
        .await;
    assert_ok(&usage);
    assert_eq!(usage["result"]["contextWindow"], 16_000.0, "{usage}");
    let compact = client
        .rpc(
            "session.context.compact",
            json!({"sessionId":sid,"branchId":line}),
        )
        .await;
    assert_ok(&compact);
    assert_eq!(compact["result"]["status"], "skipped", "{compact}");
}

/// A line stored without a configuration has one after its next opening:
/// `branch/resume` delivers the Session's initial model and System.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_line_without_configuration_adopts_it_on_resume() {
    let directory = std::env::temp_dir().join(format!("dim-at-zero-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    let server = serve(context_builder(None), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let sid = create_session(&mut client).await;
    let line = open_line(&mut client, &sid).await;
    settle(&mut client, &sid, &line).await;
    let before = model_of(&mut client, &sid, &line).await;
    assert!(
        before.is_null(),
        "the stored line has no configuration: {before}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = serve(context_builder(Some(initial_configuration())), config)
        .await
        .unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    let session = agent.get_session(&sid).unwrap();
    settle(&mut client, &sid, &line).await;
    let model = model_of(&mut client, &sid, &line).await;
    assert_eq!(model["model"]["modelId"], "initial", "{model}");
    let history = chain_events(&session, &line, ..).await.unwrap();
    let systems = adopted_system(&history);
    assert!(
        systems
            .iter()
            .any(|entry| entry.source == "rules" && entry.body == "initial rules"),
        "resuming the line writes the Session's System source: {systems:?}"
    );
    let usage = client
        .rpc(
            "session.context.usage",
            json!({"sessionId":sid,"branchId":line}),
        )
        .await;
    assert_ok(&usage);
    let compact = client
        .rpc(
            "session.context.compact",
            json!({"sessionId":sid,"branchId":line}),
        )
        .await;
    assert_ok(&compact);
    assert_eq!(compact["result"]["status"], "skipped", "{compact}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}
