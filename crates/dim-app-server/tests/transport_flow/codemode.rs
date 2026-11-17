use super::*;
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    context_methods::ContextMethodsPlugin,
    context_model::window_facts,
    host::{
        CompactionStrategy, CompactionStrategyHost, ContextModel, ContextModelHost,
        IdentityContext, ModelChunk, ModelDriver, ModelHost, ModelStopReason, ModelStream,
        PrepareContextHost, ResolveContextModel,
    },
};
use dim_agent_runtime::SessionConfig;
use std::sync::Arc;

#[derive(Default)]
struct CodeModeModel(std::sync::Mutex<Option<Arc<dim_agent_types::ModelRequest>>>);

impl ModelDriver for CodeModeModel {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let results = request
            .messages
            .iter()
            .filter(|message| message.role == dim_agent_types::Role::ToolResult)
            .count();
        let answered = results > 0;
        if answered {
            *self.0.lock().unwrap() = Some(request.clone());
            assert_eq!(
                results, 1,
                "the child tool result leaked into the model context"
            );
        }
        let chunks = if answered {
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "done".into(),
                }),
                Ok(ModelChunk::Text("done".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "done".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ]
        } else {
            vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "codemode".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "c1".into(),
                    name: "codemode".into(),
                }),
                Ok(ModelChunk::ToolUse(dim_agent_types::ToolCall {
                    id: "c1".into(),
                    name: "codemode".into(),
                    input: serde_json::from_value(json!({
                        "code": r#"const value = await tools.call("echo", {text: "live"}); return value;"#
                    }))
                    .expect("codemode input"),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "codemode".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ]
        };
        Box::pin(async move { Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream) })
    }
}

struct UsageResolver;
impl ResolveContextModel for UsageResolver {
    fn resolve(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move { Ok(window_facts(1_000_000, 64., &request)) })
    }
}

impl CompactionStrategy for UsageResolver {
    fn name(&self) -> &str {
        "fixture"
    }
    fn replacement_size(&self, _: &dyn ContextModel) -> f64 {
        0.
    }
    fn compact(
        &self,
        _: dim_agent_capabilities::host::CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::StrategyOutcome> {
        Box::pin(async {
            Err(dim_agent_capabilities::host::HostError::failed(
                "the CodeMode fixture fits without compaction",
            ))
        })
    }
}

async fn launch_codemode_with_defaults(tools: Option<Vec<String>>) -> RunningServer {
    launch_codemode_with_model(tools, Arc::new(CodeModeModel::default())).await
}

async fn launch_codemode_with_model(
    tools: Option<Vec<String>>,
    model: Arc<CodeModeModel>,
) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(UsageResolver)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(
            UsageResolver,
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
    dim_app_server::collaboration::install(&mut builder);
    dim_codemode::install(&mut builder);
    serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-server-test-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            session_defaults: SessionConfig {
                tools,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_product_style_app_server_runs_codemode_and_writes_child_facts() {
    let model = Arc::new(CodeModeModel::default());
    let server = launch_codemode_with_model(SessionConfig::default().tools, model.clone()).await;
    let mut client = Client::new_version(&server, false, 4).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),"tools":["codemode","echo"]}),
        )
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let loaded = server.agent().unwrap().get_session(&session).unwrap();
    loaded
        .service::<dim_agent_capabilities::prompt::Prompt>("main")
        .unwrap()
        .configure_loaded(
            Some(dim_agent_types::ModelSelection {
                provider_id: "mock".into(),
                model_id: "echo".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
            vec![],
        )
        .await
        .unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":session}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main","text":"run"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":"main"}),
            )
            .await,
    );
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&history);
    let events = history["result"]["events"].as_array().unwrap();
    let outer = events
        .iter()
        .find(|event| {
            event["type"] == "step"
                && event["name"] == "tool-result"
                && event["data"]["results"][0]["call_id"] == "c1"
        })
        .expect("the outer codemode result");
    assert_eq!(
        outer["data"]["results"][0]["result"]["content"],
        "{\"content\":\"live\"}"
    );
    let child = events
        .iter()
        .find(|event| {
            event["type"] == "step"
                && event["name"] == "tool-call"
                && event["parent_call_id"] == "c1"
        })
        .expect("the child echo batch");
    assert_eq!(child["data"]["calls"][0]["name"], "echo");
    let mut request = model.0.lock().unwrap().as_ref().unwrap().as_ref().clone();
    let usage = client
        .rpc(
            "session.context.usage",
            json!({"sessionId":session,"branchId":"main"}),
        )
        .await;
    assert_ok(&usage);
    let context = dim_agent_capabilities::history::HistoryRenderer::default()
        .context_in(
            &loaded,
            "main",
            usage["result"]["through"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    request.messages = dim_agent_capabilities::history::context_messages(
        &context,
        &dim_agent_capabilities::history::project_context_history(&context).unwrap(),
    );
    let expected = window_facts(1_000_000, 64., &request).count_tokens(&request);
    assert_eq!(
        usage["result"]["estimatedInputTokens"].as_f64().unwrap(),
        expected,
        "numeric usage matches the actual model request, excluding child execution facts"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v3_default_all_tools_session_does_not_mount_codemode() {
    let server = launch_codemode_with_defaults(None).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", json!({"cwd":std::env::temp_dir()}))
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    assert!(
        session
            .config()
            .excluded_tools
            .iter()
            .any(|name| name == "codemode"),
        "a pre-v4 all-tools Session must exclude codemode"
    );
    server.shutdown();
    server.join().await.unwrap();
}
