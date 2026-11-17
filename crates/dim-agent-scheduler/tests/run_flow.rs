//! A manual run (`schedule.run_now`) through a real App Server: the window
//! launches a non-interactive Session, writes one input to its `main` branch,
//! and settles `delivered` with the authority the Session received. Nothing
//! follows the launched Session after the write (§3.2).
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_scheduler::install;
use dim_agent_types::*;
use dim_app_server::{ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Replies with queued tool batches, then with text; keeps every request.
#[derive(Default)]
struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
}

impl Script {
    fn texts(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect()
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
            self.requests.lock().unwrap().push(request);
            let id = format!("m{}", self.requests.lock().unwrap().len());
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
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
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

/// A selected model, as the App Server's profile needs before a Session runs.
/// The scripted model answers every request, so the endpoint is never called.
async fn select_model(client: &mut Client) {
    client
        .rpc(
            "provider.create",
            json!({"providerId": "018ec73f-0000-7000-8000-000000000007", "displayName": "Fixture",
                "connection": {"driverKind": "openai-compatible", "baseUrl": "http://localhost/v1",
                    "auth": {"type": "anonymous"}},
                "models": [{"modelId": "fixture", "displayName": "Fixture", "status": "active",
                    "capabilities": dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],
                "enabled": true}),
        )
        .await
        .unwrap();
    client
        .rpc(
            "provider.select",
            json!({"providerId": "018ec73f-0000-7000-8000-000000000007", "modelId": "fixture"}),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_run_launches_a_session_writes_one_input_and_settles_delivered() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();

    let model = Arc::new(Script::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "scheduler-run-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "nightly", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Summarize the day.",
                "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();

    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();
    assert_eq!(accepted["state"], "delivering", "{accepted}");

    let run = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id}))
                .await
                .unwrap();
            let found = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == run_id.as_str())
                .cloned();
            if let Some(run) = found
                && run["state"] != json!("delivering")
            {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the delivery settles");
    assert_eq!(run["state"], "delivered", "{run}");
    assert_eq!(run["detail"], "", "{run}");
    assert_eq!(run["session_origin"], "launch", "{run}");
    assert_eq!(run["branch_id"], "main", "{run}");
    assert!(run["session_id"].is_string(), "{run}");
    assert!(run["input_event_id"].is_string(), "{run}");
    assert!(run["deadline_at"].is_string(), "{run}");
    assert_eq!(run["policy"]["preset"], "read-only", "{run}");

    // The record is settled by the write; the launched Session consumes the
    // input on its own, so the model sees it only once that turn runs.
    client
        .rpc(
            "session.idle",
            json!({"sessionId": run["session_id"], "branchId": "main"}),
        )
        .await
        .unwrap();
    let requests = model.texts();
    assert!(
        requests.contains("scheduled-run"),
        "the marker reached the model"
    );
    assert!(
        requests.contains("Summarize the day."),
        "the prompt reached the model"
    );
    assert!(
        !requests.contains("scheduler-report:"),
        "no report input reaches any Session"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
