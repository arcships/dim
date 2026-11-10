//! The timer through a real App Server: an `at` job runs once when its
//! instant arrives, and then the job is disabled.
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

impl Script {}

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
async fn an_at_job_runs_once_when_its_instant_arrives_and_then_disables() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    std::fs::write(
        home.path().join("schedule.json"),
        json!({"jobs": [], "max_concurrent_runs": 17}).to_string(),
    )
    .unwrap();

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
        client_name: "scheduler-timer-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;

    let at = (chrono::Utc::now() + chrono::Duration::seconds(3))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "once", "schedule": {"at": at},
                "prompt": "Summarize the day.",
                "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();

    let run = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id}))
                .await
                .unwrap();
            if let Some(run) = history["runs"].as_array().unwrap().first().cloned()
                && !matches!(
                    run["state"].as_str(),
                    Some("launching" | "submitted" | "running")
                )
            {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the window's run settles");
    assert_eq!(run["window"], at.as_str(), "{run}");
    assert_eq!(run["state"], "completed", "{run}");

    let history: Value = client
        .typed("schedule.history", json!({"job_id": job_id}))
        .await
        .unwrap();
    assert_eq!(
        history["runs"].as_array().unwrap().len(),
        1,
        "one window, one run: {history}"
    );

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
            if listed["jobs"][0]["enabled"] == json!(false) {
                return listed;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the at job is disabled after its instant");

    let body: String = rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .query_row(
            "SELECT document FROM settings_documents WHERE source = 'user/schedule'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let document: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(document["max_concurrent_runs"], 17, "{document}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
