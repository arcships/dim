//! Restart recovery through a real App Server: a record written before this
//! change in a non-terminal state is left as written, and a launch record's
//! stop timer resumes from `deadline_at` without launching or writing again.
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
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
    dim_agent_scheduler::install_with_testing(
        &mut builder,
        dim_agent_scheduler::SchedulerTexts::default(),
        dim_agent_scheduler::testing::TestingOptions {
            holds: Arc::new(dim_agent_scheduler::testing::Holds::default()),
            clock: dim_agent_scheduler::testing::clock(home),
            signals: dim_agent_scheduler::testing::signals(home),
        },
    )
    .unwrap();
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

/// Rewrites the run store as an earlier build would have left it: the edit
/// receives the inner run list and the file keeps its `{version, text}`
/// envelope. A missing store starts empty.
fn edit_runs(home: &Path, change: impl FnOnce(&mut Vec<Value>)) {
    let path = home.join("scheduler/runs.json");
    let mut inner: Value = match std::fs::read(&path) {
        Ok(bytes) => {
            let envelope: Value = serde_json::from_slice(&bytes).unwrap();
            serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap()
        }
        Err(_) => json!({"version": 1, "runs": []}),
    };
    change(inner["runs"].as_array_mut().unwrap());
    let envelope = json!({"version": 1, "text": inner.to_string()});
    std::fs::write(&path, envelope.to_string()).unwrap();
}

/// The stored run list, as the file holds it.
fn stored_runs(home: &Path) -> Vec<Value> {
    let envelope: Value =
        serde_json::from_slice(&std::fs::read(home.join("scheduler/runs.json")).unwrap()).unwrap();
    let inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    inner["runs"].as_array().unwrap().clone()
}

/// The states an earlier build wrote for a launch window (A.7).
const LEGACY_LAUNCH_STATES: [&str; 7] = [
    "launching",
    "submitted",
    "running",
    "completed",
    "cancelled",
    "timed_out",
    "skipped",
];

/// One launch record in the shape an earlier build wrote, field for field
/// (`engine.rs`'s `record()` and its later `update_run`s before this change):
/// every record carried `job_revision`, a launch fixed `deadline_at` when it
/// was accepted, the launch reply added the Session, its origin and its
/// policy together, and a terminal state carried `ended_at`. A `skipped`
/// window wrote neither a stop time nor a Session.
fn legacy_launch_record(run_id: &str, job_id: &str, state: &str) -> Value {
    let at = "2026-01-01T00:00:00.000Z";
    let launched = matches!(
        state,
        "submitted" | "running" | "completed" | "cancelled" | "timed_out"
    );
    let mut record = json!({
        "run_id": run_id,
        "job_id": job_id,
        "window": at,
        "state": state,
        "submission_id": format!("scheduler:{run_id}"),
        "created_at": at,
        "detail": if state == "skipped" { "overlap" } else { "" },
        "job_revision": 1,
    });
    if state != "skipped" {
        record["deadline_at"] = json!("2030-01-01T00:00:00.000Z");
    }
    if matches!(state, "completed" | "cancelled" | "timed_out" | "skipped") {
        record["ended_at"] = json!(at);
    }
    if launched {
        record["session_id"] = json!("legacy-session");
        record["session_origin"] = json!("launch");
        record["policy"] =
            json!({"preset": "read-only", "allowedTools": [], "disallowedTools": []});
        record["input_event_id"] = json!(format!("{run_id}-input"));
    }
    record
}

/// The states an earlier build wrote for a `session` target (A.7), and the
/// `detail` each carried. Its `delivering`/`delivered`/`failed`/`missed` are
/// the states both vocabularies name, so these are the records that reach
/// `validate_own` rather than `validate_legacy`.
const LEGACY_SESSION_RECORDS: [(&str, &str); 4] = [
    ("delivering", ""),
    ("delivered", ""),
    ("failed", "branch_missing"),
    ("missed", "missed"),
];

/// A `session` record in the shape an earlier build wrote, field for field
/// (`engine.rs`'s `record()`, `settle_delivered()` and `finish()` before this
/// change): the write named the Session and its branch with no stop time,
/// origin or policy, `settle_delivered()` added the receipt and `ended_at`,
/// and `finish()` added `ended_at` and `detail`. A `missed` window wrote
/// neither a Session nor a branch. Every record carried `job_revision`.
fn legacy_session_record(run_id: &str, job_id: &str, state: &str, detail: &str) -> Value {
    let at = "2026-01-01T00:00:00.000Z";
    let named = state != "missed";
    let mut record = json!({
        "run_id": run_id,
        "job_id": job_id,
        "window": at,
        "state": state,
        "submission_id": format!("scheduler:{run_id}"),
        "created_at": at,
        "detail": detail,
        "job_revision": 1,
    });
    if named {
        record["session_id"] = json!("legacy-session");
        record["branch_id"] = json!("main");
    }
    if state == "delivered" {
        record["input_event_id"] = json!(format!("{run_id}-input"));
    }
    if matches!(state, "delivered" | "failed" | "missed") {
        record["ended_at"] = json!(at);
    }
    record
}
/// A restart leaves an earlier build's records exactly as written, active and
/// terminal alike: nothing settles them, nothing is written again, and the
/// store still loads (A.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_leaves_earlier_build_records_as_written() {
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
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let seeded: Vec<Value> = LEGACY_LAUNCH_STATES
        .iter()
        .map(|state| legacy_launch_record(&format!("legacy-{state}"), &job_id, state))
        .collect();
    edit_runs(home.path(), |runs| runs.extend(seeded.clone()));

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let history: Value = client
        .typed::<Value>("schedule.history", json!({"job_id": job_id, "limit": 50}))
        .await
        .unwrap();
    let shown = history["runs"].as_array().unwrap();
    for state in LEGACY_LAUNCH_STATES {
        let run_id = format!("legacy-{state}");
        assert!(
            shown
                .iter()
                .any(|run| run["run_id"] == run_id.as_str() && run["state"] == state),
            "{state} is shown as written: {history}"
        );
    }
    assert_eq!(
        model.count(),
        0,
        "recovery launches nothing and submits no turn"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    assert_eq!(
        stored_runs(home.path()),
        seeded,
        "the records keep their bytes"
    );
}

/// A restart keeps an earlier build's settled `session` records as written.
/// These are the records whose states both vocabularies name, so they reach
/// the current invariants rather than the legacy ones; an answer of `accepted`
/// or no lookup at all leaves every byte alone.
///
/// `delivering` is covered separately: it is the one state a restart does act
/// on, and it acts the same way an earlier build did (A.6).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_leaves_earlier_build_session_records_as_written() {
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
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let settled: Vec<(&str, &str)> = LEGACY_SESSION_RECORDS
        .iter()
        .copied()
        .filter(|(state, _)| *state != "delivering")
        .collect();
    let seeded: Vec<Value> = settled
        .iter()
        .map(|(state, detail)| {
            legacy_session_record(&format!("legacy-{state}"), &job_id, state, detail)
        })
        .collect();
    edit_runs(home.path(), |runs| runs.extend(seeded.clone()));

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let history: Value = client
        .typed::<Value>("schedule.history", json!({"job_id": job_id, "limit": 50}))
        .await
        .unwrap();
    let shown = history["runs"].as_array().unwrap();
    for (state, _) in settled {
        let run_id = format!("legacy-{state}");
        assert!(
            shown
                .iter()
                .any(|run| run["run_id"] == run_id.as_str() && run["state"] == *state),
            "{state} is shown as written: {history}"
        );
    }
    assert_eq!(model.count(), 0, "recovery submits no turn");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    assert_eq!(
        stored_runs(home.path()),
        seeded,
        "the records keep their bytes"
    );
}

/// An earlier build's `session` record still in `delivering` is settled by
/// lookup, exactly as that build settled it (A.6): a target that is gone
/// answers `session_closed`, the record turns `failed` with that `detail`,
/// and the restart writes once. This is not history being rewritten — it is
/// the lookup the record was left waiting for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_settles_an_earlier_build_delivering_session_record() {
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
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let seeded = vec![legacy_session_record(
        "legacy-delivering",
        &job_id,
        "delivering",
        "",
    )];
    edit_runs(home.path(), |runs| runs.extend(seeded.clone()));

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let settled: Value = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id, "limit": 50}))
                .await
                .unwrap();
            let run = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == json!("legacy-delivering"))
                .cloned()
                .unwrap_or(Value::Null);
            if !run.is_null() && run["state"] != json!("delivering") {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the delivering record settles");
    assert_eq!(settled["state"], "failed", "{settled}");
    assert_eq!(settled["detail"], "session_closed", "{settled}");
    assert_eq!(
        settled["attempts"],
        json!(1),
        "the pass persists its attempt: {settled}"
    );
    assert_eq!(model.count(), 0, "recovery submits no turn");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A launch this design wrote keeps its stop timer across a restart whatever
/// state its delivery settled in: a record that settled `lost` or `failed`
/// after its launch still names the Session its stop time ends (A.7). The
/// timer runs from `deadline_at`, so the launched Session is stopped at that
/// time and its record keeps the state it settled in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_resumes_the_stop_timer_of_a_terminal_launch() {
    for state in ["lost", "failed"] {
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
                json!({"job": {"name": "slow", "schedule": {"at": "2030-01-01T00:00:00Z"},
                    "prompt": "Summarize the day.",
                    "target": {"kind": "launch", "cwd": home.path(), "max_run_minutes": 5}}}),
            )
            .await
            .unwrap();
        let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
        let accepted: Value = client
            .typed("schedule.run_now", json!({"job_id": job_id}))
            .await
            .unwrap();
        let run_id = accepted["run_id"].as_str().unwrap().to_owned();
        let delivered = await_state(&mut client, &job_id, &run_id, "delivered").await;
        let session_id = delivered["session_id"].as_str().unwrap().to_owned();
        drop(client);
        server.shutdown();
        server.join().await.unwrap();

        // The delivery settled `lost`/`failed` after the launch persisted its
        // Session, and the stop time is an hour out, as a restart that comes
        // back long before it would find it. Only an advance reaches it.
        let soon = (dim_agent_scheduler::testing::clock(home.path()).now()
            + chrono::Duration::seconds(3600))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        edit_runs(home.path(), |runs| {
            let run = runs
                .iter_mut()
                .find(|run| run["run_id"] == run_id.as_str())
                .unwrap();
            run["state"] = json!(state);
            run["detail"] = json!(if state == "lost" {
                "unknown"
            } else {
                "input_refused"
            });
            run["deadline_at"] = json!(soon);
            run.as_object_mut().unwrap().remove("input_event_id");
        });

        let parks = dim_agent_scheduler::testing::stop_parks(home.path());
        let server = start(home.path(), Arc::new(Script::default())).await;
        let mut client = connect(&server).await;
        // The timer parks on the hour and only the advance crosses it: no real
        // wait inside the row's bound can reach the stop time.
        dim_agent_scheduler::testing::stop_time(home.path(), parks).await;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let history = branch_events(&mut client, &session_id).await;
                if stopped_at_its_stop_time(&history) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the stop timer of a {state} launch runs"));
        let after = stored_runs(home.path())
            .into_iter()
            .find(|run| run["run_id"] == run_id.as_str())
            .unwrap();
        assert_eq!(
            after["state"],
            json!(state),
            "the stop is not a run outcome: {after}"
        );

        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// A restart's stop timer ends with its record: a launch record removed from
/// the store before its stop time is not stopped, because the timer runs only
/// "for as long as the record exists" (A.7). The same check precedes every
/// retry, so a stop that keeps failing cannot outlive its record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarts_stop_timer_ends_when_its_record_is_gone() {
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
            json!({"job": {"name": "removed", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Summarize the day.",
                "target": {"kind": "launch", "cwd": home.path(), "max_run_minutes": 5}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();
    let delivered = await_state(&mut client, &job_id, &run_id, "delivered").await;
    let session_id = delivered["session_id"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // An hour out, so only the advance after the record leaves reaches it.
    let soon = (dim_agent_scheduler::testing::clock(home.path()).now()
        + chrono::Duration::seconds(3600))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_runs(home.path(), |runs| {
        let run = runs
            .iter_mut()
            .find(|run| run["run_id"] == run_id.as_str())
            .unwrap();
        run["deadline_at"] = json!(soon);
    });

    let parks = dim_agent_scheduler::testing::stop_parks(home.path());
    let ended = dim_agent_scheduler::testing::stop_timers(home.path());
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    // The record leaves the store before its stop time, as a store a later
    // generation pruned at its own stop time would leave it.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            edit_runs(home.path(), |runs| {
                runs.retain(|run| run["run_id"] != run_id.as_str());
            });
            if !stored_runs(home.path())
                .iter()
                .any(|run| run["run_id"] == run_id.as_str())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the record leaves the store");
    // Only the advance crosses the hour; the timer then ends on its record
    // check, and the signal it publishes there is what this row waits on. The
    // check, not a bound, is what makes the negative claim fail when the check
    // is removed.
    dim_agent_scheduler::testing::stop_time(home.path(), parks).await;
    dim_agent_scheduler::testing::stop_ended(home.path(), ended).await;
    let history = branch_events(&mut client, &session_id).await;
    assert!(
        !stopped_at_its_stop_time(&history),
        "a record the store no longer holds is not stopped: {history:?}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Whether the stop timer's own interrupt is on the branch: the reason
/// `stop_at` sends, distinct from the one a closing branch writes.
fn stopped_at_its_stop_time(history: &[Value]) -> bool {
    history.iter().any(|event| {
        event["type"] == "stop"
            && event["name"] == "interrupt"
            && event["data"]["reason"] == "scheduled run reached its stop time"
    })
}

/// Every event of a Session's `main` branch, in order.
async fn branch_events(client: &mut Client, session_id: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let mut after = 0;
    let mut through = None;
    loop {
        let mut params = json!({"sessionId": session_id, "branchId": "main", "after": after});
        if let Some(through) = through {
            params["through"] = json!(through);
        }
        let page: dim_client::wire::History =
            client.typed("session.history", params).await.unwrap();
        through = Some(page.through);
        let next = page.next(after, through).unwrap();
        events.extend(page.events);
        match next {
            Some(next) => after = next,
            None => return events,
        }
    }
}

/// The job's run with `run_id`, once it reaches `state`.
async fn await_state(client: &mut Client, job_id: &str, run_id: &str, state: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id, "limit": 50}))
                .await
                .unwrap();
            if let Some(run) = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == run_id)
                && run["state"] == json!(state)
            {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the delivery reaches its state")
}

/// A launch delivery whose input was never confirmed is settled by lookup at
/// the next start, and its stop timer is not started for a Session no record
/// names (A.6, Appendix F).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_settles_an_unconfirmed_launch_by_lookup() {
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
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A launch whose reply never arrived: no Session, so no lookup can reach
    // it and its stop timer has nothing to stop.
    let at = "2026-01-01T00:00:00.000Z";
    edit_runs(home.path(), |runs| {
        runs.push(json!({
            "run_id": "unconfirmed",
            "job_id": job_id,
            "window": at,
            "state": "delivering",
            "deadline_at": "2030-01-01T00:00:00.000Z",
            "submission_id": "scheduler:unconfirmed",
            "created_at": at,
            "detail": "",
            "attempts": 0,
            "job_revision": 1,
        }));
    });

    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let settled: Value = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id, "limit": 50}))
                .await
                .unwrap();
            let run = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == json!("unconfirmed"))
                .cloned()
                .unwrap_or(Value::Null);
            if !run.is_null() && run["state"] != json!("delivering") {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the unconfirmed launch settles");
    assert_eq!(settled["state"], "lost", "{settled}");
    assert_eq!(settled["detail"], "launch_reply_unknown", "{settled}");
    assert_eq!(model.count(), 0, "nothing is launched again");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
