//! Loops through a real App Server (design §12): a start delivers round 1 at
//! once with the loop marker; a self-paced `next` clamps its delay and delivers
//! round 2; a round without `next` leaves the loop `waiting` and delivers
//! nothing more; `stop` is idempotent; expiry settles `expired`; a gone branch
//! settles `stopped`; a restart with a delivered round settles by lookup and
//! never writes a second one. A row that waits for a window names one an hour
//! out and crosses it with the clock; one that waits for a pass crosses a tick
//! (#1536).
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
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
/// request whose rendered text contains `hold` waits until `release`, the way
/// `support::Script` does (support/mod.rs).
#[derive(Default)]
struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    held: Mutex<Option<String>>,
    release: tokio::sync::Notify,
}

impl Script {
    fn push(&self, calls: Vec<ToolCall>) {
        self.batches.lock().unwrap().push_back(calls);
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    /// Holds every request whose rendered text contains `marker`.
    fn hold(&self, marker: &str) {
        *self.held.lock().unwrap() = Some(marker.to_owned());
    }
    fn release(&self) {
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

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    home
}

async fn start(home: &Path, model: Arc<Script>) -> RunningServer {
    start_armed(home, model, |_| {}).await
}

/// [`start`] with the server's hooks armed before it starts, so a startup pass
/// cannot run before the test's seam is in force.
async fn start_armed(
    home: &Path,
    model: Arc<Script>,
    arm: impl FnOnce(&dim_agent_scheduler::testing::Holds),
) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    let holds = Arc::new(dim_agent_scheduler::testing::Holds::default());
    arm(&holds);
    dim_agent_scheduler::install_with_testing(
        &mut builder,
        dim_agent_scheduler::SchedulerTexts::default(),
        dim_agent_scheduler::testing::TestingOptions {
            holds,
            clock: dim_agent_scheduler::testing::clock(home),
            signals: dim_agent_scheduler::testing::signals(home),
        },
    )
    .expect("the built-in texts are valid");
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

/// A connected client with the fixture model selected; a restarted server
/// keeps the selection.
async fn connect(server: &RunningServer) -> Client {
    let config = ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "loop-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    let created = client
        .rpc(
            "provider.create",
            json!({"providerId": "018ec73f-0000-7000-8000-000000000008", "displayName": "Fixture",
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
                json!({"providerId": "018ec73f-0000-7000-8000-000000000008", "modelId": "fixture"}),
            )
            .await
            .unwrap();
    }
    client
}

/// An interactive leader Session (unless `interactive` is false) mounting
/// exactly `tools`, in the project at `cwd`.
async fn create_session(
    client: &mut Client,
    cwd: &Path,
    tools: &[&str],
    interactive: bool,
) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": cwd,
                "tools": tools,
                "interactive": interactive,
                "permissionPolicy": {"preset": "full-access", "allowedTools": [], "disallowedTools": []}
            }),
        )
        .await
        .unwrap();
    created["sessionId"].as_str().unwrap().to_owned()
}

async fn history(client: &mut Client, session_id: &str) -> Vec<Value> {
    let page: Value = client
        .rpc(
            "session.history",
            json!({"sessionId": session_id, "branchId": "main", "after": 0, "limit": 256}),
        )
        .await
        .unwrap();
    page["events"].as_array().cloned().unwrap_or_default()
}

/// The loop rounds the branch has recorded, in order.
fn loop_rounds(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| {
            event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["blocks"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("dim-scheduler kind=\"loop\""))
                    })
                })
        })
        .cloned()
        .collect()
}

/// The `<dim-scheduler kind="loop" ...>` marker of one round's input.
fn marker_of(event: &Value) -> String {
    event["data"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|block| block["text"].as_str())
        .expect("an input carries a text block")
        .to_owned()
}

/// The stored loop records, as the file holds them.
fn stored_loops(home: &Path) -> Vec<Value> {
    let path = home.join("scheduler/loops.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    inner["loops"].as_array().cloned().unwrap_or_default()
}

/// Rewrites the loop store as a crash or another process would leave it.
fn edit_loops(home: &Path, change: impl FnOnce(&mut Vec<Value>)) {
    let path = home.join("scheduler/loops.json");
    let mut inner: Value = match std::fs::read(&path) {
        Ok(bytes) => {
            let envelope: Value = serde_json::from_slice(&bytes).unwrap();
            serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap()
        }
        Err(_) => json!({"version": 1, "loops": []}),
    };
    change(inner["loops"].as_array_mut().unwrap());
    let text = serde_json::to_string(&inner).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version": 1, "text": text})).unwrap(),
    )
    .unwrap();
}

async fn wait_for(description: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}
/// A loop's round 1 is delivered at once, through the delivery path, with the
/// marker that names the loop and round 1. It is not a plain user message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_delivers_round_one_with_the_loop_marker() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    let created: Value = client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "branchId": "main", "prompt": "check the deploy"}),
        )
        .await
        .unwrap();
    let loop_id = created["loop"]["loopId"].as_str().unwrap().to_owned();
    assert_eq!(created["loop"]["state"], "running");

    wait_for("round 1 to settle", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["round"] == json!(2))
    })
    .await;

    let rounds = loop_rounds(&history(&mut client, &session).await);
    assert_eq!(rounds.len(), 1, "round 1 is delivered exactly once");
    let marker = marker_of(&rounds[0]);
    assert!(
        marker.contains(&format!("loop=\"{loop_id}\"")) && marker.contains("round=\"1\""),
        "the marker names the loop and its round: {marker}"
    );
}

/// A delivered self-paced round names no following one: the loop waits. `list`
/// shows it, nothing more is delivered, and `stop` settles it idempotently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_round_without_next_waits_and_stop_is_idempotent() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    let created: Value = client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "poll the build"}),
        )
        .await
        .unwrap();
    let loop_id = created["loop"]["loopId"].as_str().unwrap().to_owned();

    wait_for("the loop to wait after round 1", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("waiting"))
    })
    .await;
    let delivered = loop_rounds(&history(&mut client, &session).await).len();
    assert_eq!(delivered, 1, "a waiting loop delivers nothing more");
    assert!(
        stored_loops(home.path())[0]["next_at"].is_null(),
        "a waiting loop holds no next_at"
    );

    let first: Value = client
        .rpc(
            "session.loop.stop",
            json!({"sessionId": session, "loopId": loop_id}),
        )
        .await
        .unwrap();
    assert_eq!(first["loop"]["state"], "stopped");
    assert!(
        first["loop"]["settledAt"].is_string(),
        "a terminal loop carries settledAt"
    );
    let settled_at = first["loop"]["settledAt"].clone();

    let second: Value = client
        .rpc(
            "session.loop.stop",
            json!({"sessionId": session, "loopId": loop_id}),
        )
        .await
        .unwrap();
    assert_eq!(second["loop"]["state"], "stopped");
    assert_eq!(
        second["loop"]["settledAt"], settled_at,
        "a second stop rewrites nothing"
    );
    assert_eq!(second["settled"], json!(false));
}

/// `next` arms the following round; an hour past the ceiling is clamped to it.
/// The tool is the model's path, so the test drives it as a tool call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_clamps_its_delay_to_the_ceiling() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &["loop"], true).await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "poll the build"}),
        )
        .await
        .unwrap();
    wait_for("the loop to wait", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("waiting"))
    })
    .await;
    let loop_id = stored_loops(home.path())[0]["loop_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The model answers with one `loop` call: next, an hour past the ceiling.
    model.push(vec![ToolCall {
        id: "call-1".into(),
        name: "loop".into(),
        input: json!({"action": "next", "delay_seconds": 100_000})
            .as_object()
            .cloned()
            .unwrap(),
    }]);
    let before = dim_agent_scheduler::testing::clock(home.path()).now();
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "keep going"}),
        )
        .await
        .unwrap();

    wait_for("the armed round", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("running"))
    })
    .await;
    let record = &stored_loops(home.path())[0];
    assert_eq!(record["loop_id"], json!(loop_id));
    let next_at = record["next_at"].as_str().expect("an armed round");
    let at = chrono::DateTime::parse_from_rfc3339(next_at)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let delta = (at - before).num_seconds();
    assert!(
        (3590..=3610).contains(&delta),
        "100000 s is clamped to 3600 s, got {delta}"
    );
}

/// An expired loop settles `expired` and delivers nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_loop_settles_expired() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "watch"}),
        )
        .await
        .unwrap();
    wait_for("the loop file", || !stored_loops(home.path()).is_empty()).await;

    // An hour out, so the expiry below is reached by the advance and by no real
    // wait the row could take.
    let past = (dim_agent_scheduler::testing::clock(home.path()).now()
        + chrono::Duration::seconds(3600))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_loops(home.path(), |loops| {
        loops[0]["expires_at"] = json!(past);
    });
    dim_agent_scheduler::testing::clock(home.path()).advance(chrono::Duration::seconds(3610));
    dim_agent_scheduler::testing::step(home.path(), 1).await;
    wait_for("expiry", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("expired"))
    })
    .await;
}

/// A loop whose branch is gone settles `stopped` and records the code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gone_branch_settles_stopped() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "watch"}),
        )
        .await
        .unwrap();
    wait_for("the loop to wait", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("waiting"))
    })
    .await;

    // A branch that does not exist answers `branch_missing`, as a deleted
    // target would. The window is an hour out, so only the advance reaches it.
    let at = (dim_agent_scheduler::testing::clock(home.path()).now()
        + chrono::Duration::seconds(3600))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_loops(home.path(), |loops| {
        loops[0]["branch_id"] = json!("gone-branch");
        loops[0]["state"] = json!("running");
        loops[0]["next_at"] = json!(at);
        loops[0]["delivering_round"] = Value::Null;
    });
    dim_agent_scheduler::testing::clock(home.path()).advance(chrono::Duration::seconds(3610));
    dim_agent_scheduler::testing::step(home.path(), 1).await;
    wait_for("a stopped loop", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("stopped"))
    })
    .await;
    let record = &stored_loops(home.path())[0];
    assert!(
        record["stopped_reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "the code that stopped it is recorded: {record}"
    );
}

/// A restart settles a round a previous process left `delivering` by lookup,
/// and writes no second round: the ablation that writes again would deliver a
/// duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_settles_a_delivering_round_by_lookup() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "watch"}),
        )
        .await
        .unwrap();
    wait_for("round 1", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["round"] == json!(2))
    })
    .await;
    let delivered = loop_rounds(&history(&mut client, &session).await).len();
    assert_eq!(delivered, 1);

    // The state a crash between the input write and the settle leaves.
    edit_loops(home.path(), |loops| {
        loops[0]["round"] = json!(1);
        loops[0]["delivering_round"] = json!(1);
        loops[0]["state"] = json!("running");
        loops[0]["next_at"] = Value::Null;
        loops[0]["last_round_at"] = Value::Null;
    });
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    wait_for("the lookup pass to settle the round", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["delivering_round"].is_null())
    })
    .await;
    let after = loop_rounds(&history(&mut client, &session).await).len();
    assert_eq!(
        after, delivered,
        "the accepted round found by lookup settles it; nothing is written again"
    );
}

/// A restart whose one startup lookup answers nothing settles the round on the
/// timer's next pass, not a minute later: while the round is unsettled the
/// loop writes nothing, so a minute of waiting is a minute without a round.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_retries_an_unanswered_lookup_on_the_next_tick() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "watch"}),
        )
        .await
        .unwrap();
    wait_for("round 1", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["round"] == json!(2))
    })
    .await;
    let delivered = loop_rounds(&history(&mut client, &session).await).len();
    assert_eq!(delivered, 1);

    // The state a crash between the input write and the settle leaves.
    edit_loops(home.path(), |loops| {
        loops[0]["round"] = json!(1);
        loops[0]["delivering_round"] = json!(1);
        loops[0]["state"] = json!("running");
        loops[0]["next_at"] = Value::Null;
        loops[0]["last_round_at"] = Value::Null;
    });
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = start_armed(home.path(), Arc::new(Script::default()), |holds| {
        holds.fail_lookups(1);
    })
    .await;
    let mut client = connect(&server).await;

    // The startup pass's lookup was lost, so the round stays `delivering` and
    // the per-tick pass asks again. One crossed tick is that retry — far short
    // of the minute a one-shot startup pass would leave it unsettled for — and
    // the record is read once after it.
    dim_agent_scheduler::testing::step(home.path(), 1).await;
    assert!(
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["delivering_round"].is_null()),
        "the per-tick pass settles the round after the unanswered startup lookup"
    );

    let after = loop_rounds(&history(&mut client, &session).await).len();
    assert_eq!(
        after, delivered,
        "the accepted round is settled by lookup; nothing is written again"
    );
}

/// `session.loop.list` answers on a cold Session: Rpc restores it for the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_works_on_a_cold_session() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "cold"}),
        )
        .await
        .unwrap();
    client
        .rpc(
            "session.idle",
            json!({"sessionId": session, "branchId": "main"}),
        )
        .await
        .unwrap();

    let listed: Value = client
        .rpc("session.loop.list", json!({"sessionId": session}))
        .await
        .unwrap();
    assert_eq!(
        listed["loops"].as_array().map(Vec::len),
        Some(1),
        "the read restores the Session and answers"
    );
}

/// A fixed loop's window is skipped while the round it delivered is still
/// pending: the skip writes nothing, and releasing the branch delivers the next
/// round at the following window.
///
/// `input/consumed` is written when a turn consumes an input
/// (`crates/dim-agent-capabilities/src/input_queue.rs:86-96`), so a held model
/// request does not by itself keep the round pending — the round's turn has
/// started and consumed it. The branch is therefore held by a *first* plain
/// Queue input, which the loop's round queues behind and which stays pending
/// until the hold is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fixed_loop_skips_a_window_while_its_round_is_pending() {
    let home = home();
    let model = Arc::new(Script::default());
    model.hold("busy-marker");
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    // The first input's turn is held inside the model request, so the branch
    // is busy and the loop's round waits behind it, applied but unconsumed.
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "busy-marker"}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if model.count() >= 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first turn's request is held");

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "poll the build", "everySeconds": 60}),
        )
        .await
        .unwrap();
    wait_for("round one to be delivered", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["last_input_id"].is_string())
    })
    .await;
    let rounds = loop_rounds(&history(&mut client, &session).await).len();
    assert_eq!(rounds, 1, "round one is delivered");

    // Bring the next window an hour forward while that round is still pending:
    // the pass must skip it rather than queue a second round. The window is far
    // enough that only the advance below reaches it.
    let due = (dim_agent_scheduler::testing::clock(home.path()).now()
        + chrono::Duration::seconds(3600))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_loops(home.path(), |loops| {
        loops[0]["next_at"] = json!(due);
    });
    dim_agent_scheduler::testing::clock(home.path()).advance(chrono::Duration::seconds(3610));
    wait_for("the skipped window", || {
        stored_loops(home.path()).first().is_some_and(|record| {
            record["next_at"]
                .as_str()
                .is_some_and(|at| at.as_bytes() > due.as_bytes())
        })
    })
    .await;
    assert_eq!(
        loop_rounds(&history(&mut client, &session).await).len(),
        rounds,
        "a skipped window writes nothing"
    );

    // Releasing the branch consumes the held round, so a later pass delivers
    // round 2. A pass that finds the branch still pending rewrites the window a
    // whole cadence ahead, so the window is re-armed due at each tick: the
    // delivery lands at the first tick whose pass finds the branch free, and
    // every tick here is crossed by the clock, not by a real second.
    model.release();
    let clock = dim_agent_scheduler::testing::clock(home.path());
    let mut delivered = false;
    for _ in 0..10 {
        let due = clock
            .now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        edit_loops(home.path(), |loops| {
            loops[0]["next_at"] = json!(due);
        });
        dim_agent_scheduler::testing::step(home.path(), 1).await;
        if loop_rounds(&history(&mut client, &session).await).len() == 2 {
            delivered = true;
            break;
        }
    }
    assert!(delivered, "round two is delivered");
    // The input write precedes the settle, so a round read straight after the
    // history shows it can still be the round before the settle.
    wait_for("the round to settle", || {
        stored_loops(home.path())[0]["round"] == json!(3)
    })
    .await;
    assert_eq!(
        loop_rounds(&history(&mut client, &session).await).len(),
        2,
        "round two is the second delivery"
    );
}

/// A `next` accepted while the delivery pass holds the loop lock keeps its
/// deadline: the pass settles first and the next writes over the settled
/// record, so the loop stays running with the deadline the model chose.
///
/// The pass persists `delivering_round` before its input write and settles
/// after it, so holding the store's write connection keeps it between those two
/// points with the loop lock taken. Without that lock the next would write
/// inside the pass and the settle would overwrite the deadline with the state
/// the pass computed before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_next_accepted_while_the_pass_holds_the_lock_keeps_its_deadline() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    let created: Value = client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "watch the deploy"}),
        )
        .await
        .unwrap();
    let loop_id = created["loop"]["loopId"].as_str().unwrap().to_owned();
    wait_for("the loop to wait after round 1", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["state"] == json!("waiting"))
    })
    .await;

    // The next window is an hour out, so the pass reaches its input write only
    // once the advance below brings it there, while the store's write
    // connection is held.
    let clock = dim_agent_scheduler::testing::clock(home.path());
    let due = (clock.now() + chrono::Duration::seconds(3600))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_loops(home.path(), |loops| {
        loops[0]["state"] = json!("running");
        loops[0]["next_at"] = json!(due);
        loops[0]["delivering_round"] = Value::Null;
    });
    clock.advance(chrono::Duration::seconds(3610));

    let lock = rusqlite::Connection::open(home.path().join("dimcode.sqlite")).unwrap();
    lock.busy_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();

    // The pass marks the round delivering, then waits on the held connection
    // while it holds the loop lock.
    wait_for("the pass to reach its input write", || {
        stored_loops(home.path())
            .first()
            .is_some_and(|record| record["delivering_round"] == json!(2))
    })
    .await;

    // A next accepted now is accepted well inside the pass. In the serialized
    // order it waits for the loop lock, so it lands after the settle write; an
    // unserialized one would land before it and be overwritten.
    let loops = server
        .service::<dim_agent_scheduler::Loops>()
        .expect("the Scheduler process part provides its loop service");
    let before = dim_agent_scheduler::testing::clock(home.path()).now();
    let mut arming = Box::pin(loops.next(&session, &loop_id, 600));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut arming)
            .await
            .is_err(),
        "the next waits for the pass's loop lock"
    );

    lock.execute_batch("COMMIT").unwrap();
    drop(lock);
    let armed = arming.await.unwrap();
    assert_eq!(armed.state.name(), "running", "{armed:?}");

    // The deadline the next stored is the one the file holds: the settle wrote
    // first, so it cannot have replaced this deadline with the waiting state.
    let record = &stored_loops(home.path())[0];
    assert_eq!(record["state"], json!("running"), "{record}");
    let at = chrono::DateTime::parse_from_rfc3339(
        record["next_at"].as_str().expect("an armed deadline"),
    )
    .unwrap()
    .with_timezone(&chrono::Utc);
    let delta = (at - before).num_seconds();
    assert!(
        (590..=610).contains(&delta),
        "the deadline is the 600 seconds the next chose, got {delta}"
    );
}

/// A `start` that races a pass expiring the line's last live loop waits for it,
/// so its record write and its live-loop refresh land after the pass settled:
/// the flag stores the file no later write supersedes, and the new loop's round
/// 1 goes out on the next pass instead of waiting for another mutation.
///
/// An expiry writes no input, so the pass is held at the input write of a
/// second Session's due loop in the same snapshot: it has read the snapshot,
/// holds the loop lock and has the expiry change uncommitted while it waits on
/// the held store connection. That is the seam
/// `a_next_accepted_while_the_pass_holds_the_lock_keeps_its_deadline` uses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_racing_an_expiry_pass_waits_and_delivers_round_one() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let other = create_session(&mut client, home.path(), &[], true).await;

    let created: Value = client
        .rpc(
            "session.loop.start",
            json!({"sessionId": session, "prompt": "old"}),
        )
        .await
        .unwrap();
    let old = created["loop"]["loopId"].as_str().unwrap().to_owned();
    wait_for("the first loop to wait after round 1", || {
        stored_loops(home.path())
            .iter()
            .any(|record| record["loop_id"] == json!(old) && record["state"] == json!("waiting"))
    })
    .await;

    client
        .rpc(
            "session.loop.start",
            json!({"sessionId": other, "prompt": "other"}),
        )
        .await
        .unwrap();
    wait_for("the second loop's round 1", || {
        stored_loops(home.path())
            .iter()
            .any(|record| record["session_id"] == json!(other) && record["round"] == json!(2))
    })
    .await;

    // The first loop is past its expiry, and the second is due again, so one
    // pass both expires the first and reaches an input write for the second.
    // The second window is an hour out, so only the advance after the edit
    // brings the pass to it.
    let clock = dim_agent_scheduler::testing::clock(home.path());
    let past = (clock.now() - chrono::Duration::seconds(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let due = (clock.now() + chrono::Duration::seconds(3600))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_loops(home.path(), |loops| {
        for record in loops.iter_mut() {
            if record["loop_id"] == json!(old) {
                record["expires_at"] = json!(past);
            }
            if record["session_id"] == json!(other) {
                record["state"] = json!("running");
                record["next_at"] = json!(due);
                record["delivering_round"] = Value::Null;
            }
        }
    });
    clock.advance(chrono::Duration::seconds(3610));

    let lock = rusqlite::Connection::open(home.path().join("dimcode.sqlite")).unwrap();
    lock.busy_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();

    // The pass has read the snapshot with the loop lock taken, marked the
    // second loop's round delivering, and waits on the held connection with
    // the expiry change still uncommitted.
    wait_for("the pass to reach its input write", || {
        stored_loops(home.path()).iter().any(|record| {
            record["session_id"] == json!(other) && record["delivering_round"] == json!(2)
        })
    })
    .await;

    let id = client
        .send(
            "session.loop.start",
            json!({"sessionId": session, "branchId": "main", "prompt": "new"}),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), client.response(id))
            .await
            .is_err(),
        "the start waits for the pass's loop lock"
    );

    lock.execute_batch("COMMIT").unwrap();
    drop(lock);
    let started: Value = client.response(id).await.unwrap();
    let started = &started["loop"];
    assert_eq!(started["state"], json!("running"), "{started}");
    let new_id = started["loopId"].as_str().unwrap().to_owned();

    let records = stored_loops(home.path());
    assert!(
        records
            .iter()
            .any(|record| record["loop_id"] == json!(old) && record["state"] == json!("expired")),
        "the pass settled the expired loop: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record["loop_id"] == json!(new_id)),
        "the start stored its record after the settle: {records:?}"
    );

    // The refresh left the flag true, so the next pass delivers the new loop's
    // round 1 rather than taking the early return.
    let marker =
        format!("<dim-scheduler kind=\"loop\" loop=\"{new_id}\" round=\"1\"></dim-scheduler>");
    wait_for("the new loop's round 1", || {
        stored_loops(home.path())
            .iter()
            .any(|record| record["loop_id"] == json!(new_id) && record["round"] == json!(2))
    })
    .await;
    let rounds = loop_rounds(&history(&mut client, &session).await);
    assert!(
        rounds.iter().any(|event| marker_of(event) == marker),
        "the next pass delivered the new loop's round 1: {rounds:?}"
    );
}
