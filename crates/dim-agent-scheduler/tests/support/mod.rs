//! A real App Server with the Scheduler installed and a scripted model.
#![allow(dead_code)]
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
use dim_agent_types::*;
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Replies with queued tool batches, then with text; keeps every request. A
/// request whose last user text contains `hold` waits until `release`.
#[derive(Default)]
pub struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    held: Mutex<Option<String>>,
    release: tokio::sync::Notify,
}

impl Script {
    pub fn push(&self, calls: Vec<ToolCall>) {
        self.batches.lock().unwrap().push_back(calls);
    }
    pub fn texts(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect()
    }
    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    /// Holds every request whose rendered text contains `marker`.
    pub fn hold(&self, marker: &str) {
        *self.held.lock().unwrap() = Some(marker.to_owned());
    }
    pub fn release(&self) {
        *self.held.lock().unwrap() = None;
        self.release.notify_waiters();
    }
}

impl ModelDriver for Script {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let rendered = format!("{request:?}");
            self.requests.lock().unwrap().push(request);
            loop {
                let released = self.release.notified();
                let held = self
                    .held
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|marker| rendered.contains(marker.as_str()));
                if !held {
                    break;
                }
                tokio::select! {
                    _ = released => {}
                    _ = cancel.cancelled() => break,
                }
            }
            let id = format!("m{}", self.count());
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

pub fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    home
}

pub async fn start(home: &Path, model: Arc<Script>) -> RunningServer {
    start_with(home, model, SessionConfig::default()).await
}

/// The hooks of each started server, by home, newest last.
static HOLDS: Mutex<Vec<(std::path::PathBuf, Arc<dim_agent_scheduler::testing::Holds>)>> =
    Mutex::new(Vec::new());

/// The hooks of the server started last for `home`.
pub fn holds(home: &Path) -> Arc<dim_agent_scheduler::testing::Holds> {
    HOLDS
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(path, _)| path == home)
        .map(|(_, holds)| holds.clone())
        .expect("a server started for this home")
}

/// [`start`] with the host's Session defaults, so a flow can set the role or
/// the authority bound a Session is created under.
pub async fn start_with(home: &Path, model: Arc<Script>, defaults: SessionConfig) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    let holds = Arc::new(dim_agent_scheduler::testing::Holds::default());
    HOLDS.lock().unwrap().push((home.to_owned(), holds.clone()));
    dim_agent_scheduler::install_with_testing(
        &mut builder,
        dim_agent_scheduler::SchedulerTexts::default(),
        holds,
    )
    .expect("the built-in texts are valid");
    serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.join("dimcode.sqlite")),
            product_home: Some(home.to_owned()),
            session_defaults: defaults,
            unix_path: Some(home.join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// A connected client with the fixture model selected; a restarted server
/// keeps the selection. The scripted model answers every request, so the
/// endpoint is never called.
pub async fn connect(server: &RunningServer) -> Client {
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "scheduler-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    let created = client
        .rpc(
            "provider.create",
            json!({"providerId": "018ec73f-0000-7000-8000-000000000007", "displayName": "Fixture",
                "connection": {"driverKind": "openai-compatible", "baseUrl": "http://localhost/v1",
                    "auth": {"type": "anonymous"}},
                "models": [{"modelId": "fixture", "displayName": "Fixture", "status": "active",
                    "capabilities": dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],
                "enabled": true}),
        )
        .await;
    if created.is_ok() {
        client
            .rpc(
                "provider.select",
                json!({"providerId": "018ec73f-0000-7000-8000-000000000007", "modelId": "fixture"}),
            )
            .await
            .unwrap();
    }
    client
}

pub fn call(id: &str, input: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "schedule".into(),
        input: input.as_object().unwrap().clone(),
    }
}

pub async fn wait_for(description: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

/// An interactive leader Session that may call `schedule`.
pub async fn create_session(client: &mut Client, home: &Path, interactive: bool) -> String {
    create_session_with(client, home, &["schedule"], interactive).await
}

/// A Session that may call `schedule` and the named tools.
pub async fn create_session_with(
    client: &mut Client,
    home: &Path,
    tools: &[&str],
    interactive: bool,
) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": tools,
                "interactive": interactive,
                "permissionPolicy": {"preset": "full-access", "allowedTools": [], "disallowedTools": []}
            }),
        )
        .await
        .unwrap();
    created["sessionId"].as_str().unwrap().to_owned()
}

/// The job's run with `run_id`, once `done` accepts it.
pub async fn run_where(
    client: &mut Client,
    job_id: &str,
    run_id: &str,
    done: impl Fn(&Value) -> bool,
) -> Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id}))
                .await
                .unwrap();
            if let Some(run) = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == run_id)
                && done(run)
            {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the run reaches the expected state")
}

/// The job's run with `run_id`, once it is terminal.
pub async fn settled(client: &mut Client, job_id: &str, run_id: &str) -> Value {
    run_where(client, job_id, run_id, |run| {
        run["state"] != json!("delivering")
    })
    .await
}

pub fn stored_runs(home: &Path) -> Vec<Value> {
    let envelope: Value =
        serde_json::from_slice(&std::fs::read(home.join("scheduler/runs.json")).unwrap()).unwrap();
    let inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    inner["runs"].as_array().unwrap().clone()
}

/// Rewrites the run store as a crash or another process would leave it; a
/// missing store starts empty.
pub fn edit_runs(home: &Path, change: impl FnOnce(&mut Vec<Value>)) {
    let path = home.join("scheduler/runs.json");
    let mut inner: Value = match std::fs::read(&path) {
        Ok(bytes) => {
            let envelope: Value = serde_json::from_slice(&bytes).unwrap();
            serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap()
        }
        Err(_) => json!({"version": 1, "runs": []}),
    };
    change(inner["runs"].as_array_mut().unwrap());
    let text = serde_json::to_string(&inner).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version": 1, "text": text})).unwrap(),
    )
    .unwrap();
}
