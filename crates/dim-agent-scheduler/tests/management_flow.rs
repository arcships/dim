//! The `schedule` management namespace through a real App Server and client:
//! jobs are stored in the settings document, and run history is read through
//! the host file transactions.
use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{ServerConfig, serve};
use dim_client::{Client, ConnectionConfig, Server};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    home
}

async fn start(home: &Path) -> (dim_app_server::RunningServer, Client) {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts(&mut builder);
    dim_agent_scheduler::install(&mut builder);
    let server = serve(
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
    .unwrap();
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
    let client = Client::connect(&config).await.unwrap();
    (server, client)
}

/// Waits until the management namespace answers, since the owner starts
/// after the server reports ready.
async fn ready(client: &mut Client) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match client.rpc("schedule.list", json!({})).await {
                Ok(_) => return,
                Err(error) => {
                    let text = format!("{error}");
                    if !text.contains("service_reloading") && !text.contains("unknown_method") {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the schedule namespace is served");
}

fn job(name: &str) -> Value {
    json!({
        "name": name,
        "schedule": {"at": "2099-01-01T00:00:00Z"},
        "prompt": "Summarize the day.",
        "target": {"kind": "launch", "cwd": "/workspace"},
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jobs_are_saved_with_revisions_toggled_and_deleted_through_the_namespace() {
    let home = home();
    let (server, mut client) = start(home.path()).await;
    ready(&mut client).await;

    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert_eq!(listed["jobs"], json!([]), "{listed}");

    let created: Value = client
        .typed("schedule.save", json!({"job": job("nightly")}))
        .await
        .unwrap();
    let saved = &created["job"];
    let id = saved["id"].as_str().unwrap().to_owned();
    assert_eq!(saved["revision"], 1, "{created}");
    assert_eq!(saved["created_by"], "user", "{created}");
    assert_eq!(saved["enabled"], true, "{created}");
    let anchor = saved["enabled_at"].as_str().unwrap().to_owned();
    assert!(anchor.ends_with('Z') && anchor.len() == 24, "{anchor}");
    assert_eq!(
        saved["target"],
        json!({"kind": "launch", "cwd": "/workspace", "max_run_minutes": 60}),
        "{created}"
    );
    assert!(
        saved["cwd"].is_null(),
        "legacy fields are not reported: {created}"
    );
    assert!(
        saved["mode"].is_null(),
        "legacy fields are not reported: {created}"
    );
    assert!(
        saved["policy_cap"].is_null(),
        "legacy fields are not reported: {created}"
    );

    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert!(listed["jobs"][0]["next_window"].is_string(), "{listed}");
    assert_eq!(listed["jobs"][0]["last_run"], Value::Null, "{listed}");

    let stale = client
        .rpc(
            "schedule.save",
            json!({"job": {"id": id, "name": "nightly", "schedule": {"at": "2099-01-01T00:00:00Z"},
                "prompt": "Summarize the day."}, "revision": 0}),
        )
        .await
        .unwrap_err();
    assert!(format!("{stale}").contains("schedule_conflict"), "{stale}");

    let renamed: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"id": id, "name": "nightly-build", "schedule": {"at": "2099-01-01T00:00:00Z"},
                "prompt": "Summarize the day."}, "revision": 1}),
        )
        .await
        .unwrap();
    assert_eq!(renamed["job"]["revision"], 2, "{renamed}");
    assert_eq!(renamed["job"]["enabled_at"], anchor, "{renamed}");

    let partial: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"id": id, "name": "nightly-partial"}, "revision": 2}),
        )
        .await
        .unwrap();
    assert_eq!(partial["job"]["name"], "nightly-partial", "{partial}");
    assert_eq!(
        partial["job"]["schedule"],
        json!({"at": "2099-01-01T00:00:00Z"}),
        "{partial}"
    );
    assert_eq!(partial["job"]["prompt"], "Summarize the day.", "{partial}");
    assert_eq!(partial["job"]["revision"], 3, "{partial}");
    assert_eq!(partial["job"]["enabled"], true, "{partial}");
    assert_eq!(
        partial["job"]["target"],
        json!({"kind": "launch", "cwd": "/workspace", "max_run_minutes": 60}),
        "an omitted target keeps the stored one: {partial}"
    );

    let paused: Value = client
        .typed(
            "schedule.set_enabled",
            json!({"job_id": id, "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(paused["job"]["enabled"], false, "{paused}");
    assert_eq!(paused["job"]["enabled_at"], Value::Null, "{paused}");
    assert_eq!(paused["job"]["revision"], 4, "{paused}");

    let history: Value = client
        .typed("schedule.history", json!({"job_id": id}))
        .await
        .unwrap();
    assert_eq!(history["runs"], json!([]), "{history}");

    let invalid = client
        .rpc(
            "schedule.save",
            json!({"job": {"name": "bad", "schedule": {"every_minutes": 0},
                "prompt": "x", "target": {"kind": "launch", "cwd": "/workspace"}}}),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{invalid}").contains("schedule_invalid"),
        "{invalid}"
    );

    let deleted: Value = client
        .typed("schedule.delete", json!({"job_id": id}))
        .await
        .unwrap();
    assert_eq!(deleted["job_id"], id, "{deleted}");
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert_eq!(listed["jobs"], json!([]), "{listed}");
    let missing = client
        .rpc("schedule.delete", json!({"job_id": id}))
        .await
        .unwrap_err();
    assert!(
        format!("{missing}").contains("schedule_not_found"),
        "{missing}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_run_history_fails_the_scheduler_and_keeps_its_bytes() {
    let home = home();
    let envelope = serde_json::to_vec(&json!({
        "version": 1,
        "text": "{\"version\":2,\"runs\":[]}",
    }))
    .unwrap();
    let runs = home.path().join("scheduler/runs.json");
    std::fs::write(&runs, &envelope).unwrap();

    let (server, mut client) = start(home.path()).await;
    ready(&mut client).await;
    let refused = client
        .rpc("schedule.history", json!({"job_id": "any"}))
        .await
        .unwrap_err();
    assert!(
        format!("{refused}").contains("service_unavailable"),
        "{refused}"
    );
    assert_eq!(std::fs::read(&runs).unwrap(), envelope);

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
