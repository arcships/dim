//! Run lifecycle details through a real App Server: a tool-saved run that is
//! skipped sends no report, a run reads past one history page, a consumed
//! input is `running`, a past `at` job records `missed`, and an unload leaves
//! an active run for the next generation without settling it.
mod support;
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_is_not_editable_and_job_mutations_preserve_the_legacy_document() {
    let home = home();
    std::fs::write(
        home.path().join("schedule.json"),
        json!({"jobs": [], "max_concurrent_runs": 17}).to_string(),
    )
    .unwrap();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let saver = create_session(&mut client, home.path(), true).await;
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "legacy",
            "schedule": {"at": "2030-01-01T00:00:00Z"}, "prompt": "Check the inbox.",
            "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    let stored = || {
        rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
            .unwrap()
            .query_row(
                "SELECT version, document FROM settings_documents WHERE source = 'user/schedule'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap()
    };
    let before = stored();

    for (id, input) in [
        (
            "root-create",
            json!({"action": "create", "max_concurrent_runs": 32,
            "job": {"name": "bad", "schedule": {"at": "2030-01-01T00:00:00Z"}, "prompt": "p"}}),
        ),
        (
            "job-create",
            json!({"action": "create", "job": {"name": "bad",
            "schedule": {"at": "2030-01-01T00:00:00Z"}, "prompt": "p", "max_concurrent_runs": 32}}),
        ),
        (
            "root-update",
            json!({"action": "update", "job_id": job_id,
            "max_concurrent_runs": 32, "job": {"name": "bad"}}),
        ),
        (
            "job-update",
            json!({"action": "update", "job_id": job_id,
            "job": {"name": "bad", "max_concurrent_runs": 32}}),
        ),
    ] {
        model.push(vec![call(id, input)]);
        client
            .rpc(
                "session.submit",
                json!({"sessionId": saver, "branchId": "main", "text": id}),
            )
            .await
            .unwrap();
        client
            .rpc(
                "session.idle",
                json!({"sessionId": saver, "branchId": "main"}),
            )
            .await
            .unwrap();
        assert_eq!(
            stored(),
            before,
            "rejected tool {id} changes neither bytes nor revision"
        );
    }
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId": saver, "branchId": "main"}),
        )
        .await
        .unwrap();
    let events: Vec<dim_agent_types::AgentEvent> =
        serde_json::from_value(history["events"].clone()).unwrap();
    let results: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event.kind {
            dim_agent_types::EventKind::ToolResult(data) => Some(data.results),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        results.len(),
        4,
        "all invalid calls produce a result: {results:?}"
    );
    assert!(
        results
            .iter()
            .all(|result| result.result.is_error == Some(true)),
        "{results:?}"
    );

    for params in [
        json!({"max_concurrent_runs": 32, "job": {"name": "bad",
            "schedule": {"at": "2030-01-01T00:00:00Z"}, "prompt": "p", "cwd": home.path()}}),
        json!({"job": {"id": job_id, "name": "bad", "max_concurrent_runs": 32}, "revision": 1}),
    ] {
        assert!(client.rpc("schedule.save", params).await.is_err());
        assert_eq!(
            stored(),
            before,
            "rejected management writes preserve the document"
        );
    }
    for (method, params) in [
        (
            "settings.read",
            json!({"source": "schedule", "scope": "user"}),
        ),
        (
            "settings.update",
            json!({"source": "schedule", "scope": "user",
            "expectedRevision": format!("native:user/schedule:{}", before.0),
            "document": {"jobs": [], "max_concurrent_runs": 32}}),
        ),
    ] {
        let error = client.rpc(method, params).await.unwrap_err();
        assert!(
            error.to_string().contains("settings_unknown_source"),
            "{error}"
        );
        assert_eq!(stored(), before);
    }

    model.push(vec![call(
        "rename",
        json!({"action": "update", "job_id": job_id,
        "job": {"name": "renamed"}}),
    )]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": saver, "branchId": "main", "text": "rename"}),
        )
        .await
        .unwrap();
    client
        .rpc(
            "session.idle",
            json!({"sessionId": saver, "branchId": "main"}),
        )
        .await
        .unwrap();
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert_eq!(listed["jobs"][0]["name"], "renamed", "{listed}");
    for enabled in [false, true] {
        client
            .rpc(
                "schedule.set_enabled",
                json!({"job_id": job_id, "enabled": enabled}),
            )
            .await
            .unwrap();
    }
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "completed", "{run}");
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert_eq!(listed["jobs"][0]["target"]["kind"], "launch", "{listed}");
    assert!(listed["jobs"][0]["session_id"].is_null(), "{listed}");
    let document: Value = serde_json::from_str(&stored().1).unwrap();
    assert_eq!(document["max_concurrent_runs"], 17, "{document}");
    client
        .rpc("schedule.delete", json!({"job_id": job_id}))
        .await
        .unwrap();
    let document: Value = serde_json::from_str(&stored().1).unwrap();
    assert_eq!(document, json!({"jobs": [], "max_concurrent_runs": 17}));

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_runs_skip_overlap_and_capacity_and_release_their_slots_on_completion() {
    for legacy_limit in [2, 32] {
        let home = home();
        std::fs::write(
            home.path().join("schedule.json"),
            json!({"jobs": [], "max_concurrent_runs": legacy_limit}).to_string(),
        )
        .unwrap();
        let model = Arc::new(Script::default());
        model.hold("Hold the capacity slot.");
        let server = start(home.path(), model.clone()).await;
        let mut client = connect(&server).await;
        let mut jobs = Vec::new();
        let mut active = Vec::new();

        for index in 0..9 {
            let saved: Value = client
                .typed(
                    "schedule.save",
                    json!({"job": {"name": format!("capacity-{index}"),
                    "schedule": {"at": "2030-01-01T00:00:00Z"},
                    "prompt": "Hold the capacity slot.",
                    "target": {"kind": "launch", "cwd": home.path()}}}),
                )
                .await
                .unwrap();
            jobs.push(saved["job"]["id"].as_str().unwrap().to_owned());
        }

        for job in jobs.iter().take(8) {
            let accepted: Value = client
                .typed("schedule.run_now", json!({"job_id": job}))
                .await
                .unwrap();
            assert_eq!(
                accepted["state"], "launching",
                "eight slots regardless of legacy limit {legacy_limit}: {accepted}"
            );
            let run_id = accepted["run_id"].as_str().unwrap().to_owned();
            run_where(&mut client, job, &run_id, |run| run["state"] == "running").await;
            active.push(run_id);
        }

        for (job, reason) in [(&jobs[0], "overlap"), (&jobs[8], "capacity")] {
            let accepted: Value = client
                .typed("schedule.run_now", json!({"job_id": job}))
                .await
                .unwrap();
            let run = settled(&mut client, job, accepted["run_id"].as_str().unwrap()).await;
            assert_eq!(run["state"], "skipped", "{run}");
            assert_eq!(run["detail"], reason, "{run}");
            assert!(
                run["session_id"].is_null(),
                "no extra Session is launched: {run}"
            );
            assert!(
                run["input_event_id"].is_null(),
                "no extra input is submitted: {run}"
            );
        }

        model.release();
        for (job, run_id) in jobs.iter().zip(&active) {
            let run = settled(&mut client, job, run_id).await;
            assert_eq!(run["state"], "completed", "{run}");
        }

        for job in [&jobs[0], &jobs[8]] {
            let accepted: Value = client
                .typed("schedule.run_now", json!({"job_id": job}))
                .await
                .unwrap();
            let run = settled(&mut client, job, accepted["run_id"].as_str().unwrap()).await;
            assert_eq!(
                run["state"], "completed",
                "the released slot accepts work: {run}"
            );
        }

        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consumed_input_is_running_and_an_unload_leaves_it_for_recovery() {
    let home = home();
    let model = Arc::new(Script::default());
    model.hold("Hold this run.");
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "slow", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Hold this run.",
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
    let running = run_where(&mut client, &job_id, &run_id, |run| {
        run["state"] == "running"
    })
    .await;
    assert!(running["input_event_id"].is_string(), "{running}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let stored = stored_runs(home.path());
    let run = stored
        .iter()
        .find(|run| run["run_id"] == run_id.as_str())
        .unwrap();
    assert_eq!(
        run["state"], "running",
        "unload stops the tracker without settling the run: {run}"
    );

    model.release();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let resumed = settled(&mut client, &job_id, &run_id).await;
    assert_ne!(resumed["state"], "lost", "{resumed}");
    assert_eq!(
        resumed["input_event_id"], running["input_event_id"],
        "{resumed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_at_job_saved_after_its_instant_records_missed_and_disables() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "late", "schedule": {"at": "2020-01-01T00:00:00Z"},
                "prompt": "Too late.",
                "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();

    let disabled = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
            if listed["jobs"][0]["enabled"] == false {
                return listed;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the past at job disables itself");
    let history: Value = client
        .typed("schedule.history", json!({"job_id": job_id}))
        .await
        .unwrap();
    let runs = history["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{history}");
    assert_eq!(runs[0]["state"], "missed", "{history}");
    assert_eq!(runs[0]["window"], "2020-01-01T00:00:00.000Z", "{history}");
    assert!(runs[0]["session_id"].is_null(), "{history} {disabled}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_scheduler_does_not_block_native_sessions() {
    let home = home();
    std::fs::write(
        home.path().join("scheduler/runs.json"),
        serde_json::to_vec(&json!({"version": 1, "text": "{\"version\":2,\"runs\":[]}"})).unwrap(),
    )
    .unwrap();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;

    let session = create_session(&mut client, home.path(), true).await;
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "hello"}),
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
    assert!(model.count() >= 1, "the Session runs a turn");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_job_retires_its_history_and_launched_sessions() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;

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
    let run = settled(&mut client, &job_id, accepted["run_id"].as_str().unwrap()).await;
    let launched = run["session_id"].as_str().unwrap().to_owned();
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();
    assert!(listed.to_string().contains(&launched), "{listed}");

    client
        .typed::<Value>("schedule.delete", json!({"job_id": job_id}))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let listed = client.rpc("session.summaries", json!({})).await.unwrap();
            if !listed.to_string().contains(&launched)
                && !stored_runs(home.path())
                    .iter()
                    .any(|run| run["job_id"] == job_id.as_str())
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the launched Session is deleted before its last record is removed");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Restarts the server with the run's deadline moved into the past, as the
/// clock would have moved it: recovery tracks the run against that deadline.
async fn restart_expired(
    home: &std::path::Path,
    server: dim_app_server::RunningServer,
    client: dim_client::Client,
    model: Arc<Script>,
    run_id: &str,
) -> (dim_app_server::RunningServer, dim_client::Client) {
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home, |runs| {
        let run = runs.iter_mut().find(|run| run["run_id"] == run_id).unwrap();
        run["deadline_at"] = json!("2020-01-01T00:00:00.000Z");
    });
    let server = start(home, model).await;
    let client = connect(&server).await;
    (server, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_whose_turn_ended_before_its_deadline_keeps_that_outcome() {
    let home = home();
    let model = Arc::new(Script::default());
    model.hold("Hold this run.");
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "slow", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Hold this run.",
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
    run_where(&mut client, &job_id, &run_id, |run| {
        run["state"] == "running"
    })
    .await;

    let (server, mut client) =
        restart_expired(home.path(), server, client, model.clone(), &run_id).await;
    let ended = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(
        ended["state"], "cancelled",
        "the turn the shutdown ended is in the published prefix, so the past deadline does not replace it: {ended}"
    );
    model.release();

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_stop_of_the_run_turn_settles_cancelled() {
    let home = home();
    let model = Arc::new(Script::default());
    model.hold("Hold this run.");
    let server = start(home.path(), model.clone()).await;
    let mut client = connect(&server).await;

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "slow", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Hold this run.",
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
    let running = run_where(&mut client, &job_id, &run_id, |run| {
        run["state"] == "running"
    })
    .await;
    client
        .rpc(
            "session.stop",
            json!({"sessionId": running["session_id"], "branchId": "main"}),
        )
        .await
        .unwrap();
    let stopped = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(stopped["state"], "cancelled", "{stopped}");
    model.release();

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_document_written_by_the_typescript_scheduler_loads() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    client
        .typed::<Value>(
            "schedule.save",
            json!({"job": {"name": "seed", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "seed",
                "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let legacy = json!({"max_concurrent_runs": 4, "jobs": [{
        "id": "legacy", "name": "legacy", "schedule": {"at": "2030-01-01T00:00:00Z"},
        "prompt": "", "cwd": home.path(), "mode": "create", "enabled": false, "revision": 3,
        "created_by": "user",
        "policy_cap": {"preset": "read-only", "allowedTools": [], "disallowedTools": [],
            "fileAccess": "read-only", "toolApproval": "ask"}}]});
    rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .execute(
            "UPDATE settings_documents SET document = ?1 WHERE source = 'user/schedule'",
            [legacy.to_string()],
        )
        .unwrap();

    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let job = &listed["jobs"][0];
    assert_eq!(job["id"], "legacy", "{listed}");
    assert_eq!(job["target"]["kind"], "launch", "{listed}");
    assert_eq!(job["target"]["cwd"], json!(home.path()), "{listed}");
    assert_eq!(job["target"]["max_run_minutes"], 60, "{listed}");
    assert!(job["policy_cap"].is_null(), "{listed}");
    assert!(
        job["cwd"].is_null(),
        "legacy fields are not reported: {listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn management_saves_take_a_launch_target_with_an_agent_and_a_policy() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;

    let with_agent: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "agent run", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Check the inbox.",
                "target": {"kind": "launch", "cwd": home.path(), "agent_id": "default",
                    "policy": "read-only", "max_run_minutes": 5}}}),
        )
        .await
        .unwrap();
    assert_eq!(
        with_agent["job"]["target"]["agent_id"], "default",
        "{with_agent}"
    );
    assert_eq!(
        with_agent["job"]["target"]["policy"], "read-only",
        "{with_agent}"
    );
    assert_eq!(
        with_agent["job"]["target"]["max_run_minutes"], 5,
        "{with_agent}"
    );
    assert!(with_agent.get("transition").is_none(), "{with_agent}");

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "inbox", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Check the inbox.",
                "target": {"kind": "launch", "cwd": home.path(), "policy": "read-only"}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    let run: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run = settled(&mut client, &job_id, run["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "completed", "{run}");
    assert_eq!(run["policy"]["preset"], "read-only", "{run}");
    assert!(run["deadline_at"].is_string(), "{run}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The `next_window` `schedule.list` reports for a New York cron job whose
/// last recorded window is `after`.
async fn next_cron_window(cron: &str, after: &str) -> Value {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "dst", "schedule": {"cron": cron, "tz": "America/New_York"},
                "prompt": "p",
                "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    edit_runs(home.path(), |runs| {
        runs.push(json!({"run_id": "seed", "job_id": job_id, "window": after,
            "state": "missed", "submission_id": "scheduler:seed",
            "created_at": after, "ended_at": after, "detail": "missed"}));
    });
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let next = listed["jobs"][0]["next_window"].clone();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    next
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cron_windows_skip_a_dst_gap_and_fire_a_repeated_time_once() {
    // 2030-03-10 02:30 does not exist in New York; the next 02:30 is a day later.
    assert_eq!(
        next_cron_window("30 2 * * *", "2030-03-09T07:30:00.000Z").await,
        "2030-03-11T06:30:00.000Z"
    );
    // 2030-11-03 01:30 happens twice; it fires at the first (EDT, 05:30Z) only.
    assert_eq!(
        next_cron_window("30 1 * * *", "2030-11-03T05:30:00.000Z").await,
        "2030-11-04T06:30:00.000Z"
    );
}

#[test]
fn a_multi_line_report_guidance_is_refused() {
    let mut builder = dim_agent_runtime::AgentBuilder::new(Default::default());
    let refused = dim_agent_scheduler::install_with(
        &mut builder,
        dim_agent_scheduler::SchedulerTexts {
            report_guidance: "first\nsecond".into(),
            ..Default::default()
        },
    );
    assert!(refused.is_err(), "{refused:?}");
}
