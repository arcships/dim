//! Run lifecycle details through a real App Server: a launch window is
//! `delivered` once its input is written and is not tracked afterwards, a
//! past `at` job records `missed`, DST gaps skip and repeated times fire
//! once, and a launch Session runs until its stop time.
mod support;
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accepted_launch_survives_idle_shutdown_before_its_session_exists() {
    let home = home();
    let model = Arc::new(Script::default());
    let server = start(home.path(), model).await;
    let mut client = connect(&server).await;
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {
                "name": "admission", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Complete the accepted work.",
                "target": {"kind": "launch", "cwd": home.path()}
            }}),
        )
        .await
        .unwrap();
    let job = saved["job"]["id"].as_str().unwrap();
    let release = holds(home.path()).hold_next_launch();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job}))
        .await
        .unwrap();
    assert_eq!(accepted["state"], "delivering");
    assert_eq!(
        client.rpc("daemon.ping", json!({})).await.unwrap()["sessions"],
        0
    );
    let error = client
        .rpc("daemon.shutdown", json!({"ifIdle": true}))
        .await
        .unwrap_err();
    assert_eq!(
        dim_client::app_error(&error),
        Some("daemon_busy"),
        "{error}"
    );
    release.notify_one();
    let run = settled(&mut client, job, accepted["run_id"].as_str().unwrap()).await;
    assert_eq!(run["state"], "delivered", "{run}");
    client
        .rpc(
            "session.idle",
            json!({"sessionId": run["session_id"], "branchId": "main"}),
        )
        .await
        .unwrap();
    client
        .rpc("daemon.shutdown", json!({"ifIdle": true}))
        .await
        .unwrap();
    assert_eq!(
        server.join().await.unwrap(),
        dim_app_server::ShutdownReason::Graceful
    );
}

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
    assert_eq!(run["state"], "delivered", "{run}");
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
async fn launch_windows_are_not_capped_by_the_legacy_concurrency_field() {
    for legacy_limit in [2, 32] {
        let home = home();
        std::fs::write(
            home.path().join("schedule.json"),
            json!({"jobs": [], "max_concurrent_runs": legacy_limit}).to_string(),
        )
        .unwrap();
        let model = Arc::new(Script::default());
        model.hold("Hold the turn.");
        let server = start(home.path(), model.clone()).await;
        let mut client = connect(&server).await;
        let mut jobs = Vec::new();

        for index in 0..9 {
            let saved: Value = client
                .typed(
                    "schedule.save",
                    json!({"job": {"name": format!("launch-{index}"),
                    "schedule": {"at": "2030-01-01T00:00:00Z"},
                    "prompt": "Hold the turn.",
                    "target": {"kind": "launch", "cwd": home.path()}}}),
                )
                .await
                .unwrap();
            jobs.push(saved["job"]["id"].as_str().unwrap().to_owned());
        }

        for job in &jobs {
            let accepted: Value = client
                .typed("schedule.run_now", json!({"job_id": job}))
                .await
                .unwrap();
            let run = settled(&mut client, job, accepted["run_id"].as_str().unwrap()).await;
            assert_eq!(
                run["state"], "delivered",
                "every window launches, whatever max_concurrent_runs is: {run}"
            );
            assert!(run["session_id"].is_string(), "{run}");
            assert!(run["input_event_id"].is_string(), "{run}");
        }

        model.release();
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// A launch delivery is `delivered` once its input is written, even while the
/// launched Session still holds the turn: the record states what the window
/// did, not how the Session went on (§2.2, §3.2). A restart leaves it as
/// written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delivered_launch_is_not_tracked_and_an_unload_leaves_it_as_written() {
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
    let delivered = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(delivered["state"], "delivered", "{delivered}");
    assert!(delivered["input_event_id"].is_string(), "{delivered}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let stored = stored_runs(home.path());
    let run = stored
        .iter()
        .find(|run| run["run_id"] == run_id.as_str())
        .unwrap();
    assert_eq!(
        run, &delivered,
        "nothing settles a delivered launch after the fact"
    );

    model.release();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let resumed = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(resumed, delivered, "{resumed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A launch whose input reply is lost still settles from the identity the
/// launch persisted: the lookup reaches the launched Session, its `accepted`
/// answer settles the record `delivered`, the input is not sent again, and
/// nothing launches a second time (A.5, A.6).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_launch_with_a_lost_input_reply_settles_from_its_persisted_session() {
    let home = home();
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "lost-reply", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Summarize the day.",
                "target": {"kind": "launch", "cwd": home.path(), "max_run_minutes": 5}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();
    // The write reaches the target and is accepted; only its reply is lost.
    holds(home.path()).lose_next_input_reply();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();
    let run = settled(&mut client, &job_id, &run_id).await;

    assert_eq!(run["state"], "delivered", "{run}");
    assert_eq!(run["attempts"], 1, "one lookup settled it: {run}");
    let session_id = run["session_id"].as_str().unwrap().to_owned();
    let submission = run["submission_id"].clone();
    let mut inputs = Vec::new();
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
        inputs.extend(
            page.events
                .into_iter()
                .filter(|event| event["data"]["submission_id"] == submission),
        );
        match next {
            Some(next) => after = next,
            None => break,
        }
    }
    assert_eq!(inputs.len(), 1, "the input is not sent again: {inputs:?}");

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

    // The first timer pass records the past window `missed`; the one after it
    // finds the delivery terminal and disables the job. Two crossed ticks are
    // those two passes, rather than a poll for a real second.
    step(home.path(), 2).await;
    let disabled: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert_eq!(disabled["jobs"][0]["enabled"], false, "{disabled}");
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
    let mut client = connect(&server).await;
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();
    assert!(listed.to_string().contains(&launched), "{listed}");

    // The delete spawns the retention pass that deletes the launched Session
    // and then its record; the count names that pass, and both facts are read
    // once after it.
    let before = signals(home.path()).retention();
    client
        .typed::<Value>("schedule.delete", json!({"job_id": job_id}))
        .await
        .unwrap();
    retention(home.path(), before).await;
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();
    assert!(!listed.to_string().contains(&launched), "{listed}");
    assert!(
        !stored_runs(home.path())
            .iter()
            .any(|run| run["job_id"] == job_id.as_str()),
        "the launched Session is deleted before its last record is removed"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// The stop timer ends the launched Session at its stop time and leaves the
/// delivery `delivered` (§3.2): a stop bounds the launched work, it is not a
/// run outcome. A restart resumes the timer from the record's `deadline_at`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_launch_session_is_stopped_at_its_stop_time() {
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
    let delivered = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(delivered["state"], "delivered", "{delivered}");
    let session_id = delivered["session_id"].as_str().unwrap().to_owned();
    assert!(
        !delivered["deadline_at"].is_null(),
        "a launch records its stop time: {delivered}"
    );

    // Move the stop time an hour out, as a restart that resumes the timer long
    // before it would meet it; the advance below is what reaches it.
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let soon = (clock(home.path()).now() + chrono::Duration::seconds(3600))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    edit_runs(home.path(), |runs| {
        let run = runs
            .iter_mut()
            .find(|run| run["run_id"] == run_id.as_str())
            .unwrap();
        run["deadline_at"] = json!(soon);
    });
    let parks = stop_parks(home.path());
    let server = start(home.path(), Arc::new(Script::default())).await;
    let mut client = connect(&server).await;
    stop_time(home.path(), parks).await;

    // The held turn ends at the stop time; the held model reply is what the
    // launched Session was waiting on.
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let idle = client
                .rpc(
                    "session.idle",
                    json!({"sessionId": session_id, "branchId": "main"}),
                )
                .await;
            match idle {
                Ok(_) => return Ok(()),
                Err(error) if format!("{error}").contains("reloading") => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the stop timer runs");
    assert!(stopped.is_ok(), "{stopped:?}");
    let after = settled(&mut client, &job_id, &run_id).await;
    assert_eq!(
        after["state"], "delivered",
        "the stop is not a run outcome: {after}"
    );

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
    assert_eq!(run["state"], "delivered", "{run}");
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
