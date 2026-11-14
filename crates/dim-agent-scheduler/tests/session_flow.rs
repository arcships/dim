//! The `session` target end to end through a real App Server (design §3.1,
//! A.1, A.6): one input per window on the target branch, the delivery
//! vocabulary, bounded lookup settling, overlap skips, legacy mapping and
//! retention.
mod support;
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;

/// Every event of one branch, in order.
async fn branch_events(client: &mut dim_client::Client, session_id: &str) -> Vec<Value> {
    branch_events_on(client, session_id, "main").await
}

/// Every event of the named branch, in order.
async fn branch_events_on(
    client: &mut dim_client::Client,
    session_id: &str,
    branch_id: &str,
) -> Vec<Value> {
    let mut events = Vec::new();
    let mut after = 0;
    let mut through = None;
    loop {
        let mut params = json!({"sessionId": session_id, "branchId": branch_id, "after": after});
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
            None => break,
        }
    }
    events
}

/// The scheduler inputs on one branch: their submission id, mode and marker.
async fn scheduled_inputs(client: &mut dim_client::Client, session_id: &str) -> Vec<Value> {
    branch_events(client, session_id)
        .await
        .into_iter()
        .filter(|event| {
            event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["submission_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("scheduler:"))
        })
        .collect()
}

/// The job's delivery with `run_id`, once it is no longer `delivering`.
async fn settled(client: &mut dim_client::Client, job_id: &str, run_id: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
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
                && run["state"] != json!("delivering")
            {
                return run.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the delivery settles")
}

/// Saves a job whose target is `session_id`'s main branch.
async fn save_session_job(
    client: &mut dim_client::Client,
    name: &str,
    session_id: &str,
    input_mode: Option<&str>,
    schedule: Value,
) -> String {
    let mut target = json!({"kind": "session", "session_id": session_id, "branch_id": "main"});
    if let Some(mode) = input_mode {
        target["input_mode"] = json!(mode);
    }
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": name, "schedule": schedule,
                "prompt": format!("Work on {name}."), "target": target}}),
        )
        .await
        .unwrap();
    saved["job"]["id"].as_str().unwrap().to_owned()
}

/// A delivering record the tests seed into the store: the job's target
/// branch and a submission id nothing has written yet.
fn seeded_delivery(
    run_id: &str,
    job_id: &str,
    session_id: &str,
    submission_id: &str,
    attempts: Option<u32>,
) -> Value {
    let now = "2026-01-01T00:00:00.000Z";
    let mut record = json!({
        "run_id": run_id,
        "job_id": job_id,
        "window": "manual",
        "state": "delivering",
        "session_id": session_id,
        "branch_id": "main",
        "submission_id": submission_id,
        "created_at": now,
        "detail": "",
    });
    if let Some(attempts) = attempts {
        record["attempts"] = json!(attempts);
    }
    record
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_job_writes_one_input_to_its_branch_and_no_report() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let other = create_session(&mut client, home.path(), true).await;

    let mut runs = Vec::new();
    for (name, mode) in [
        ("queued", None),
        ("steered", Some("steer")),
        ("appended", Some("append")),
    ] {
        let job_id = save_session_job(
            &mut client,
            name,
            &session,
            mode,
            json!({"at": "2030-01-01T00:00:00Z"}),
        )
        .await;
        let accepted: Value = client
            .typed("schedule.run_now", json!({"job_id": job_id}))
            .await
            .unwrap();
        assert_eq!(accepted["state"], "delivering", "{accepted}");
        let run = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
        assert_eq!(run["state"], "delivered", "{run}");
        assert_eq!(run["session_id"], json!(session), "{run}");
        assert_eq!(run["branch_id"], "main", "{run}");
        assert_eq!(run["window"], "manual", "{run}");
        assert!(run["input_event_id"].is_string(), "{run}");
        assert!(run["session_origin"].is_null(), "{run}");
        assert!(run["policy"].is_null(), "{run}");
        assert!(run["report_to"].is_null(), "{run}");
        assert_eq!(run["detail"], "", "{run}");
        runs.push((job_id, run));
    }

    let inputs = scheduled_inputs(&mut client, &session).await;
    assert_eq!(inputs.len(), 3, "{inputs:?}");
    let modes: Vec<&str> = inputs
        .iter()
        .map(|input| input["data"]["mode"].as_str().unwrap())
        .collect();
    assert_eq!(modes, ["queue", "steer", "append"], "{inputs:?}");
    for (_, run) in &runs {
        let submission = run["submission_id"].as_str().unwrap();
        let matching: Vec<_> = inputs
            .iter()
            .filter(|input| input["data"]["submission_id"] == run["submission_id"])
            .collect();
        assert_eq!(matching.len(), 1, "one input for {submission}: {inputs:?}");
        assert_eq!(matching[0]["id"], run["input_event_id"], "{run}");
        assert!(
            matching[0]["data"]["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("dim-scheduler kind=\"scheduled-run\""))),
            "the marker is the first block: {matching:?}"
        );
    }
    assert!(
        !serde_json::to_string(&inputs)
            .unwrap()
            .contains("scheduler-report:"),
        "a session delivery sends no report"
    );
    let other_inputs = scheduled_inputs(&mut client, &other).await;
    assert!(
        other_inputs.is_empty(),
        "nothing is written to another Session: {other_inputs:?}"
    );
    let texts = model.texts();
    assert!(
        texts.contains("Work on queued."),
        "the saved prompt reaches the branch"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_past_window_is_missed_and_a_gone_target_fails_and_disables() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;

    let late = save_session_job(
        &mut client,
        "late",
        &session,
        None,
        json!({"at": "2020-01-01T00:00:00Z"}),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": late}))
                .await
                .unwrap();
            if !history["runs"].as_array().unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the past window is recorded");
    let history: Value = client
        .typed("schedule.history", json!({"job_id": late}))
        .await
        .unwrap();
    let run = &history["runs"][0];
    assert_eq!(run["state"], "missed", "{history}");
    assert_eq!(run["detail"], "missed", "{history}");
    assert_eq!(run["window"], "2020-01-01T00:00:00.000Z", "{history}");
    assert!(run["session_id"].is_null(), "{history}");
    assert!(
        scheduled_inputs(&mut client, &session).await.is_empty(),
        "a missed window writes no input"
    );

    let gone = save_session_job(
        &mut client,
        "gone",
        "no-such-session",
        None,
        json!({"every_minutes": 1}),
    )
    .await;
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": gone}))
        .await
        .unwrap();
    let run = settled(&mut client, &gone, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "failed", "{run}");
    assert_eq!(run["detail"], "session_closed", "{run}");
    let job = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
            let job = listed["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == gone.as_str())
                .unwrap()
                .clone();
            if job["enabled"] == json!(false) {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the gone target disables the job");
    assert_eq!(job["enabled"], false, "{job}");
    assert_eq!(job["disabled_reason"]["code"], "session_closed", "{job}");
    assert!(job["disabled_reason"]["at"].is_string(), "{job}");

    // Later windows do not fire while the job is disabled.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let history: Value = client
        .typed("schedule.history", json!({"job_id": gone}))
        .await
        .unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 1, "{history}");

    // A save clears the reason; enabling clears it too.
    let renamed: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"id": gone, "name": "gone again"}}),
        )
        .await
        .unwrap();
    assert!(renamed["job"]["disabled_reason"].is_null(), "{renamed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The lost-reply seam spends its one-shot flag on the next reply but rewrites
/// only an accepting one: a refusal the target gave reaches the match as it
/// came, so the delivery settles `failed` on it instead of taking a lookup
/// (A.5, A.6).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lost_reply_seam_passes_a_refusal_through() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let job = save_session_job(
        &mut client,
        "refused",
        "no-such-session",
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    support::holds(home.path()).lose_next_input_reply();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job}))
        .await
        .unwrap();
    let run = settled(&mut client, &job, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "failed", "{run}");
    assert_eq!(run["detail"], "session_closed", "{run}");
    assert!(
        run["attempts"].is_null(),
        "a refusal takes no lookup: {run}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lookup_settles_a_delivering_delivery_at_any_attempt() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job_id = save_session_job(
        &mut client,
        "nightly",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let delivered = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(delivered["state"], "delivered", "{delivered}");
    let run_id = delivered["run_id"].as_str().unwrap().to_owned();
    let input_id = delivered["input_event_id"].as_str().unwrap().to_owned();
    let submission = delivered["submission_id"].as_str().unwrap().to_owned();

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    // A crash left the record unsettled after its input was published and
    // three lookups had run.
    edit_runs(home.path(), |runs| {
        let run = runs
            .iter_mut()
            .find(|run| run["run_id"] == run_id.as_str())
            .unwrap();
        run["state"] = json!("delivering");
        run["attempts"] = json!(3);
        run.as_object_mut().unwrap().remove("ended_at");
        run.as_object_mut().unwrap().remove("input_event_id");
    });

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let resumed = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(resumed["state"], "delivered", "{resumed}");
    assert_eq!(resumed["input_event_id"], json!(input_id), "{resumed}");
    assert_eq!(resumed["attempts"], 4, "{resumed}");
    let inputs = scheduled_inputs(&mut client, &session).await;
    let matching: Vec<_> = inputs
        .iter()
        .filter(|input| input["data"]["submission_id"] == json!(submission))
        .collect();
    assert_eq!(matching.len(), 1, "no second input: {inputs:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_absent_attempts_field_reads_as_zero() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job_id = save_session_job(
        &mut client,
        "nightly",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let delivered = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
    let run_id = delivered["run_id"].as_str().unwrap().to_owned();
    let input_id = delivered["input_event_id"].as_str().unwrap().to_owned();

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        let run = runs
            .iter_mut()
            .find(|run| run["run_id"] == run_id.as_str())
            .unwrap();
        run["state"] = json!("delivering");
        run.as_object_mut().unwrap().remove("ended_at");
        run.as_object_mut().unwrap().remove("input_event_id");
        run.as_object_mut().unwrap().remove("attempts");
    });

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let resumed = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(resumed["state"], "delivered", "{resumed}");
    assert_eq!(resumed["input_event_id"], json!(input_id), "{resumed}");
    assert_eq!(resumed["attempts"], 1, "the first lookup is attempt one");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_keeps_the_delivery_unsettled_and_the_count_persists() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job_id = save_session_job(
        &mut client,
        "nightly",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;

    let run_id = "seed-unknown";
    let submission = format!("scheduler:{run_id}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        runs.push(seeded_delivery(
            run_id,
            &job_id,
            &session,
            &submission,
            None,
        ));
    });

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let after_unknown = wait_attempts(home.path(), run_id, 1).await;
    assert_eq!(after_unknown["state"], "delivering", "{after_unknown}");
    assert_eq!(after_unknown["attempts"], 1, "{after_unknown}");
    assert!(
        scheduled_inputs(&mut client, &session).await.is_empty(),
        "the delivery wrote nothing"
    );
    client
        .rpc("plugin.disable", json!({"name": "Scheduler"}))
        .await
        .unwrap();
    let refused = client
        .rpc("daemon.shutdown", json!({"ifIdle": true}))
        .await
        .unwrap_err();
    assert_eq!(
        dim_client::app_error(&refused),
        Some("daemon_busy"),
        "{refused}"
    );
    client
        .rpc("plugin.enable", json!({"name": "Scheduler"}))
        .await
        .unwrap();
    wait_attempts(home.path(), run_id, 2).await;

    // The answer carried no fact, so the record stays unsettled and a new
    // pass counts the next attempt.
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let client = support::connect(&server).await;
    let after_second = wait_attempts(home.path(), run_id, 3).await;
    assert_eq!(after_second["state"], "delivering", "{after_second}");
    assert_eq!(after_second["attempts"], 3, "{after_second}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Polls the store until a delivery's persisted attempts reach `attempts`.
async fn wait_attempts(home: &std::path::Path, run_id: &str, attempts: u32) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(run) = stored_runs(home)
                .into_iter()
                .find(|run| run["run_id"] == run_id)
                && run["attempts"] == json!(attempts)
            {
                return run;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the lookup pass persists the attempt")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fifth_answer_settles_lost_and_an_exhausted_budget_needs_no_lookup() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let fifth = save_session_job(
        &mut client,
        "fifth",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    let exhausted = save_session_job(
        &mut client,
        "exhausted",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        runs.push(seeded_delivery(
            "seed-fifth",
            &fifth,
            &session,
            "scheduler:seed-fifth",
            Some(4),
        ));
        runs.push(seeded_delivery(
            "seed-exhausted",
            &exhausted,
            &session,
            "scheduler:seed-exhausted",
            Some(5),
        ));
    });

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let run = settled(&mut client, &fifth, "seed-fifth").await;
    assert_eq!(run["state"], "lost", "{run}");
    assert_eq!(run["detail"], "unknown", "{run}");
    assert_eq!(run["attempts"], 5, "{run}");

    let run = settled(&mut client, &exhausted, "seed-exhausted").await;
    assert_eq!(run["state"], "lost", "{run}");
    assert_eq!(run["detail"], "lookup_budget_exhausted", "{run}");
    assert_eq!(run["attempts"], 5, "{run}");
    assert!(
        scheduled_inputs(&mut client, &session).await.is_empty(),
        "no window wrote an input"
    );

    client
        .rpc(
            "session.idle",
            json!({"sessionId": session, "branchId": "main"}),
        )
        .await
        .unwrap();
    client
        .rpc("daemon.shutdown", json!({"ifIdle": true}))
        .await
        .unwrap();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unsettled_delivery_skips_later_windows_and_run_now_records_its_own() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job_id = save_session_job(
        &mut client,
        "inbox",
        &session,
        None,
        json!({"every_minutes": 1}),
    )
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        runs.push(seeded_delivery(
            "seed-open",
            &job_id,
            &session,
            "scheduler:seed-open",
            None,
        ));
    });

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let open = wait_attempts(home.path(), "seed-open", 1).await;
    assert_eq!(open["state"], "delivering", "{open}");

    let mut skipped = Vec::new();
    for _ in 0..2 {
        let run: Value = client
            .typed("schedule.run_now", json!({"job_id": job_id}))
            .await
            .unwrap();
        assert_eq!(run["state"], "missed", "{run}");
        assert_eq!(run["detail"], "previous_delivery_unsettled", "{run}");
        assert_ne!(
            run["run_id"], "seed-open",
            "run_now records its own delivery"
        );
        skipped.push(run);
    }
    assert_ne!(skipped[0]["run_id"], skipped[1]["run_id"], "{skipped:?}");
    let open = wait_attempts(home.path(), "seed-open", 1).await;
    assert_eq!(
        open["state"], "delivering",
        "the seed stays unsettled: {open}"
    );
    assert!(
        scheduled_inputs(&mut client, &session).await.is_empty(),
        "a skipped window writes no input"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_keeps_an_unsettled_delivery_and_the_newest_hundred_terminal_ones() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job_id = save_session_job(
        &mut client,
        "nightly",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        for index in 0..102 {
            let run_id = format!("old-{index:03}");
            let at = format!("2026-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60);
            runs.push(json!({
                "run_id": run_id,
                "job_id": job_id,
                "window": "manual",
                "state": "delivered",
                "session_id": session,
                "branch_id": "main",
                "input_event_id": format!("input-{index}"),
                "submission_id": format!("scheduler:old-{index:03}"),
                "created_at": at,
                "ended_at": at,
                "detail": "",
            }));
        }
        runs.push(seeded_delivery(
            "seed-open",
            &job_id,
            &session,
            "scheduler:seed-open",
            None,
        ));
    });

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    wait_attempts(home.path(), "seed-open", 1).await;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let stored = stored_runs(home.path());
            if stored
                .iter()
                .filter(|run| run["job_id"] == job_id.as_str())
                .count()
                == 101
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("retention leaves the newest hundred terminal deliveries and the unsettled one");
    let history: Value = client
        .typed("schedule.history", json!({"job_id": job_id, "limit": 50}))
        .await
        .unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 50, "{history}");
    let stored = stored_runs(home.path());
    assert!(
        stored.iter().any(|run| run["run_id"] == json!("seed-open")),
        "the delivering record is kept: {stored:?}"
    );
    let terminal = stored
        .iter()
        .filter(|run| run["job_id"] == job_id.as_str() && run["state"] == json!("delivered"))
        .count();
    assert_eq!(terminal, 100, "the newest hundred terminal deliveries stay");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The stored jobs document is main's own serialization: its required
/// fields, `max_run_minutes` always written, `mode` create or reuse, and the
/// optional fields main omits when empty, for both origins (zerob13). It
/// loads without a migration, runs from its mapped targets and keeps those
/// bytes on a save.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_jobs_map_to_their_targets_and_keep_their_bytes() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let first = create_session(&mut client, home.path(), true).await;
    let second = create_session(&mut client, home.path(), true).await;
    // A delivery needs the job to exist before the document is replaced.
    save_session_job(
        &mut client,
        "seed",
        &first,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let policy_cap = json!({"preset": "read-only", "allowedTools": [], "disallowedTools": []});
    let job = |id: &str, created_by: Value, mode: &str, session: Option<&str>| {
        let mut job = json!({
            "id": id,
            "name": id,
            "schedule": {"at": "2030-01-01T00:00:00Z"},
            "prompt": format!("Work on {id}."),
            "cwd": home.path(),
            "mode": mode,
            "enabled": true,
            "enabled_at": "2026-01-01T00:00:00.000Z",
            "max_run_minutes": 60,
            "revision": 1,
            "created_by": created_by,
            "policy_cap": policy_cap.clone(),
        });
        if let Some(session) = session {
            job["session_id"] = json!(session);
        }
        job
    };
    let legacy = json!({"max_concurrent_runs": 4, "jobs": [
        job("tool-create", json!({"tool": {"session_id": first, "branch_id": "main"}}), "create", None),
        job("tool-reuse", json!({"tool": {"session_id": first, "branch_id": "main"}}), "reuse", None),
        job("user-create", json!("user"), "create", None),
        job("user-reuse", json!("user"), "reuse", Some(&second)),
        job("user-reuse-none", json!("user"), "reuse", None),
    ]});
    rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .execute(
            "UPDATE settings_documents SET document = ?1 WHERE source = 'user/schedule'",
            [legacy.to_string()],
        )
        .unwrap();

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let target = |id: &str| {
        listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .unwrap()["target"]
            .clone()
    };
    assert_eq!(
        target("tool-create"),
        json!({"kind": "session", "session_id": first, "branch_id": "main",
            "input_mode": "queue"}),
        "{listed}"
    );
    assert_eq!(target("tool-reuse"), target("tool-create"), "{listed}");
    assert_eq!(target("user-create")["kind"], "launch", "{listed}");
    assert_eq!(
        target("user-reuse"),
        json!({"kind": "session", "session_id": second, "branch_id": "main",
            "input_mode": "queue"}),
        "{listed}"
    );
    assert_eq!(target("user-reuse-none")["kind"], "launch", "{listed}");

    for (id, session) in [("tool-create", &first), ("user-reuse", &second)] {
        let job_id = listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let accepted: Value = client
            .typed("schedule.run_now", json!({"job_id": job_id}))
            .await
            .unwrap();
        let run = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
        assert_eq!(run["state"], "delivered", "{id}: {run}");
        assert_eq!(run["session_id"], json!(session), "{id}: {run}");
        assert_eq!(run["branch_id"], "main", "{id}: {run}");
        let inputs = scheduled_inputs(&mut client, session).await;
        assert!(
            inputs
                .iter()
                .any(|input| input["data"]["submission_id"] == run["submission_id"]),
            "{id}: the delivery wrote its branch: {inputs:?}"
        );
    }
    let launch_id = listed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == "user-create")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": launch_id}))
        .await
        .unwrap();
    let run = settled(
        &mut client,
        &launch_id,
        accepted["run_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(run["state"], "delivered", "{run}");
    assert_eq!(run["session_origin"], "launch", "{run}");

    // A save preserves the legacy fields and leaves the stored target absent.
    let tool_id = listed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == "tool-create")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": tool_id, "name": "tool-create renamed"}}),
        )
        .await
        .unwrap();
    let document: String = rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .query_row(
            "SELECT document FROM settings_documents WHERE source = 'user/schedule'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let document: Value = serde_json::from_str(&document).unwrap();
    let stored = document["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == tool_id.as_str())
        .unwrap();
    assert_eq!(stored["name"], "tool-create renamed", "{stored}");
    assert!(stored.get("target").is_none(), "{stored}");
    assert_eq!(stored["mode"], "create", "{stored}");
    assert_eq!(stored["cwd"], json!(home.path()), "{stored}");
    assert_eq!(stored["policy_cap"], policy_cap, "{stored}");

    // A reused Session survives its job's deletion; a launched Session goes.
    let reused_job = listed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == "user-reuse")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    client
        .typed::<Value>("schedule.delete", json!({"job_id": reused_job}))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let catalog = client
        .rpc(
            "session.catalog",
            json!({"withInternal": true, "limit": 100}),
        )
        .await
        .unwrap();
    assert!(
        catalog.to_string().contains(&second),
        "a Session the job only reused survives: {catalog}"
    );
    let launched = run["session_id"].as_str().unwrap().to_owned();
    // A launch record's stop time bounds when its Session may be deleted
    // (A.7): move it into the past, as time would.
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        let run = runs
            .iter_mut()
            .find(|run| run["session_id"] == json!(launched))
            .unwrap();
        run["deadline_at"] = json!("2020-01-01T00:00:00.000Z");
    });
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    client
        .typed::<Value>("schedule.delete", json!({"job_id": launch_id}))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let catalog = client
                .rpc(
                    "session.catalog",
                    json!({"withInternal": true, "limit": 100}),
                )
                .await
                .unwrap();
            if !catalog.to_string().contains(&launched) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a launched Session goes with its job");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// One delivery record in the shape the Scheduler wrote before this change
/// (A.7): the old state names, with the fields each state carried. A
/// `reuse` origin keeps the records out of launch ownership, which A.7's
/// session retention tests cover separately.
fn legacy_delivery_record(run_id: &str, job_id: &str, state: &str) -> Value {
    let at = "2026-01-01T00:00:00.000Z";
    let mut record = json!({
        "run_id": run_id,
        "job_id": job_id,
        "window": at,
        "state": state,
        "submission_id": format!("scheduler:{run_id}"),
        "created_at": at,
        "detail": "legacy",
    });
    if matches!(
        state,
        "skipped" | "completed" | "cancelled" | "timed_out" | "lost"
    ) {
        record["ended_at"] = json!(at);
    }
    if state != "skipped" {
        record["deadline_at"] = json!(at);
    }
    if matches!(
        state,
        "submitted" | "running" | "completed" | "cancelled" | "timed_out"
    ) {
        record["session_id"] = json!("legacy-session");
        record["session_origin"] = json!("reuse");
        record["policy"] =
            json!({"preset": "read-only", "allowedTools": [], "disallowedTools": []});
        record["input_event_id"] = json!(format!("{run_id}-input"));
    }
    record
}

/// The eight state names this change decodes from earlier builds.
const LEGACY_STATES: [&str; 8] = [
    "skipped",
    "submitted",
    "running",
    "completed",
    "cancelled",
    "timed_out",
    "lost",
    "launching",
];

/// The delivery file's inner text, as the store holds it.
fn runs_text(home: &std::path::Path) -> String {
    let envelope: Value =
        serde_json::from_slice(&std::fs::read(home.join("scheduler/runs.json")).unwrap()).unwrap();
    envelope["text"].as_str().unwrap().to_owned()
}

/// A delivery file written by an earlier build loads with the state names it
/// wrote, shows them in `schedule.history`, and is not rewritten by reading
/// or retention (A.7). The records are seeded into a running server: the
/// retained launch recovery only reads the file at start, and this test is
/// about the stored shapes, not about that recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_deliveries_load_show_in_history_and_keep_their_bytes() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let job_id = save_session_job(
        &mut client,
        "legacy-deliveries",
        "00000000-0000-7000-8000-000000000001",
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;

    edit_runs(home.path(), |runs| {
        for state in LEGACY_STATES {
            runs.push(legacy_delivery_record(
                &format!("legacy-{state}"),
                &job_id,
                state,
            ));
        }
    });
    let seeded = runs_text(home.path());

    let history: Value = client
        .typed::<Value>("schedule.history", json!({"job_id": job_id, "limit": 50}))
        .await
        .unwrap();
    let shown = history["runs"].as_array().unwrap();
    for state in LEGACY_STATES {
        let run_id = format!("legacy-{state}");
        assert!(
            shown
                .iter()
                .any(|run| run["run_id"] == run_id.as_str() && run["state"] == state),
            "{state} is shown: {history}"
        );
    }
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    assert_eq!(
        seeded,
        runs_text(home.path()),
        "the records keep their bytes"
    );
}

/// Retention keeps the newest 100 terminal deliveries per existing job; a
/// non-terminal record from an earlier build is retained as written and is
/// never selected by that rule (A.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_non_terminal_deliveries_survive_retention_as_written() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let job_id = save_session_job(
        &mut client,
        "legacy-retention",
        "00000000-0000-7000-8000-000000000002",
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    let dummy = save_session_job(
        &mut client,
        "legacy-retention-dummy",
        "00000000-0000-7000-8000-000000000003",
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;

    edit_runs(home.path(), |runs| {
        for state in LEGACY_STATES {
            runs.push(legacy_delivery_record(
                &format!("legacy-{state}"),
                &job_id,
                state,
            ));
        }
        for index in 0..100 {
            let ended = format!("2026-03-01T00:{:02}:{:02}.000Z", index / 60, index % 60);
            runs.push(json!({
                "run_id": format!("delivered-{index}"),
                "job_id": job_id,
                "window": "manual",
                "state": "delivered",
                "session_id": "legacy-session",
                "branch_id": "main",
                "input_event_id": format!("delivered-{index}-input"),
                "submission_id": format!("scheduler:delivered-{index}"),
                "created_at": ended,
                "ended_at": ended,
                "detail": "",
            }));
        }
    });

    // Deleting the dummy job runs a retention pass (A.7).
    client
        .typed::<Value>("schedule.delete", json!({"job_id": dummy}))
        .await
        .unwrap();
    wait_for("the retention pass", || {
        !stored_runs(home.path()).iter().any(|run| {
            matches!(
                run["state"].as_str(),
                Some("skipped" | "completed" | "cancelled" | "timed_out" | "lost")
            )
        })
    })
    .await;
    let runs = stored_runs(home.path());
    assert_eq!(
        runs.iter()
            .filter(|run| run["state"] == "delivered")
            .count(),
        100,
        "the newest hundred terminal deliveries are kept"
    );
    for state in ["submitted", "running", "launching"] {
        let run_id = format!("legacy-{state}");
        let run = runs
            .iter()
            .find(|run| run["run_id"] == run_id.as_str())
            .unwrap_or_else(|| panic!("{state} is never pruned: {runs:?}"));
        assert_eq!(
            run,
            &legacy_delivery_record(&run_id, &job_id, state),
            "{state} is retained as written"
        );
    }
    assert_eq!(runs.len(), 103, "{runs:?}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    client
        .rpc("plugin.disable", json!({"name": "Scheduler"}))
        .await
        .unwrap();
    client
        .rpc("daemon.shutdown", json!({"ifIdle": true}))
        .await
        .unwrap();
    server.join().await.unwrap();
}

/// A job's explicit target is the whole truth: a legacy top-level policy and
/// Agent do not come back once a save replaces the target (zerob13
/// 4511587383). The host allows full access, so a stale full-access request
/// would reach the Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_target_ignores_the_legacy_top_level_fields() {
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset};

    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let seed = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "seed",
        &seed,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let policy_cap = json!({"preset": "full-access", "allowedTools": [], "disallowedTools": []});
    let job = |id: &str, agent: Option<&str>| {
        let mut job = json!({
            "id": id,
            "name": id,
            "schedule": {"at": "2030-01-01T00:00:00Z"},
            "prompt": format!("Work on {id}."),
            "cwd": home.path(),
            "mode": "create",
            "policy": "full-access",
            "enabled": true,
            "enabled_at": "2026-01-01T00:00:00.000Z",
            "max_run_minutes": 60,
            "revision": 1,
            "created_by": "user",
            "policy_cap": policy_cap.clone(),
        });
        if let Some(agent) = agent {
            job["agent_id"] = json!(agent);
        }
        job
    };
    let legacy = json!({"max_concurrent_runs": 4, "jobs": [
        job("policy-job", None),
        job("agent-job", Some("00000000-0000-7000-8000-0000000000ff")),
    ]});
    rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .execute(
            "UPDATE settings_documents SET document = ?1 WHERE source = 'user/schedule'",
            [legacy.to_string()],
        )
        .unwrap();

    let server = start_with(
        home.path(),
        Arc::new(Script::default()),
        SessionConfig {
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let mut client = support::connect(&server).await;
    for id in ["policy-job", "agent-job"] {
        client
            .typed::<Value>(
                "schedule.save",
                json!({"job": {"id": id,
                    "target": {"kind": "launch", "cwd": home.path(), "max_run_minutes": 60}}}),
            )
            .await
            .unwrap();
    }
    for id in ["policy-job", "agent-job"] {
        let accepted: Value = client
            .typed("schedule.run_now", json!({"job_id": id}))
            .await
            .unwrap();
        let run = settled(&mut client, id, accepted["run_id"].as_str().unwrap()).await;
        // The stale Agent would make the launch fail; the stale policy would
        // be the effect the Session received.
        assert_eq!(run["state"], "delivered", "{id}: {run}");
        assert_eq!(run["policy"]["preset"], "read-only", "{id}: {run}");
    }

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Seeds a legacy `reuse` job bound to `launched`, with one terminal
/// launch-origin delivery. The server must be stopped.
fn seed_legacy_reuse_job(home: &std::path::Path, launched: &str) {
    let policy = json!({"preset": "read-only", "allowedTools": [], "disallowedTools": []});
    let legacy = json!({"max_concurrent_runs": 4, "jobs": [{
        "id": "reuse",
        "name": "reuse",
        "schedule": {"at": "2030-01-01T00:00:00Z"},
        "prompt": "Work.",
        "cwd": home,
        "mode": "reuse",
        "session_id": launched,
        "enabled": true,
        "enabled_at": "2026-01-01T00:00:00.000Z",
        "revision": 1,
        "created_by": "user",
        "policy_cap": policy.clone(),
    }]});
    rusqlite::Connection::open(home.join("dimcode.sqlite"))
        .unwrap()
        .execute(
            "UPDATE settings_documents SET document = ?1 WHERE source = 'user/schedule'",
            [legacy.to_string()],
        )
        .unwrap();
    edit_runs(home, |runs| {
        runs.push(json!({
            "run_id": "run-1",
            "job_id": "reuse",
            "window": "2026-01-01T00:00:00.000Z",
            "state": "completed",
            "deadline_at": "2026-01-01T01:00:00.000Z",
            "session_id": launched,
            "session_origin": "launch",
            "policy": policy,
            "input_event_id": "input-1",
            "submission_id": "scheduler:run-1",
            "report_to": null,
            "created_at": "2026-01-01T00:00:00.000Z",
            "ended_at": "2026-01-01T00:01:00.000Z",
            "detail": "",
            "attempts": null,
        }));
    });
}

/// The deliveries `schedule.history` shows, as `(run_id, state)` pairs.
fn shown_deliveries(history: &Value) -> Vec<(String, String)> {
    history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| {
            (
                run["run_id"].as_str().unwrap().to_owned(),
                run["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// A save that replaces a legacy `reuse` job's target releases the Session
/// that binding launched: the deliveries keep their place and only the
/// launch ownership is retired, so deleting the retargeted job keeps the
/// Session (zerob13 4511587391, 4512306141; Simon, eric).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_save_that_replaces_a_legacy_reuse_target_releases_its_session() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let launched = create_session(&mut client, home.path(), true).await;
    let retarget = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "seed",
        &launched,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    seed_legacy_reuse_job(home.path(), &launched);

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    // The retarget keeps every delivery: only the ownership changes.
    let before: Value = client
        .typed("schedule.history", json!({"job_id": "reuse", "limit": 50}))
        .await
        .unwrap();
    let deliveries = shown_deliveries(&before);
    assert_eq!(
        deliveries,
        vec![("run-1".to_owned(), "completed".to_owned())],
        "{before}"
    );
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse",
                "target": {"kind": "session", "session_id": retarget, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    let after: Value = client
        .typed("schedule.history", json!({"job_id": "reuse", "limit": 50}))
        .await
        .unwrap();
    assert_eq!(
        shown_deliveries(&after),
        deliveries,
        "the retarget keeps every delivery: {after}"
    );
    let shown = after["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run_id"] == "run-1")
        .unwrap();
    assert_eq!(
        shown["session_origin"], "launch",
        "the record keeps the origin it recorded: {shown}"
    );
    assert_eq!(
        shown["released"], true,
        "the record is marked released: {shown}"
    );
    let runs = stored_runs(home.path());
    let released = runs.iter().find(|run| run["run_id"] == "run-1").unwrap();
    assert_eq!(
        released["session_origin"], "launch",
        "the origin stays as recorded: {released}"
    );
    assert_eq!(
        released["released"], true,
        "the stored record is marked released: {released}"
    );
    // The compatibility fields keep their bytes through the retarget.
    let document: String = rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .query_row(
            "SELECT document FROM settings_documents WHERE source = 'user/schedule'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let document: Value = serde_json::from_str(&document).unwrap();
    let stored = &document["jobs"][0];
    assert_eq!(stored["mode"], "reuse", "{stored}");
    assert_eq!(stored["session_id"], json!(launched), "{stored}");
    assert_eq!(stored["target"]["kind"], "session", "{stored}");

    client
        .typed::<Value>("schedule.delete", json!({"job_id": "reuse"}))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let catalog = client
        .rpc(
            "session.catalog",
            json!({"withInternal": true, "limit": 100}),
        )
        .await
        .unwrap();
    assert!(
        catalog.to_string().contains(&launched),
        "the released Session survives the job's deletion: {catalog}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A save whose own release write fails still reports the save; the next
/// retention pass re-derives the release from the document (A.7; eric's
/// ordering point).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_release_heals_at_the_next_retention_pass() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let launched = create_session(&mut client, home.path(), true).await;
    let retarget = create_session(&mut client, home.path(), true).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    seed_legacy_reuse_job(home.path(), &launched);

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    // A second job whose deletion runs a retention pass later.
    let dummy = save_session_job(
        &mut client,
        "dummy",
        "00000000-0000-7000-8000-0000000000ee",
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    // The release write cannot decode while the store is broken; the save
    // still lands and the reply reports it.
    let valid = std::fs::read(home.path().join("scheduler/runs.json")).unwrap();
    std::fs::write(
        home.path().join("scheduler/runs.json"),
        br#"{"version":1,"text":"not json"}"#,
    )
    .unwrap();
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse",
                "target": {"kind": "session", "session_id": retarget, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    std::fs::write(home.path().join("scheduler/runs.json"), &valid).unwrap();
    let runs = stored_runs(home.path());
    let still_owned = runs.iter().find(|run| run["run_id"] == "run-1").unwrap();
    assert_eq!(still_owned["session_origin"], "launch", "{still_owned}");
    assert!(
        still_owned.get("released").is_none(),
        "the failed release leaves the record unreleased: {still_owned}"
    );

    // Deleting the dummy job runs a retention pass, which re-derives the
    // release from the saved document.
    client
        .typed::<Value>("schedule.delete", json!({"job_id": dummy}))
        .await
        .unwrap();
    wait_for("the retention pass releases the record", || {
        stored_runs(home.path())
            .iter()
            .any(|run| run["run_id"] == "run-1" && run["released"] == true)
    })
    .await;

    // The released Session survives the job's deletion.
    client
        .typed::<Value>("schedule.delete", json!({"job_id": "reuse"}))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let catalog = client
        .rpc(
            "session.catalog",
            json!({"withInternal": true, "limit": 100}),
        )
        .await
        .unwrap();
    assert!(
        catalog.to_string().contains(&launched),
        "the released Session survives the job's deletion: {catalog}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A due `session` window writes the branch its target names, not only
/// `main` (Appendix D item 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_target_writes_a_non_main_branch() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session =
        create_session_with(&mut client, home.path(), &["schedule", "branch"], true).await;
    let agent = server.agent().expect("the default Agent");
    let at = agent
        .get_session(&session)
        .unwrap()
        .sequence("main")
        .unwrap();
    let branch = client
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
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "side", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Work on the side.",
                "target": {"kind": "session", "session_id": session, "branch_id": branch}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "delivered", "{run}");
    assert_eq!(run["branch_id"], json!(branch), "{run}");
    let inputs = branch_events_on(&mut client, &session, &branch).await;
    assert!(
        inputs.iter().any(|event| {
            event["type"] == "input" && event["data"]["submission_id"] == run["submission_id"]
        }),
        "the target branch takes the delivery: {inputs:?}"
    );
    let main = scheduled_inputs(&mut client, &session).await;
    assert!(main.is_empty(), "main takes nothing: {main:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A target-gone disable whose write loses or fails is still required state:
/// the conflict retry and the timer's claim scan both recover it (zerob13
/// 4511587398, eric).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_target_gone_disable_is_retried() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let gone = "00000000-0000-7000-8000-0000000000aa";
    let first = client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"name": "gone", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Work.", "target": {"kind": "session", "session_id": gone}}}),
        )
        .await
        .unwrap()["job"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"name": "gone too", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Work.", "target": {"kind": "session", "session_id": gone}}}),
        )
        .await
        .unwrap()["job"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A conflicting write: hold the settings revision, let the delivery's
    // read see it, then commit the bump so the CAS loses.
    let lock = rusqlite::Connection::open(home.path().join("dimcode.sqlite")).unwrap();
    lock.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    lock.execute(
        "UPDATE settings_documents SET version = version + 1 WHERE source = 'user/schedule'",
        [],
    )
    .unwrap();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": first}))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    lock.execute_batch("COMMIT").unwrap();
    drop(lock);
    let run = settled(&mut client, &first, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "failed", "{run}");
    let disabled = wait_for_job(&mut client, &first).await;
    assert_eq!(disabled["enabled"], false, "{disabled}");
    assert_eq!(
        disabled["disabled_reason"]["code"], "session_closed",
        "{disabled}"
    );

    // A failed write: the settings update is refused until the failure is
    // lifted, and the timer's claim scan then disables the job.
    let trigger = rusqlite::Connection::open(home.path().join("dimcode.sqlite")).unwrap();
    trigger
        .execute_batch(
            "CREATE TRIGGER \"fail-disable\" BEFORE UPDATE ON settings_documents \
             WHEN OLD.source = 'user/schedule' BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .unwrap();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": second}))
        .await
        .unwrap();
    let run = settled(&mut client, &second, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "failed", "{run}");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let still = &listed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == second.as_str())
        .unwrap();
    assert_eq!(
        still["enabled"], true,
        "the refused write leaves the job enabled: {still}"
    );
    trigger
        .execute_batch("DROP TRIGGER \"fail-disable\"")
        .unwrap();
    let disabled = wait_for_job(&mut client, &second).await;
    assert_eq!(disabled["enabled"], false, "{disabled}");
    assert_eq!(
        disabled["disabled_reason"]["code"], "session_closed",
        "{disabled}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The job as `schedule.list` reports it, once it reads disabled.
async fn wait_for_job(client: &mut dim_client::Client, job_id: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
            let job = listed["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == job_id)
                .unwrap()
                .clone();
            if job["enabled"] == json!(false) && !job["disabled_reason"].is_null() {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the job is disabled with its reason")
}

/// An uncertain input reply looks up only its own delivery: another job's
/// `delivering` record keeps its persisted attempts (zerob13 4511587407).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uncertain_write_looks_up_only_its_own_delivery() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let earlier = create_session(&mut client, home.path(), true).await;
    let other = create_session(&mut client, home.path(), true).await;
    let earlier_job = save_session_job(
        &mut client,
        "earlier",
        &earlier,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    let other_job = save_session_job(
        &mut client,
        "other",
        &other,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A write that meets the owner's unload is answered with an uncertain
    // reply, which carries no fact about the write (A.5, A.6): while the
    // Scheduler unloads, the delivery task keeps running, and its input outlives
    // the Sessions index. `other`'s run then takes its own lookup (attempt
    // one) while `earlier`'s record is left to the next generation's
    // once-per-pass settling — which this test never starts.
    let mut observed = None;
    for _ in 0..4 {
        edit_runs(home.path(), |runs| {
            runs.retain(|run| run["run_id"] != "earlier-run");
            runs.push(json!({
                "run_id": "earlier-run",
                "job_id": earlier_job,
                "window": "manual",
                "state": "delivering",
                "session_id": earlier,
                "branch_id": "main",
                "submission_id": "scheduler:earlier-run",
                "created_at": "2026-01-01T00:00:00.000Z",
                "detail": "",
                "attempts": 0,
            }));
        });
        let server = start(home.path(), Arc::new(Script::default())).await;
        let mut client = support::connect(&server).await;
        // The start's own recovery pass settles every `delivering` record
        // once, so `earlier` starts at attempt one.
        wait_for("the startup settling pass", || {
            stored_runs(home.path())
                .iter()
                .any(|run| run["run_id"] == "earlier-run" && run["attempts"] == json!(1))
        })
        .await;
        let run: Value = client
            .typed("schedule.run_now", json!({"job_id": other_job}))
            .await
            .unwrap();
        let run_id = run["run_id"].as_str().unwrap().to_owned();
        server.shutdown();
        server.join().await.unwrap();
        let runs = stored_runs(home.path());
        let own = runs
            .iter()
            .find(|run| run["run_id"] == run_id.as_str())
            .expect("the manual window is recorded");
        if own["state"] == "delivering" && own["attempts"] == json!(1) {
            let own = own.clone();
            observed = Some((runs, own));
            break;
        }
        // The unload closed before the write reached the Sessions service,
        // so the reply was definite; take the next unload.
    }
    let (runs, own) = observed.expect("a write answered with the uncertain reply");
    println!("the uncertain delivery: {own}");
    let earlier = runs
        .iter()
        .find(|run| run["run_id"] == "earlier-run")
        .unwrap();
    assert_eq!(
        earlier["attempts"], 1,
        "the earlier delivery keeps its own budget: {earlier}"
    );
    assert_eq!(earlier["state"], "delivering", "{earlier}");
}

/// A target-gone claim belongs to the enable that recorded it: a successful
/// re-enable or save does not replay it (zerob13 4512306129, Simon).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_re_enable_does_not_replay_the_old_target_gone_claim() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let re_enabled = create_session(&mut client, home.path(), true).await;
    let saved = create_session(&mut client, home.path(), true).await;
    let re_enabled_job = save_session_job(
        &mut client,
        "re-enable",
        &re_enabled,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    let saved_job = save_session_job(
        &mut client,
        "save-enable",
        &saved,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    // Both targets answer target-gone: their Sessions are closed, and a
    // closed Session with rows is restorable.
    for session in [&re_enabled, &saved] {
        client
            .rpc("session.close", json!({"sessionId": session}))
            .await
            .unwrap();
    }
    for job in [&re_enabled_job, &saved_job] {
        let accepted: Value = client
            .typed("schedule.run_now", json!({"job_id": job}))
            .await
            .unwrap();
        let run = settled(&mut client, job, accepted["run_id"].as_str().unwrap()).await;
        assert_eq!(run["state"], "failed", "{run}");
        assert_eq!(run["detail"], "session_closed", "{run}");
        wait_for_job(&mut client, job).await;
    }
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A fresh generation restores the closed Sessions on their next use.
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let listed_job = |listed: &Value, id: &str| {
        listed["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .unwrap()
            .clone()
    };
    client
        .typed::<Value>(
            "schedule.set_enabled",
            json!({"job_id": re_enabled_job, "enabled": true}),
        )
        .await
        .unwrap();
    // The timer pass must not read the old claim back once the enable landed.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let job = listed_job(&listed, &re_enabled_job);
    assert_eq!(job["enabled"], true, "{listed}");
    assert!(job["disabled_reason"].is_null(), "{listed}");
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": re_enabled_job}))
        .await
        .unwrap();
    let run = settled(
        &mut client,
        &re_enabled_job,
        accepted["run_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(run["state"], "delivered", "{run}");

    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": saved_job, "enabled": true}}),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let job = listed_job(&listed, &saved_job);
    assert_eq!(job["enabled"], true, "{listed}");
    assert!(job["disabled_reason"].is_null(), "{listed}");
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": saved_job}))
        .await
        .unwrap();
    let run = settled(
        &mut client,
        &saved_job,
        accepted["run_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(run["state"], "delivered", "{run}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A lookup that answers target-gone settles the delivery `failed` and
/// disables the job, the same rule as a direct write (Simon).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_target_gone_lookup_settles_failed_and_disables() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let missing = "00000000-0000-7000-8000-0000000000dd";
    let job = save_session_job(
        &mut client,
        "lookup-gone",
        missing,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    edit_runs(home.path(), |runs| {
        let mut record = seeded_delivery("gone-run", &job, missing, "scheduler:gone-run", None);
        // The delivery was due under the job's first revision.
        record["job_revision"] = json!(1);
        runs.push(record);
    });
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    wait_for("the lookup settles the delivery", || {
        stored_runs(home.path())
            .iter()
            .any(|run| run["run_id"] == "gone-run" && run["state"] == "failed")
    })
    .await;
    let runs = stored_runs(home.path());
    let run = runs.iter().find(|run| run["run_id"] == "gone-run").unwrap();
    assert_eq!(run["detail"], "session_closed", "{run}");
    let job = wait_for_job(&mut client, &job).await;
    assert_eq!(job["disabled_reason"]["code"], "session_closed", "{job}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The job `schedule.list` reports for `job_id`.
fn listed_job_value(listed: &Value, job_id: &str) -> Value {
    listed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == job_id)
        .unwrap()
        .clone()
}

/// The stored `schedule` settings document.
fn stored_document(home: &std::path::Path) -> Value {
    let document: String = rusqlite::Connection::open(home.join("dimcode.sqlite"))
        .unwrap()
        .query_row(
            "SELECT document FROM settings_documents WHERE source = 'user/schedule'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&document).unwrap()
}

/// A same-target save that commits after the target-gone failure and before
/// the disable lands supersedes the claim: the disable re-checks on its CAS
/// retry, leaves the job enabled, and the next window delivers (A.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_target_save_supersedes_the_target_gone_claim() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job = save_session_job(
        &mut client,
        "same-target",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    client
        .rpc("session.close", json!({"sessionId": session}))
        .await
        .unwrap();
    let hold = support::holds(home.path()).hold_next_disable();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job}))
        .await
        .unwrap();
    let run = settled(&mut client, &job, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "failed", "{run}");
    assert_eq!(run["detail"], "session_closed", "{run}");
    // The disable has read the document; the save commits before it writes.
    hold.read.notified().await;
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": job, "prompt": "Work on it after the edit."}}),
        )
        .await
        .unwrap();
    hold.release();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let listed = listed_job_value(&listed, &job);
    assert_eq!(listed["enabled"], true, "{listed}");
    assert!(listed["disabled_reason"].is_null(), "{listed}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The restored Session takes the next window.
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job}))
        .await
        .unwrap();
    let run = settled(&mut client, &job, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "delivered", "{run}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A pause and resume that commit between the disable's read and its CAS
/// force a retry, and the retry re-checks the claim: the job stays enabled
/// (A.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_and_resume_between_the_disable_read_and_write_supersedes_it() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    let job = save_session_job(
        &mut client,
        "pause-resume",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    client
        .rpc("session.close", json!({"sessionId": session}))
        .await
        .unwrap();
    let hold = support::holds(home.path()).hold_next_disable();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job}))
        .await
        .unwrap();
    let run = settled(&mut client, &job, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "failed", "{run}");
    hold.read.notified().await;
    for enabled in [false, true] {
        client
            .typed::<Value>(
                "schedule.set_enabled",
                json!({"job_id": job, "enabled": enabled}),
            )
            .await
            .unwrap();
    }
    hold.release();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let listed = listed_job_value(&listed, &job);
    assert_eq!(listed["enabled"], true, "{listed}");
    assert!(listed["disabled_reason"].is_null(), "{listed}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// No save captures authority (§3.3): a job stores the preset its target
/// names, and every launch applies the live host bound. The legacy
/// `policy_cap` is read and preserved, never written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_save_stores_a_policy_cap() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let session = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "session-target",
        &session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"name": "launch-target",
                "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Work.",
                "target": {"kind": "launch", "cwd": home.path(),
                    "policy": "read-only"}}}),
        )
        .await
        .unwrap();
    let document = stored_document(home.path());
    for job in document["jobs"].as_array().unwrap() {
        assert!(
            job.get("policy_cap").is_none(),
            "no save captures authority: {job}"
        );
    }
    let launch = document["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["name"] == "launch-target")
        .unwrap();
    assert_eq!(
        launch["target"]["policy"], "read-only",
        "the job stores only the preset its target names: {launch}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A save that sends the legacy `reuse` binding's mapped target explicitly
/// has not replaced it: nothing is released, before or after a retention
/// pass (A.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicitly_sent_equivalent_target_does_not_release_the_binding() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let launched = create_session(&mut client, home.path(), true).await;
    let dummy_session = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "seed",
        &launched,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    seed_legacy_reuse_job(home.path(), &launched);

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let dummy = save_session_job(
        &mut client,
        "dummy",
        &dummy_session,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    // The client sends the binding's mapped target explicitly.
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse",
                "target": {"kind": "session", "session_id": launched, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    let runs = stored_runs(home.path());
    let run = runs.iter().find(|run| run["run_id"] == "run-1").unwrap();
    assert!(
        run.get("released").is_none(),
        "the equivalent target releases nothing: {run}"
    );
    // A retention pass re-derives no release either.
    client
        .typed::<Value>("schedule.delete", json!({"job_id": dummy}))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let runs = stored_runs(home.path());
    let run = runs.iter().find(|run| run["run_id"] == "run-1").unwrap();
    assert_eq!(run["session_origin"], "launch", "{run}");
    assert!(
        run.get("released").is_none(),
        "the retention pass releases nothing: {run}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A failed release wakes a retention pass, so the release heals at once
/// rather than at the timer's next minute (A.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_release_wakes_a_retention_pass() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let launched = create_session(&mut client, home.path(), true).await;
    let retarget = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "seed",
        &launched,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    seed_legacy_reuse_job(home.path(), &launched);

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    support::holds(home.path()).fail_next_release();
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse",
                "target": {"kind": "session", "session_id": retarget, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    // No delivery or delete follows: only the failed release's wake can run
    // the pass that re-derives it.
    wait_for("the failed release heals at the wake", || {
        stored_runs(home.path())
            .iter()
            .any(|run| run["run_id"] == "run-1" && run["released"] == true)
    })
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A later save of a job whose target was already replaced releases nothing
/// again: the ownership records are not rewritten, and the save attempts no
/// release at all (A.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_save_of_a_retargeted_job_releases_nothing_again() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let launched = create_session(&mut client, home.path(), true).await;
    let retarget = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "seed",
        &launched,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    seed_legacy_reuse_job(home.path(), &launched);

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    // The first save replaces the legacy binding and releases run-1.
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse",
                "target": {"kind": "session", "session_id": retarget, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    let path = home.path().join("scheduler/runs.json");
    let before = std::fs::read(&path).unwrap();
    // A later save that keeps the target must not release or rewrite again.
    let holds = support::holds(home.path());
    holds.fail_next_release();
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse", "name": "renamed"}}),
        )
        .await
        .unwrap();
    let after = std::fs::read(&path).unwrap();
    assert_eq!(before, after, "the ownership records are not rewritten");
    assert!(
        holds.release_failure_armed(),
        "the later save attempts no release"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A legacy `reuse` job saved with its mapped `session` target explicitly
/// has not replaced the binding, and keeps its stored cap (A.7, §2.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_job_saved_with_its_mapped_target_keeps_its_policy_cap() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    let launched = create_session(&mut client, home.path(), true).await;
    save_session_job(
        &mut client,
        "seed",
        &launched,
        None,
        json!({"at": "2030-01-01T00:00:00Z"}),
    )
    .await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    seed_legacy_reuse_job(home.path(), &launched);

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = support::connect(&server).await;
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"id": "reuse",
                "target": {"kind": "session", "session_id": launched, "branch_id": "main"}}}),
        )
        .await
        .unwrap();
    let stored = stored_document(home.path())["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == "reuse")
        .unwrap()
        .clone();
    assert!(
        stored.get("policy_cap").is_some(),
        "an equivalent target keeps the legacy cap: {stored}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
