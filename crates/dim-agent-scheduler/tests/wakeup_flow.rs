//! Session-scoped wakeups through a real App Server (design §10): create,
//! list, cancel and fire; restart and expiry; an unloaded target and a
//! deleted one; the change notices an attached client receives (design §11.1).
//! Timing follows `timer_flow.rs`: a real clock, an instant two or three
//! seconds ahead, and a short poll with a hard timeout.
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{Agent, AgentBuilder, AgentConfig};
use dim_agent_scheduler::install;
use dim_agent_types::*;
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Message, Server};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    path::Path,
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
    fn push(&self, calls: Vec<ToolCall>) {
        self.batches.lock().unwrap().push_back(calls);
    }
    fn texts(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect()
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
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
        client_name: "wakeup-test",
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

async fn wait_for(description: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

/// An interactive leader Session (unless `interactive` is false) mounting
/// exactly `tools`, in the project at `cwd`. A restore authorizes the project
/// again, so a test can make it unavailable to a later lookup.
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

fn instant_after(seconds: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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

fn wakeup_inputs(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| {
            event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["blocks"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("dim-scheduler kind=\"wakeup\""))
                    })
                })
        })
        .collect()
}

/// Polls until the session's `main` branch has recorded a finished turn.
async fn wait_turn_end(client: &mut Client, session_id: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let ended = history(client, session_id)
                .await
                .iter()
                .any(|event| event["type"] == "turn" && event["name"] == "end");
            if ended {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the turn ends");
}

/// The wakeup id a delivered input's marker names.
fn wakeup_id_of(event: &Value) -> String {
    let marker = event["data"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|block| block["text"].as_str())
        .expect("an input carries a text block");
    marker
        .trim_start_matches("<dim-scheduler kind=\"wakeup\">")
        .trim_end_matches("</dim-scheduler>")
        .to_owned()
}

/// The stored wakeup records, as the file holds them.
fn stored_wakeups(home: &Path) -> Vec<Value> {
    let path = home.join("scheduler/wakeups.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    inner["wakeups"].as_array().cloned().unwrap_or_default()
}

/// Rewrites the wakeup store as a crash or another process would leave it.
fn edit_wakeups(home: &Path, change: impl FnOnce(&mut Vec<Value>)) {
    let path = home.join("scheduler/wakeups.json");
    let mut inner: Value = match std::fs::read(&path) {
        Ok(bytes) => {
            let envelope: Value = serde_json::from_slice(&bytes).unwrap();
            serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap()
        }
        Err(_) => json!({"version": 1, "wakeups": []}),
    };
    change(inner["wakeups"].as_array_mut().unwrap());
    let text = serde_json::to_string(&inner).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"version": 1, "text": text})).unwrap(),
    )
    .unwrap();
}

/// Polls until the wakeup store holds `state` for record `wakeup_id`.
async fn wait_state(home: &Path, wakeup_id: &str, state: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if stored_wakeups(home)
                .iter()
                .any(|wakeup| wakeup["wakeup_id"] == wakeup_id && wakeup["state"] == state)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("wakeup {wakeup_id} never reached {state}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wakeup_fires_one_turn_on_the_same_session() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let before = history(&mut client, &session).await;

    let at = instant_after(2);
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": at,
                "prompt": "Continue the review."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();

    let listed: Value = client
        .rpc("session.wakeup.list", json!({"sessionId": session}))
        .await
        .unwrap();
    let records = listed["wakeups"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{listed}");
    assert_eq!(records[0]["wakeupId"], json!(wakeup_id), "{listed}");
    assert_eq!(records[0]["at"], json!(at), "{listed}");
    assert_eq!(
        records[0]["prompt"],
        json!("Continue the review."),
        "{listed}"
    );
    assert_eq!(records[0]["state"], json!("scheduled"), "{listed}");

    // A wakeup is not a job: it never shows in the job list.
    let jobs: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert!(jobs["jobs"].as_array().unwrap().is_empty(), "{jobs}");

    wait_state(home.path(), &wakeup_id, "fired").await;

    // The turn ran on the same Session, with its earlier history intact and
    // exactly one wakeup input.
    wait_for("the wakeup turn to reach the model", || model.count() >= 1).await;
    let after = history(&mut client, &session).await;
    assert!(
        after[..before.len()] == before[..],
        "the history before the wakeup is intact"
    );
    let inputs = wakeup_inputs(&after);
    assert_eq!(inputs.len(), 1, "{after:?}");
    assert!(
        inputs[0]["data"]["blocks"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&wakeup_id),
        "the marker body is the wakeup id: {}",
        inputs[0]
    );

    // The guide entry and the marker reach the model.
    let texts = model.texts();
    assert!(
        texts.contains(
            "a wakeup's prompt, delivered once at its set time as one turn on the same Session"
        ),
        "the dim-scheduler guide entry reaches the model"
    );
    assert!(
        texts.contains("Continue the review."),
        "the prompt is delivered: {texts}"
    );

    let listed: Value = client
        .rpc("session.wakeup.list", json!({"sessionId": session}))
        .await
        .unwrap();
    assert_eq!(listed["wakeups"][0]["state"], json!("fired"), "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_wakeup_never_fires() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(30),
                "prompt": "Should never arrive."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    let cancelled: Value = client
        .rpc(
            "session.wakeup.cancel",
            json!({"sessionId": session, "wakeupId": wakeup_id}),
        )
        .await
        .unwrap();
    assert_eq!(cancelled, json!({}), "a scheduled record cancels to {{}}");

    let listed: Value = client
        .rpc("session.wakeup.list", json!({"sessionId": session}))
        .await
        .unwrap();
    assert_eq!(
        listed["wakeups"][0]["state"],
        json!("cancelled"),
        "{listed}"
    );

    // Cancelling again is not an error after the fact; it reports the state.
    let again: Value = client
        .rpc(
            "session.wakeup.cancel",
            json!({"sessionId": session, "wakeupId": wakeup_id}),
        )
        .await
        .unwrap();
    assert_eq!(again["state"], json!("cancelled"), "{again}");

    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(model.count(), 0, "a cancelled wakeup starts no turn");
    assert!(wakeup_inputs(&history(&mut client, &session).await).is_empty());

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instant_past_the_grace_is_expired_at_create() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;

    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(-120),
                "prompt": "Too late."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();

    let listed: Value = client
        .rpc("session.wakeup.list", json!({"sessionId": session}))
        .await
        .unwrap();
    assert_eq!(listed["wakeups"][0]["state"], json!("expired"), "{listed}");

    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(model.count(), 0, "an expired wakeup starts no turn");
    assert!(wakeup_inputs(&history(&mut client, &session).await).is_empty());
    assert_eq!(
        stored_wakeups(home.path())[0]["state"],
        json!("expired"),
        "the record stays expired"
    );
    assert_eq!(wakeup_id, stored_wakeups(home.path())[0]["wakeup_id"]);

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_fires_a_window_still_inside_the_grace() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    // Ten seconds ahead: far enough that stopping the first daemon always
    // wins the race, and the restart below lands inside the window's 60 s
    // grace. The first daemon must not fire it; the fresh model of the
    // second daemon is the evidence that the restart did.
    let at = instant_after(10);
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": at,
                "prompt": "After the restart."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The daemon stays down past the window; the restart at ~12 s after the
    // instant is still inside the 60 s grace, so the wakeup fires once.
    tokio::time::sleep(Duration::from_secs(12)).await;
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    wait_state(home.path(), &wakeup_id, "fired").await;
    wait_for("the restarted daemon's turn", || model.count() >= 1).await;
    let events = history(&mut client, &session).await;
    assert_eq!(wakeup_inputs(&events).len(), 1, "{events:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_expires_a_window_past_the_grace_and_never_replays() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "Lost to the downtime."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The 60 s grace makes a real-clock version sleep a minute before the
    // restart; the edit stands in for the elapsed time. The classifier at
    // the restart sees exactly what it would after downtime of that length:
    // a `scheduled` record whose instant is past the grace. The within-grace
    // restart above exercises the real clock.
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["at"] = json!(instant_after(-120));
    });
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    wait_state(home.path(), &wakeup_id, "expired").await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(model.count(), 0, "an expired wakeup is never replayed");
    assert!(wakeup_inputs(&history(&mut client, &session).await).is_empty());

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delivered_wakeup_past_the_grace_is_fired_after_a_restart() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(2),
                "prompt": "Delivered before the crash."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    wait_state(home.path(), &wakeup_id, "fired").await;
    wait_turn_end(&mut client, &session).await;
    let events = history(&mut client, &session).await;
    assert_eq!(wakeup_inputs(&events).len(), 1, "{events:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The crash the settle write did not survive: the input was published but
    // the record is still `scheduled`, and the restart happens long after the
    // instant left the grace. The submission id the input carries is the only
    // evidence that the prompt was delivered, so the pass must look it up
    // before it expires the record.
    edit_wakeups(home.path(), |wakeups| {
        let record = wakeups[0].as_object_mut().unwrap();
        record.insert("state".into(), json!("scheduled"));
        record.remove("settled_at");
        record.insert("at".into(), json!(instant_after(-120)));
    });

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    wait_state(home.path(), &wakeup_id, "fired").await;

    // Exactly once: the found submission settles the record without writing
    // a second input or starting a second turn.
    let events = history(&mut client, &session).await;
    assert_eq!(wakeup_inputs(&events).len(), 1, "{events:?}");
    assert_eq!(
        stored_wakeups(home.path())[0]["state"],
        json!("fired"),
        "a delivered record is not expired"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(model.count(), 0, "the settle writes no input");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_lookup_leaves_a_record_past_the_grace_scheduled() {
    let home = home();
    let project = home.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, &project, &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "Delivery unknown."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A transient failure, not a definite answer: the restart leaves the
    // target unloaded and its project directory gone, so the pass's lookup
    // restores the Session, project authorization fails, and the answer is
    // `failed`. The lookup is the real service call; no test double is
    // involved.
    std::fs::remove_dir_all(&project).unwrap();
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["at"] = json!(instant_after(-120));
    });
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    // Several passes see the failed lookup: the record must stay scheduled
    // rather than be expired on a guess.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        stored_wakeups(home.path())[0]["state"],
        json!("scheduled"),
        "a failed lookup leaves the record for the next pass"
    );
    // The restore really fails: the same acquire the pass's `sessions.lookup`
    // performs cannot bring the Session back while the project is gone.
    assert!(
        client
            .rpc(
                "session.history",
                json!({"sessionId": session, "branchId": "main", "after": 0, "limit": 256}),
            )
            .await
            .is_err(),
        "the project authorization refuses the restore"
    );

    // The project returns, the lookup answers — the submission was never
    // accepted — and the record settles expired.
    std::fs::create_dir_all(&project).unwrap();
    wait_state(home.path(), &wakeup_id, "expired").await;
    assert!(wakeup_inputs(&history(&mut client, &session).await).is_empty());
    assert_eq!(model.count(), 0, "the failed pass never wrote an input");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_record_whose_target_is_gone_settles_cancelled_and_is_pruned() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "Nobody to wake."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    client
        .rpc("session.delete", json!({"sessionId": session}))
        .await
        .unwrap();

    // The target is gone for good and the record is past the grace, so the
    // pass's lookup answers `session_closed`: a definite answer, settled
    // `cancelled` instead of repeating a lookup that can never succeed.
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["at"] = json!(instant_after(-120));
    });
    wait_state(home.path(), &wakeup_id, "cancelled").await;
    assert_eq!(model.count(), 0, "a gone target starts no turn");

    // Cancelled is terminal like fired or expired: the seven-day retention
    // removes it by its settle time.
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["settled_at"] = json!(
            (chrono::Utc::now() - chrono::Duration::days(8))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
    });
    wait_for("the cancelled record to be pruned", || {
        stored_wakeups(home.path()).is_empty()
    })
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_past_the_grace_with_no_accepted_input_is_expired_once() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "On the boundary."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let at = (chrono::Utc::now() - chrono::Duration::seconds(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["at"] = json!(at);
    });
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    wait_state(home.path(), &wakeup_id, "expired").await;
    let settled_at = stored_wakeups(home.path())[0]["settled_at"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(model.count(), 0, "the boundary record never fires");
    assert!(wakeup_inputs(&history(&mut client, &session).await).is_empty());

    // Exactly one terminal state: a later pass does not re-settle or fire it.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let stored = stored_wakeups(home.path());
    assert_eq!(stored[0]["state"], json!("expired"), "{stored:?}");
    assert_eq!(
        stored[0]["settled_at"].as_str().unwrap(),
        settled_at,
        "the settle happened exactly once: {stored:?}"
    );
    assert_eq!(model.count(), 0);
    assert!(wakeup_inputs(&history(&mut client, &session).await).is_empty());

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ended_branch_cancels_its_record_and_never_retries() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    // A branch can only be opened when the Session mounts the `branch` tool.
    let session = create_session(&mut client, home.path(), &["branch"], true).await;
    let agent: Agent = server.agent().expect("the default Agent");
    let at = agent
        .get_session(&session)
        .unwrap()
        .sequence("main")
        .unwrap();
    let child = client
        .rpc(
            "session.branch_create",
            json!({"sessionId": session, "parentBranchId": "main", "at": at,
                "prompt": "Child work."}),
        )
        .await
        .unwrap()["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": child, "at": instant_after(6),
                "prompt": "Into the ended line."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    client
        .rpc(
            "session.branch_end",
            json!({"sessionId": session, "branchId": child}),
        )
        .await
        .unwrap();

    // An ended line accepts no new input, so the record settles cancelled like
    // one whose Session is gone instead of retrying until the grace expires.
    wait_state(home.path(), &wakeup_id, "cancelled").await;
    let events = history(&mut client, &session).await;
    assert!(
        wakeup_inputs(&events).is_empty(),
        "the ended line never takes the input: {events:?}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overdue_wakeups_fire_in_at_order_with_a_stable_tie_breaker() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let mut ids = Vec::new();
    for prompt in ["last", "tied one", "tied two", "third"] {
        let created: Value = client
            .rpc(
                "session.wakeup.create",
                json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                    "prompt": prompt}),
            )
            .await
            .unwrap();
        ids.push(created["wakeupId"].as_str().unwrap().to_owned());
    }
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The downtime leaves every record past its instant but inside the grace,
    // and the file's order is not the due order: last, tied, tied, third.
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["at"] = json!(instant_after(-20));
        wakeups[1]["at"] = json!(instant_after(-40));
        wakeups[2]["at"] = json!(instant_after(-40));
        wakeups[3]["at"] = json!(instant_after(-30));
    });

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    for id in &ids {
        wait_state(home.path(), id, "fired").await;
    }

    let events = history(&mut client, &session).await;
    let fired: Vec<String> = wakeup_inputs(&events)
        .iter()
        .map(|event| wakeup_id_of(event))
        .collect();
    let mut tied = [ids[1].clone(), ids[2].clone()];
    tied.sort();
    assert_eq!(
        fired,
        vec![
            tied[0].clone(),
            tied[1].clone(),
            ids[3].clone(),
            ids[0].clone()
        ],
        "overdue wakeups fire in at order, sharing an instant in id order"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_records_are_pruned_after_their_retention_age() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    for prompt in ["Old.", "Recent."] {
        client
            .rpc(
                "session.wakeup.create",
                json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                    "prompt": prompt}),
            )
            .await
            .unwrap();
    }
    // The timer edits stand in for the two terminal ages: the first record
    // was created a moment ago and has been terminal for eight days, the
    // second was created eight days ago and has been terminal for one. Both
    // contrasts make `settled_at`, not `created_at`, the cutoff.
    let days = |days: i64| {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["state"] = json!("fired");
        wakeups[0]["created_at"] = json!(days(0));
        wakeups[0]["settled_at"] = json!(days(8));
        wakeups[1]["state"] = json!("cancelled");
        wakeups[1]["created_at"] = json!(days(8));
        wakeups[1]["settled_at"] = json!(days(1));
    });
    wait_for("the old terminal record to be pruned", || {
        stored_wakeups(home.path()).len() == 1
    })
    .await;
    let stored = stored_wakeups(home.path());
    assert_eq!(stored[0]["state"], json!("cancelled"), "{stored:?}");
    assert_eq!(stored[0]["prompt"], json!("Recent."), "{stored:?}");
    // `list` reads what is stored: it shows the surviving record only.
    let listed: Value = client
        .rpc("session.wakeup.list", json!({"sessionId": session}))
        .await
        .unwrap();
    let records = listed["wakeups"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{listed}");
    assert_eq!(records[0]["state"], json!("cancelled"), "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unloaded_target_is_restored_before_the_input() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "Warm up."}),
        )
        .await
        .unwrap();
    wait_for("the first turn", || model.count() >= 1).await;
    let at = instant_after(6);
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": at,
                "prompt": "Back from cold storage."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();

    // Unload the idle Session instance, as the daemon's idle pass would.
    let agent: Agent = server.agent().expect("the default Agent");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(loaded) = agent.get_session(&session)
                && loaded.unload_if_idle().await.unwrap_or(false)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the idle Session unloads");

    wait_state(home.path(), &wakeup_id, "fired").await;
    wait_for("the restored Session's turn", || model.count() >= 2).await;
    let events = history(&mut client, &session).await;
    assert_eq!(wakeup_inputs(&events).len(), 1, "{events:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deleted_target_cancels_its_record_and_never_relaunches() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(4),
                "prompt": "Nobody is left."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    client
        .rpc("session.delete", json!({"sessionId": session}))
        .await
        .unwrap();

    wait_state(home.path(), &wakeup_id, "cancelled").await;
    assert_eq!(model.count(), 0, "a deleted target starts no turn");
    let error = client
        .rpc(
            "session.history",
            json!({"sessionId": session, "branchId": "main"}),
        )
        .await
        .unwrap_err();
    assert_eq!(dim_client::app_error(&error), Some("session_closed"));

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scope_and_param_errors() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let first = create_session(&mut client, home.path(), &[], true).await;
    let second = create_session(&mut client, home.path(), &[], true).await;
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": first, "branchId": "main", "at": instant_after(30),
                "prompt": "Mine."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();

    // Another Session's id cannot cancel this record.
    let error = client
        .rpc(
            "session.wakeup.cancel",
            json!({"sessionId": second, "wakeupId": wakeup_id}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        dim_client::app_error(&error),
        Some("wakeup_not_found"),
        "{error}"
    );
    // The record is still this Session's to cancel.
    client
        .rpc(
            "session.wakeup.cancel",
            json!({"sessionId": first, "wakeupId": wakeup_id}),
        )
        .await
        .unwrap();

    for (case, params) in [
        (
            "a malformed at",
            json!({"sessionId": first, "branchId": "main", "at": "tomorrow", "prompt": "x"}),
        ),
        (
            "an unknown field",
            json!({"sessionId": first, "branchId": "main", "at": instant_after(30),
                "prompt": "x", "extra": 1}),
        ),
        (
            "a missing branchId",
            json!({"sessionId": first, "at": instant_after(30), "prompt": "x"}),
        ),
        (
            "a missing prompt",
            json!({"sessionId": first, "branchId": "main", "at": instant_after(30)}),
        ),
        (
            "an empty prompt",
            json!({"sessionId": first, "branchId": "main", "at": instant_after(30), "prompt": ""}),
        ),
    ] {
        let error = client
            .rpc("session.wakeup.create", params)
            .await
            .unwrap_err();
        assert_eq!(
            dim_client::app_error(&error),
            Some("invalid_params"),
            "{case}: {error}"
        );
    }

    // An unknown Session id names no Session; the method never runs.
    let error = client
        .rpc(
            "session.wakeup.list",
            json!({"sessionId": "no-such-session"}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        dim_client::app_error(&error),
        Some("session_closed"),
        "{error}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversized_prompt_is_invalid_params_at_both_entry_points() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &["wakeup"], true).await;
    let oversized = "x".repeat(32 * 1024 + 1);

    // The method answers the caller's error, not `invalid_data`.
    let error = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(30),
                "prompt": oversized}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        dim_client::app_error(&error),
        Some("invalid_params"),
        "{error}"
    );

    // The tool handler refuses the same bound as the method.
    model.push(vec![ToolCall {
        id: "w1".into(),
        name: "wakeup".into(),
        input: json!({"at": instant_after(30), "prompt": oversized})
            .as_object()
            .unwrap()
            .clone(),
    }]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "Too long."}),
        )
        .await
        .unwrap();
    wait_for("the tool's invalid_params result", || {
        model.texts().contains("wakeup: invalid_params")
    })
    .await;
    assert!(
        stored_wakeups(home.path()).is_empty(),
        "neither refused call wrote a record"
    );

    // Exactly 32 KiB is accepted: the bound is a maximum, not a prefix.
    client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "x".repeat(32 * 1024)}),
        )
        .await
        .unwrap();
    let stored = stored_wakeups(home.path());
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["prompt"].as_str().unwrap().len(), 32 * 1024);

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_multi_byte_prompt_over_the_byte_bound_is_invalid_params() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &["wakeup"], true).await;

    // The bound is bytes, and only the handler enforces it: the declaration
    // states it in prose because JSON Schema's `maxLength` counts characters.
    // 12000 three-byte characters are well under 32768 characters and well
    // over 32768 bytes.
    let prompt = "汉".repeat(12_000);
    assert!(prompt.chars().count() < 32_768 && prompt.len() > 32 * 1024);
    model.push(vec![ToolCall {
        id: "w1".into(),
        name: "wakeup".into(),
        input: json!({"at": instant_after(30), "prompt": prompt})
            .as_object()
            .unwrap()
            .clone(),
    }]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "Too long by bytes."}),
        )
        .await
        .unwrap();
    wait_for("the tool's invalid_params result", || {
        model.texts().contains("wakeup: invalid_params")
    })
    .await;
    assert!(
        stored_wakeups(home.path()).is_empty(),
        "the refused call wrote no record"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tool_schedules_a_wakeup_for_the_caller_and_refuses_elsewhere() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &["wakeup"], true).await;

    let at = instant_after(2);
    model.push(vec![ToolCall {
        id: "w1".into(),
        name: "wakeup".into(),
        input: json!({"at": at, "prompt": "Tool wake."})
            .as_object()
            .unwrap()
            .clone(),
    }]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "Schedule a wakeup."}),
        )
        .await
        .unwrap();
    wait_for("the tool result", || {
        model.texts().contains("wakeup: created")
    })
    .await;
    let stored = stored_wakeups(home.path());
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["session_id"], json!(session), "{stored:?}");
    assert_eq!(stored[0]["prompt"], json!("Tool wake."), "{stored:?}");
    let wakeup_id = stored[0]["wakeup_id"].as_str().unwrap().to_owned();
    wait_state(home.path(), &wakeup_id, "fired").await;
    wait_for("the wakeup turn", || model.count() >= 2).await;

    // A non-interactive Session gets the same refusal the `schedule` tool
    // gives; the member-role check is the same configuration test.
    let worker = create_session(&mut client, home.path(), &["wakeup"], false).await;
    model.push(vec![ToolCall {
        id: "w2".into(),
        name: "wakeup".into(),
        input: json!({"at": instant_after(30), "prompt": "Nope."})
            .as_object()
            .unwrap()
            .clone(),
    }]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": worker, "branchId": "main", "text": "Try it."}),
        )
        .await
        .unwrap();
    wait_for("the refusal", || {
        model
            .texts()
            .contains("wakeup is available only in interactive leader Sessions")
    })
    .await;
    assert_eq!(
        stored_wakeups(home.path()).len(),
        1,
        "the refused tool wrote nothing"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Reads notifications in arrival order until `accept` takes one, returning
/// its params; `before` sees every notification read on the way.
async fn wait_notice(
    client: &mut Client,
    what: &str,
    mut before: impl FnMut(&str, &Value),
    mut accept: impl FnMut(&str, &Value) -> bool,
) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let Message::Notice { method, params } = client.next().await.unwrap() else {
                continue;
            };
            if accept(&method, &params) {
                return params;
            }
            before(&method, &params);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Whether a `session.notice` announces the wakeup record's `state`.
fn wakeup_state(method: &str, params: &Value, wakeup_id: &str, state: &str) -> bool {
    method == "session.notice"
        && params["source"] == json!("Scheduler")
        && params["kind"] == json!("wakeup")
        && params["data"]["wakeupId"] == json!(wakeup_id)
        && params["data"]["state"] == json!(state)
}

/// Whether a `session.event` notification is the wakeup input the pass wrote,
/// named by its submission id `wakeup:<wakeup_id>`.
fn wakeup_input_event(method: &str, params: &Value, wakeup_id: &str) -> bool {
    method == "session.event"
        && params["event"]["type"] == json!("input")
        && params["event"]["name"] == json!("message")
        && params["event"]["data"]["submission_id"] == json!(format!("wakeup:{wakeup_id}"))
}

/// A committed record change is announced as `session.notice`. A `fired`
/// notice is a committed-state notice, not ordered against the input's
/// `session.event`; a client relates them by the input's submission id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wakeup_changes_reach_an_attached_client_as_session_notices() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    client
        .rpc("session.attach", json!({"sessionId": session}))
        .await
        .unwrap();

    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(3),
                "prompt": "Announce me."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();

    let scheduled = wait_notice(
        &mut client,
        "the scheduled notice",
        |_, _| {},
        |method, params| wakeup_state(method, params, &wakeup_id, "scheduled"),
    )
    .await;
    assert_eq!(scheduled["sessionId"], json!(session), "{scheduled}");
    assert_eq!(scheduled["data"]["branchId"], json!("main"), "{scheduled}");

    // The notice is not ordered against the input's event: collect both
    // without assuming which arrives first.
    let mut input_event = None;
    let fired = wait_notice(
        &mut client,
        "the fired notice",
        |method, params| {
            if wakeup_input_event(method, params, &wakeup_id) {
                input_event = Some(params.clone());
            }
        },
        |method, params| wakeup_state(method, params, &wakeup_id, "fired"),
    )
    .await;
    assert_eq!(fired["data"]["state"], json!("fired"), "{fired}");
    let input = match input_event {
        Some(input) => input,
        None => {
            wait_notice(
                &mut client,
                "the input event",
                |_, _| {},
                |method, params| wakeup_input_event(method, params, &wakeup_id),
            )
            .await
        }
    };
    assert_eq!(
        input["event"]["data"]["submission_id"],
        json!(format!("wakeup:{wakeup_id}")),
        "{input}"
    );

    // The notice reports the committed record; the owner's query is the truth
    // once the notice has arrived.
    let listed: Value = client
        .rpc("session.wakeup.list", json!({"sessionId": session}))
        .await
        .unwrap();
    assert_eq!(listed["wakeups"][0]["state"], json!("fired"), "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A cancel that settles a scheduled record announces `cancelled`; a cancel
/// of the terminal record changes nothing and announces nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settling_cancel_announces_and_a_terminal_cancel_does_not() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    client
        .rpc("session.attach", json!({"sessionId": session}))
        .await
        .unwrap();
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "Cancel me."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    wait_notice(
        &mut client,
        "the scheduled notice",
        |_, _| {},
        |method, params| wakeup_state(method, params, &wakeup_id, "scheduled"),
    )
    .await;

    let cancelled: Value = client
        .rpc(
            "session.wakeup.cancel",
            json!({"sessionId": session, "wakeupId": wakeup_id}),
        )
        .await
        .unwrap();
    assert_eq!(cancelled, json!({}), "{cancelled}");
    wait_notice(
        &mut client,
        "the cancelled notice",
        |_, _| {},
        |method, params| wakeup_state(method, params, &wakeup_id, "cancelled"),
    )
    .await;

    let again: Value = client
        .rpc(
            "session.wakeup.cancel",
            json!({"sessionId": session, "wakeupId": wakeup_id}),
        )
        .await
        .unwrap();
    assert_eq!(again["state"], json!("cancelled"), "{again}");
    let quiet = tokio::time::timeout(Duration::from_millis(1500), async {
        loop {
            let Message::Notice { method, params } = client.next().await.unwrap() else {
                continue;
            };
            if method == "session.notice"
                && params["kind"] == json!("wakeup")
                && params["data"]["wakeupId"] == json!(wakeup_id)
            {
                return params;
            }
        }
    })
    .await;
    assert!(
        quiet.is_err(),
        "a terminal cancel announces nothing: {quiet:?}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A record the pass settles `expired` announces that state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expiry_announces_expired_to_an_attached_client() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &[], true).await;
    client
        .rpc("session.attach", json!({"sessionId": session}))
        .await
        .unwrap();
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": "main", "at": instant_after(120),
                "prompt": "Too late."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
    wait_notice(
        &mut client,
        "the scheduled notice",
        |_, _| {},
        |method, params| wakeup_state(method, params, &wakeup_id, "scheduled"),
    )
    .await;

    // The same edit the restart expiry test uses stands in for the elapsed
    // grace: the pass classifies a `scheduled` record past the window.
    edit_wakeups(home.path(), |wakeups| {
        wakeups[0]["at"] = json!(instant_after(-120));
    });
    let expired = wait_notice(
        &mut client,
        "the expired notice",
        |_, _| {},
        |method, params| wakeup_state(method, params, &wakeup_id, "expired"),
    )
    .await;
    assert_eq!(expired["data"]["branchId"], json!("main"), "{expired}");
    assert_eq!(model.count(), 0, "an expired wakeup starts no turn");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A notice is Session-level: a client attached while the record targets
/// another branch of the same Session receives it too, and `data.branchId`
/// names that branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wakeup_notices_reach_a_client_attached_to_the_whole_session() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &["branch"], true).await;
    let mut other = connect(&server).await;
    client
        .rpc("session.attach", json!({"sessionId": session}))
        .await
        .unwrap();
    other
        .rpc("session.attach", json!({"sessionId": session}))
        .await
        .unwrap();
    let agent: Agent = server.agent().expect("the default Agent");
    let at = agent
        .get_session(&session)
        .unwrap()
        .sequence("main")
        .unwrap();
    let child = client
        .rpc(
            "session.branch_create",
            json!({"sessionId": session, "parentBranchId": "main", "at": at,
                "prompt": "Child work."}),
        )
        .await
        .unwrap()["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    let created: Value = client
        .rpc(
            "session.wakeup.create",
            json!({"sessionId": session, "branchId": child, "at": instant_after(120),
                "prompt": "Into the child."}),
        )
        .await
        .unwrap();
    let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();

    for (client, who) in [
        (&mut client, "the creating client"),
        (&mut other, "the other client"),
    ] {
        let notice = wait_notice(
            client,
            &format!("{who}'s scheduled notice"),
            |_, _| {},
            |method, params| wakeup_state(method, params, &wakeup_id, "scheduled"),
        )
        .await;
        assert_eq!(notice["data"]["branchId"], json!(child), "{notice}");
    }

    drop(other);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_wakeup_tool_reports_the_owners_expired_state() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let session = create_session(&mut client, home.path(), &["wakeup"], true).await;
    model.push(vec![ToolCall {
        id: "expired-wakeup".into(),
        name: "wakeup".into(),
        input: json!({"at": instant_after(-120), "prompt": "Expired request."})
            .as_object()
            .unwrap()
            .clone(),
    }]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId":session,"branchId":"main","text":"Record this wakeup."}),
        )
        .await
        .unwrap();
    wait_for("the expired tool result", || {
        model.texts().contains("wakeup: created")
    })
    .await;
    let records = stored_wakeups(home.path());
    let record = &records[0];
    assert_eq!(record["state"], "expired");
    {
        let requests = model.requests.lock().unwrap();
        let text = requests
            .iter()
            .flat_map(|request| &request.messages)
            .find_map(|message| {
                let MessageContent::Parts(parts) = &message.content else {
                    return None;
                };
                parts.iter().find_map(|part| match part {
                    MessagePart::ToolResult {
                        tool_use_id,
                        content: ToolContent::Text(text),
                        ..
                    } if tool_use_id == "expired-wakeup" => Some(text),
                    _ => None,
                })
            })
            .expect("the next model request contains the tool result");
        let reply: Value = serde_json::from_str(text.split_once('\n').unwrap().1).unwrap();
        assert_eq!(reply["state"], "expired");
        assert_eq!(reply["wakeupId"], record["wakeup_id"]);
    }
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
