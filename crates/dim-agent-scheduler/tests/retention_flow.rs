//! Retention through a real App Server: the run store keeps the newest 100
//! terminal runs per job, and an expired record leaves the file once a later
//! run settles and triggers a pass.
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_scheduler::install;
use dim_agent_types::*;
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

/// Answers every request with a text reply.
struct Reply;

impl ModelDriver for Reply {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let chunks = vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "m1".into(),
                }),
                Ok(ModelChunk::Text("done".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "m1".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ];
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

async fn start(home: &Path) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), Arc::new(Reply));
    install(&mut builder);
    serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.join("dimcode.sqlite")),
            product_home: Some(home.to_owned()),
            unix_path: Some(home.join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn connect(server: &RunningServer) -> Client {
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "scheduler-retention-test",
        protocol_version: 4,
    };
    Client::connect(&config).await.unwrap()
}

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

/// The stored run list, read from the file itself so the count is not limited
/// by a history page.
fn stored_runs(home: &Path) -> Vec<Value> {
    let envelope: Value =
        serde_json::from_slice(&std::fs::read(home.join("scheduler/runs.json")).unwrap()).unwrap();
    let inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    inner["runs"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_store_keeps_the_newest_hundred_terminal_runs_per_job() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();

    let server = start(home.path()).await;
    let mut client = connect(&server).await;
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

    let seeded: Vec<Value> = (0..101)
        .map(|index| {
            let run_id = format!("seed-{index:03}");
            json!({
                "run_id": run_id,
                "job_id": job_id,
                "window": "manual",
                "state": "missed",
                "submission_id": format!("scheduler:{run_id}"),
                "created_at": format!("2020-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60),
                "ended_at": format!("2020-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60),
                "detail": "missed",
            })
        })
        .collect();
    let inner = json!({"version": 1, "runs": seeded}).to_string();
    std::fs::write(
        home.path().join("scheduler/runs.json"),
        json!({"version": 1, "text": inner}).to_string(),
    )
    .unwrap();

    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let runs = stored_runs(home.path());
        let settled = runs
            .iter()
            .any(|run| run["run_id"] == run_id.as_str() && run["state"] == "delivered");
        if settled && runs.len() == 100 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "retention settles to 100 records: {} records",
            runs.len()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let runs = stored_runs(home.path());
    assert!(
        runs.iter()
            .all(|run| run["run_id"] != "seed-000" && run["run_id"] != "seed-001"),
        "the two oldest seeded records expired: 102 terminal runs keep the newest 100"
    );
    assert!(
        runs.iter().any(|run| run["run_id"] == "seed-002"),
        "the newest seeded records are kept"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
