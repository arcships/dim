use super::*;
use dim_agent_capabilities::host::{ModelChunk, ModelDriver, ModelStopReason, ModelStream};
use dim_agent_runtime::SessionConfig;
use std::sync::Arc;

struct CodeModeModel;

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

async fn launch_codemode() -> RunningServer {
    launch_codemode_with_defaults(SessionConfig::default().tools).await
}

async fn launch_codemode_with_defaults(tools: Option<Vec<String>>) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts_with_model(
        &mut builder,
        dim_agent_capabilities::history::HistoryRenderer::default(),
        Arc::new(CodeModeModel),
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
    let server = launch_codemode().await;
    let mut client = Client::new_version(&server, false, 4).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),"tools":["codemode","echo"]}),
        )
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
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
