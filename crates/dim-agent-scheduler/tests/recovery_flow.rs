//! Restart recovery through a real App Server. Runs that were active when the
//! previous process stopped are settled from the stored Session state: a
//! launch that was never recorded is `lost`, and an accepted input resumes
//! tracking to its outcome. Nothing is launched or submitted again.
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
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Answers every request with text and counts the requests it received.
#[derive(Default)]
struct Script {
    requests: Mutex<usize>,
}

impl ModelDriver for Script {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let id = {
                let mut requests = self.requests.lock().unwrap();
                *requests += 1;
                format!("m{requests}")
            };
            let chunks = vec![
                Ok(ModelChunk::MessageStart {
                    message_id: id.clone(),
                }),
                Ok(ModelChunk::Text("done".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: id,
                    reason: ModelStopReason::EndTurn,
                }),
            ];
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

impl Script {
    fn count(&self) -> usize {
        *self.requests.lock().unwrap()
    }
}

async fn start(home: &Path, model: Arc<Script>) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
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
        client_name: "scheduler-recovery-test",
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

/// Polls a run until it leaves the active states, and returns its record.
async fn settled(client: &mut Client, job_id: &str, run_id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id}))
                .await
                .unwrap();
            let run = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == run_id)
                .cloned()
                .unwrap_or(Value::Null);
            if !run.is_null()
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
    .expect("the run settles")
}

/// Rewrites the run store as a crash would leave it: the edit receives the
/// inner run list and the file keeps its `{version, text}` envelope.
fn edit_runs(home: &Path, change: impl FnOnce(&mut Vec<Value>)) {
    let path = home.join("scheduler/runs.json");
    let envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    change(inner["runs"].as_array_mut().unwrap());
    let envelope = json!({"version": 1, "text": inner.to_string()});
    std::fs::write(&path, envelope.to_string()).unwrap();
}

fn find<'a>(runs: &'a mut [Value], run_id: &str) -> &'a mut Value {
    runs.iter_mut()
        .find(|run| run["run_id"] == run_id)
        .expect("the run is in the store")
}

/// Puts a run back in `launching` with the given Session fields kept or cleared.
fn relaunch(run: &mut Value, keep_session: bool) {
    run["state"] = json!("launching");
    run["detail"] = json!("");
    run.as_object_mut().unwrap().remove("ended_at");
    run.as_object_mut().unwrap().remove("input_event_id");
    if !keep_session {
        for field in ["session_id", "session_origin", "policy"] {
            run.as_object_mut().unwrap().remove(field);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_settles_active_runs_without_launching_again() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
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

    let mut run_ids = Vec::new();
    for _ in 0..2 {
        let accepted: Value = client
            .typed("schedule.run_now", json!({"job_id": job_id}))
            .await
            .unwrap();
        let run_id = accepted["run_id"].as_str().unwrap().to_owned();
        let run = settled(&mut client, &job_id, &run_id).await;
        assert_eq!(run["state"], "completed", "{run}");
        run_ids.push(run_id);
    }
    let accepted_input = {
        let history: Value = client
            .typed("schedule.history", json!({"job_id": job_id}))
            .await
            .unwrap();
        let run = history["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["run_id"] == run_ids[1].as_str())
            .unwrap()
            .clone();
        run["input_event_id"].as_str().unwrap().to_owned()
    };
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let requests_before = model.count();

    edit_runs(home.path(), |runs| {
        relaunch(find(runs, &run_ids[0]), false);
        relaunch(find(runs, &run_ids[1]), true);
    });

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;

    let unconfirmed = settled(&mut client, &job_id, &run_ids[0]).await;
    assert_eq!(unconfirmed["state"], "lost", "{unconfirmed}");
    assert_eq!(
        unconfirmed["detail"], "no_session_after_restart",
        "{unconfirmed}"
    );

    let resumed = settled(&mut client, &job_id, &run_ids[1]).await;
    assert_eq!(resumed["state"], "completed", "{resumed}");
    assert_eq!(
        resumed["input_event_id"],
        json!(accepted_input),
        "recovery adopts the accepted input and does not submit again: {resumed}"
    );
    assert_eq!(
        model.count(),
        0,
        "recovery does not launch or submit a Session turn; requests before the restart: {requests_before}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
