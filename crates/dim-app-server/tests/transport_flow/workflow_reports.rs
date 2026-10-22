//! Public report/store binding acceptance; no Workflow stage engine is installed.
use super::*;
use std::path::{Path, PathBuf};

fn parts() -> dim_app_server::session_typescript::SessionTypeScriptConfig {
    let mut parts = super::super::session_typescript::fixture();
    parts.rows.clear();
    parts.process_rows = vec![json!({
        "id":"workflow-report", "name":parts.anchor.parent().unwrap().join("fixtures/workflow-report.ts"),
        "provides":{"managementMethods:report":{"describe":"async","call":"async"}}, "config":{},
    })];
    parts
}
fn home() -> PathBuf {
    // Failed probes retain their disposable home for inspection.
    let home = std::env::temp_dir().join(format!("dim-report-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    home
}
fn seed(home: &Path, session: &str) {
    let run = json!({
        "run_id":"binding-report", "name":"binding-report", "source":"user",
        "definition":{"schema":"dim.workflow/v1","name":"binding-report","parameters":[],
            "steps":[{"stage":{"id":"work","prompt":"fixture"}}]},
        "params":{}, "cwd":home, "report_to":{"session_id":session,"branch_id":"main"},
        "started_by":"management", "created_at":"2026-10-21T00:00:00.000Z",
        "ended_at":"2026-10-21T00:00:01.000Z", "state":"completed",
        "stages":[{"id":"work","step":0,"state":"completed","result_text":"completed"}], "log":[],
        "report":{"state":"pending","submission_id":"workflow:report:binding-report","attempt_open":false}
    });
    std::fs::write(
        home.join("workflow/runs.json"),
        json!({
            "version":1,"text":json!({"version":2,"runs":[run]}).to_string()
        })
        .to_string(),
    )
    .unwrap();
}
fn stored(home: &Path) -> Value {
    let outer: Value =
        serde_json::from_slice(&std::fs::read(home.join("workflow/runs.json")).unwrap()).unwrap();
    assert_eq!(outer["version"], 1);
    serde_json::from_str(outer["text"].as_str().unwrap()).unwrap()
}
async fn start(
    home: &Path,
    controls: Option<tokio::sync::mpsc::UnboundedSender<super::super::session_typescript::Control>>,
) -> Served {
    serve_with(Setup {
        home: Some(home.to_owned()),
        database: Some(home.join("native.sqlite")),
        typescript: Some(parts()),
        controls,
        ..Default::default()
    })
    .await
}
async fn ready(client: &mut Client) -> Value {
    super::super::process_typescript::files_available(client, "report.status", json!({})).await
}
async fn phase(client: &mut Client, id: u64, phase: &str) {
    loop {
        let notice = client.notice("management.progress").await;
        if notice["params"]["id"] == id && notice["params"]["data"]["phase"] == phase {
            return;
        }
    }
}
async fn count(session: &Session) -> usize {
    session.local("main", ..).await.unwrap().iter().filter(|event|
        matches!(&event.kind, EventKind::InputMessage(data) if data.submission_id.as_deref() == Some("workflow:report:binding-report"))
    ).count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_report_crash_keeps_the_marker_and_drains_before_recovery() {
    let home = home();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = start(&home, Some(send)).await;
    super::super::process_typescript::select_model(&mut client).await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    seed(&home, target.id());
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let owner = loader.get("workflow-report").unwrap().view.unwrap();
    let before = ready(&mut client).await;
    let connection = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    // Release the fault before the native writer's five-second busy timeout.
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let request = client.send("report.send", json!({})).await;
    phase(&mut client, request, "input").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !target.publication("main").unwrap().has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stored(&home)["runs"][0]["report"]["attempt_open"], true);
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &before["pid"].as_u64().unwrap().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(client.response(request).await.get("error").is_some());
    let (restarted, ()) = tokio::join!(loader.restart("SessionNodeRuntime"), async {
        let mut changed = owner.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while changed.borrow_and_update().state != rutis::FiberState::Unloading {
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            owner
                .effects()
                .iter()
                .any(|effect| effect.label == "sessions.input"),
            "the original admitted write still owns its drain while SQLite holds it"
        );
        connection.execute_batch("COMMIT").unwrap();
    });
    restarted.unwrap();
    assert_eq!(count(&target).await, 1);
    let lookup = sessions
        .ok(
            "lookup",
            json!({"sessionId":target.id(),"branchId":"main","submissionId":"workflow:report:binding-report"}),
        )
        .await;
    assert_eq!(lookup["status"], "accepted");
    assert_ne!(ready(&mut client).await["pid"], before["pid"]);
    let reply = client.rpc("report.recover", json!({})).await;
    assert_ok(&reply);
    assert_eq!(
        stored(&home)["runs"][0]["report"],
        json!({"state":"delivered"})
    );
    assert_eq!(count(&target).await, 1);
    loader.restart("workflow-report").await.unwrap();
    ready(&mut client).await;
    assert_ok(&client.rpc("report.recover", json!({})).await);
    assert_eq!(count(&target).await, 1);
    // Existing duplicate inputs settle delivered and emit a violation, never a third input.
    let duplicate = client_session(&mut client, &agent, host_policy()).await;
    seed(&home, duplicate.id());
    for _ in 0..2 {
        sessions
            .ok(
                "input",
                input(&duplicate, "workflow:report:binding-report", json!([])),
            )
            .await;
    }
    loader.restart("workflow-report").await.unwrap();
    ready(&mut client).await;
    let recover = client.send("report.recover", json!({})).await;
    phase(&mut client, recover, "duplicate_report_violation").await;
    assert_ok(&client.response(recover).await);
    assert_eq!(
        stored(&home)["runs"][0]["report"],
        json!({"state":"delivered"})
    );
    assert_eq!(count(&duplicate).await, 2);
    loader.restart("workflow-report").await.unwrap();
    ready(&mut client).await;
    assert_ok(&client.rpc("report.recover", json!({})).await);
    assert_eq!(count(&duplicate).await, 2);
    server.shutdown();
    server.join().await.unwrap();
    drop(connection);
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_report_crash_before_input_retries_after_the_original_row_drains() {
    let home = home();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let Served {
        server,
        agent,
        mut client,
        ..
    } = start(&home, Some(send)).await;
    super::super::process_typescript::select_model(&mut client).await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    seed(&home, target.id());
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let before = ready(&mut client).await;
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = client
        .send(
            "report.send",
            json!({"pause":"opened","gate":{
                "host":"127.0.0.1","port":gate.local_addr().unwrap().port()
            }}),
        )
        .await;
    let (_release, _) = tokio::time::timeout(Duration::from_secs(5), gate.accept())
        .await
        .unwrap()
        .unwrap();
    phase(&mut client, request, "opened").await;
    assert_eq!(stored(&home)["runs"][0]["report"]["attempt_open"], true);
    assert_eq!(count(&target).await, 0);
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &before["pid"].as_u64().unwrap().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(client.response(request).await.get("error").is_some());
    loader.restart("SessionNodeRuntime").await.unwrap();
    assert_ne!(ready(&mut client).await["pid"], before["pid"]);
    assert_ok(&client.rpc("report.recover", json!({})).await);
    assert_eq!(
        stored(&home)["runs"][0]["report"],
        json!({"state":"delivered"})
    );
    assert_eq!(count(&target).await, 1);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_report_unavailable_store_drains_and_reconciles_without_resend() {
    let home = home();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let Served {
        server,
        agent,
        mut client,
        ..
    } = start(&home, Some(send)).await;
    super::super::process_typescript::select_model(&mut client).await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    seed(&home, target.id());
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let owner = loader.get("workflow-report").unwrap().view.unwrap();
    ready(&mut client).await;
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = client
        .send(
            "report.send",
            json!({"pause":"reply_write","gate":{
                "host":"127.0.0.1","port":gate.local_addr().unwrap().port()
            }}),
        )
        .await;
    let (mut release, _) = tokio::time::timeout(Duration::from_secs(5), gate.accept())
        .await
        .unwrap()
        .unwrap();
    phase(&mut client, request, "reply_write").await;
    assert_eq!(count(&target).await, 1);
    let path = home.join("workflow/runs.json");
    let saved = home.join("workflow/held.json");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (restarted, ()) = tokio::join!(loader.restart("workflow-report"), async {
        let mut changed = owner.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while changed.borrow_and_update().state != rutis::FiberState::Unloading {
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            client.rpc("report.status", json!({})).await["error"]["data"]["appError"],
            "service_reloading"
        );
        release.write_all(b"replace").await.unwrap();
        assert_eq!(
            client.response(request).await["error"]["data"]["appError"],
            "io"
        );
    });
    restarted.unwrap();
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(saved, &path).unwrap();
    assert_eq!(stored(&home)["runs"][0]["report"]["attempt_open"], true);
    ready(&mut client).await;
    assert_ok(&client.rpc("report.recover", json!({})).await);
    assert_eq!(
        stored(&home)["runs"][0]["report"],
        json!({"state":"delivered"})
    );
    assert_eq!(count(&target).await, 1);

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

async fn child(home: &Path) -> (tokio::process::Child, Client) {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sessions::workflow_reports::node_workflow_reports_reconcile_after_host_process_restart", "--ignored", "--nocapture"])
        .env("DIM_WORKFLOW_REPORT_HOST", home)
        .env("DIMCODE_HOME", home)
        .stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let socket = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).await.unwrap(),
                0,
                "child exited before startup"
            );
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && let Some(socket) = value["socket"].as_str()
            {
                break PathBuf::from(socket);
            }
        }
    })
    .await
    .unwrap();
    child.stdout = Some(output.into_inner());
    let mut client = Client {
        transport: Transport::Unix(BufReader::new(UnixStream::connect(socket).await.unwrap())),
        id: 0,
        notices: vec![],
        instance: Value::Null,
        features: Value::Null,
    };
    assert_ok(
        &client
            .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await,
    );
    ready(&mut client).await;
    (child, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_reports_reconcile_after_host_process_restart() {
    if let Some(home) = std::env::var_os("DIM_WORKFLOW_REPORT_HOST") {
        let Served { server, .. } = start(Path::new(&home), None).await;
        println!("\n{}", json!({"socket":server.unix_path}));
        server.join().await.unwrap();
        return;
    }
    for pause in ["opened", "reply_write"] {
        let home = home();
        let (mut host, mut client) = child(&home).await;
        super::super::process_typescript::select_model(&mut client).await;
        let created = client
            .rpc("session.create", json!({"cwd":home,"tools":[]}))
            .await;
        assert_ok(&created);
        let session = created["result"]["sessionId"].as_str().unwrap();
        seed(&home, session);
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let request = client
            .send(
                "report.send",
                json!({"pause":pause,"gate":{
                    "host":"127.0.0.1","port":gate.local_addr().unwrap().port()
                }}),
            )
            .await;
        let (_release, _) = tokio::time::timeout(Duration::from_secs(5), gate.accept())
            .await
            .unwrap()
            .unwrap();
        phase(&mut client, request, pause).await;
        assert_eq!(stored(&home)["runs"][0]["report"]["attempt_open"], true);
        let pid = host.id().unwrap();
        host.kill().await.unwrap();
        host.wait().await.unwrap();
        drop(client);
        let (mut host, mut client) = child(&home).await;
        assert_ne!(host.id().unwrap(), pid);
        assert_ok(&client.rpc("report.recover", json!({})).await);
        assert_eq!(
            stored(&home)["runs"][0]["report"],
            json!({"state":"delivered"})
        );
        let history = client
            .rpc(
                "session.history",
                json!({"sessionId":session,"branchId":"main","after":0,"limit":1000}),
            )
            .await;
        assert_ok(&history);
        assert_eq!(history["result"]["done"], true);
        assert_eq!(
            history["result"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["submission_id"] == "workflow:report:binding-report")
                .count(),
            1,
            "{history}"
        );
        assert_ok(&client.rpc("daemon.shutdown", json!({"force":true})).await);
        tokio::time::timeout(Duration::from_secs(10), host.wait())
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}
