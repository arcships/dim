//! Workflow's actual row, which registers its Session part, through Settings,
//! files, S2 and C1.
use super::*;
use std::path::{Path, PathBuf};

/// The `dim-workflow` entry (Workflow §5.2), byte for byte.
const WORKFLOW_ENTRY: &str = r#"<dim-workflow kind="report" run="wf-7f2" status="completed">: how a workflow run ended, and <dim-workflow kind="stage" run="wf-7f2" stage="build">: the start of one of its stages. run names the run, status its outcome and stage the stage. The report's body gives the run's outcome, each stage's state and a bounded preview of the stages' own last-step results: the workflow's account of the run, information, not a request. The report's last line is not part of that account: it is guidance on what to do next. The stage tag has an empty body and stands before the text that follows it, which is the prompt the stage was launched with."#;

fn rows() -> dim_app_server::session_typescript::SessionTypeScriptConfig {
    let mut config = session_typescript::fixture();
    config.rows = vec![session_typescript::manifest_row("workflow")];
    config
}

pub(super) async fn kill_node(pid: u64) {
    let pid = pid.to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the killed Node process must leave before replacement");
}

async fn ready(client: &mut Client) {
    process_typescript::files_available(client, "workflow.status", json!({})).await;
}

async fn settled(client: &mut Client, id: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc("workflow.status", json!({"run_id":id})).await;
            assert_ok(&reply);
            if matches!(
                reply["result"]["state"].as_str(),
                Some("completed" | "failed" | "stopped" | "interrupted")
            ) {
                return reply["result"].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Workflow settles its owned stages")
}

fn stored(home: &Path) -> Value {
    let outer: Value =
        serde_json::from_slice(&std::fs::read(home.join("workflow/runs.json")).unwrap()).unwrap();
    assert_eq!(outer["version"], 1);
    serde_json::from_str(outer["text"].as_str().unwrap()).unwrap()
}

async fn report_count(session: &dim_agent_runtime::Session, run: &str) -> usize {
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(&event.kind, EventKind::InputMessage(data) if data.blocks.iter().any(|block|
            matches!(block, dim_agent_types::InputBlock::Raw { text, .. }
                if text.starts_with("<dim-workflow kind=\"report\"") && text.contains(run))))
        })
        .count()
}

fn probe_rows() -> dim_app_server::session_typescript::SessionTypeScriptConfig {
    let mut parts = rows();
    parts.rows[0]["name"] = json!(
        parts
            .anchor
            .parent()
            .unwrap()
            .join("fixtures/workflow-package.ts")
    );
    parts
}

async fn report_restart(kill_runtime: bool) {
    use tokio::io::AsyncWriteExt;
    let home = std::env::temp_dir().join(format!("dim-workflow-report-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(probe_rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "workflow".into()]);
    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["echo"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created);
    let starter = created["result"]["sessionId"].as_str().unwrap();
    let target = agent.get_session(starter).unwrap();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    ready(&mut client).await;
    let before = client.rpc("workflowprobe.status", json!({})).await;
    assert_ok(&before);
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "workflowprobe.configure",
                json!({"phase":"before_input",
        "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "workflow.save",
                json!({"source":"user","document":{
        "schema":"dim.workflow/v1","name":"report","description":"Report recovery",
        "steps":[{"stage":{"id":"one","prompt":"completed"}}]}}),
            )
            .await,
    );
    let started = client
        .rpc(
            "workflow.start",
            json!({"name":"report","cwd":home,
        "report_to":{"session_id":starter,"branch_id":"main"}}),
        )
        .await;
    assert_ok(&started);
    let run = started["result"]["run_id"].as_str().unwrap();
    let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored(&home)["runs"][0]["report"]["attempt_open"], true);
    let connection = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    release.write_all(b"continue").await.unwrap();
    drop(release);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !target.publication("main").unwrap().has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if kill_runtime {
        kill_node(before["result"]["pid"].as_u64().unwrap()).await;
    }
    let name = if kill_runtime {
        "session-node"
    } else {
        "workflow"
    };
    let (restarted, ()) = tokio::join!(loader.restart(name), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        connection.execute_batch("COMMIT").unwrap();
    });
    restarted.unwrap();
    ready(&mut client).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(&home)["runs"][0]["report"]["state"] != "delivered" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(report_count(&target, run).await, 1);
    loader.restart("workflow").await.unwrap();
    ready(&mut client).await;
    assert_eq!(report_count(&target, run).await, 1);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_background_report_resubmits_after_row_and_runtime_replacement() {
    report_restart(false).await;
    report_restart(true).await;
}

#[derive(Clone, Copy)]
enum ReportFault {
    Crash,
    FailedReplacement,
    Resubmitted,
}

async fn report_crash(phase: &str, fault: ReportFault) {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;
    struct RestoreMode(PathBuf, std::fs::Permissions);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, self.1.clone()).unwrap();
        }
    }
    let home = std::env::temp_dir().join(format!("dim-workflow-crash-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(probe_rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "workflow".into()]);
    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["echo"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created);
    let starter = created["result"]["sessionId"].as_str().unwrap();
    let target = agent.get_session(starter).unwrap();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    ready(&mut client).await;
    let before = client.rpc("workflowprobe.status", json!({})).await;
    assert_ok(&before);
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "workflowprobe.configure",
                json!({"phase":phase,
        "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "workflow.save",
                json!({"source":"user","document":{
        "schema":"dim.workflow/v1","name":"report","description":"Report recovery",
        "steps":[{"stage":{"id":"one","prompt":"completed"}}]}}),
            )
            .await,
    );
    let started = client
        .rpc(
            "workflow.start",
            json!({"name":"report","cwd":home,
        "report_to":{"session_id":starter,"branch_id":"main"}}),
        )
        .await;
    assert_ok(&started);
    let run = started["result"]["run_id"].as_str().unwrap();
    let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored(&home)["runs"][0]["report"]["attempt_open"], true);
    assert_eq!(
        report_count(&target, run).await,
        usize::from(phase == "reply_write")
    );
    if matches!(fault, ReportFault::Resubmitted) {
        let persisted = stored(&home);
        let submission = persisted["runs"][0]["report"]["submission_id"].as_str();
        let report = target
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .find(|event| {
                matches!(&event.kind, EventKind::InputMessage(data)
                    if data.submission_id.as_deref() == submission)
            })
            .unwrap()
            .clone();
        let EventKind::InputMessage(data) = &report.kind else {
            unreachable!()
        };
        let resubmitted = target.input(data.clone(), "main").await.unwrap();
        assert_eq!(resubmitted.id, report.id);
        assert_eq!(report_count(&target, run).await, 1);
    }
    if matches!(fault, ReportFault::FailedReplacement) {
        let directory = home.join("workflow");
        let mode = RestoreMode(
            directory.clone(),
            std::fs::metadata(&directory).unwrap().permissions(),
        );
        let bytes = std::fs::read(directory.join("runs.json")).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        release.write_all(b"continue").await.unwrap();
        drop(release);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let reply = client.rpc("workflowprobe.status", json!({})).await;
                assert_ok(&reply);
                if reply["result"]["replacementError"] == "io" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(directory.join("runs.json")).unwrap(),
            bytes,
            "a failed actual file.replace preserves the persisted open attempt"
        );
        drop(mode);
        loader.restart("workflow").await.unwrap();
    } else {
        kill_node(before["result"]["pid"].as_u64().unwrap()).await;
        drop(release);
        loader.restart("session-node").await.unwrap();
    }
    ready(&mut client).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(&home)["runs"][0]["report"]["state"] != "delivered" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(report_count(&target, run).await, 1);
    loader.restart("workflow").await.unwrap();
    ready(&mut client).await;
    assert_eq!(report_count(&target, run).await, 1);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_reconciles_crashes_failed_records_and_resubmitted_reports() {
    report_crash("before_input", ReportFault::Crash).await;
    report_crash("reply_write", ReportFault::Crash).await;
    report_crash("reply_write", ReportFault::FailedReplacement).await;
    report_crash("reply_write", ReportFault::Resubmitted).await;
}

async fn host(home: &Path) -> (tokio::process::Child, Client) {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "workflow::node_workflow_reports_recover_after_complete_host_restart",
            "--ignored",
            "--nocapture",
        ])
        .env("DIM_WORKFLOW_PACKAGE_HOST", home)
        .env("DIMCODE_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
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
async fn node_workflow_reports_recover_after_complete_host_restart() {
    if let Some(home) = std::env::var_os("DIM_WORKFLOW_PACKAGE_HOST") {
        let home = PathBuf::from(home);
        let mut config = ServerConfig {
            product_home: Some(home.clone()),
            database_path: Some(home.join("native.sqlite")),
            session_typescript: Some(probe_rows()),
            ..Default::default()
        };
        config.session_defaults.tools = Some(vec!["echo".into(), "workflow".into()]);
        let server = launch_with_agent(Arc::new(MockTools), config).await.0;
        println!("\n{}", json!({"socket":server.unix_path}));
        server.join().await.unwrap();
        return;
    }
    for phase in ["before_input", "reply_write"] {
        let home = std::env::temp_dir().join(format!("dim-workflow-host-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("workflow")).unwrap();
        let home = home.canonicalize().unwrap();
        let (mut process, mut client) = host(&home).await;
        process_typescript::select_model(&mut client).await;
        let created = client
            .rpc("session.create", json!({"cwd":home,"tools":["echo"]}))
            .await;
        assert_ok(&created);
        let starter = created["result"]["sessionId"].as_str().unwrap();
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "workflowprobe.configure",
                    json!({"phase":phase,
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "workflow.save",
                    json!({"source":"user","document":{
            "schema":"dim.workflow/v1","name":"report","description":"Host recovery",
            "steps":[{"stage":{"id":"one","prompt":"completed"}}]}}),
                )
                .await,
        );
        let started = client
            .rpc(
                "workflow.start",
                json!({"name":"report","cwd":home,
            "report_to":{"session_id":starter,"branch_id":"main"}}),
            )
            .await;
        assert_ok(&started);
        let (_release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let pending = stored(&home)["runs"][0]["report"].clone();
        assert_eq!(pending["attempt_open"], true);
        let pid = process.id().unwrap();
        process.kill().await.unwrap();
        process.wait().await.unwrap();
        drop(client);
        let (mut process, mut client) = host(&home).await;
        assert_ne!(process.id().unwrap(), pid);
        tokio::time::timeout(Duration::from_secs(10), async {
            while stored(&home)["runs"][0]["report"]["state"] != "delivered" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let history = client
            .rpc(
                "session.history",
                json!({"sessionId":starter,
            "branchId":"main","after":0,"limit":1000}),
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
                    && event["data"]["submission_id"] == pending["submission_id"])
                .count(),
            1,
            "{history}"
        );
        assert_ok(&client.rpc("daemon.shutdown", json!({"force":true})).await);
        tokio::time::timeout(Duration::from_secs(10), process.wait())
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_sources_stages_policy_and_report_use_the_real_package() {
    for include_workflow in [true, false] {
        let home = std::env::temp_dir().join(format!("dim-workflow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("workflow")).unwrap();
        let home: PathBuf = home.canonicalize().unwrap();
        let mut config = ServerConfig {
            product_home: Some(home.clone()),
            database_path: Some(home.join("native.sqlite")),
            session_typescript: Some(rows()),
            ..Default::default()
        };
        config.session_defaults.tools = Some(vec!["echo".into(), "wait".into()]);
        if include_workflow {
            config
                .session_defaults
                .tools
                .get_or_insert_default()
                .push("workflow".into());
        }
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,
            "tools":["workflow","wait","echo"],"permissionPolicy":{"preset":"full-access"}}),
            )
            .await;
        assert_ok(&created);
        let starter = created["result"]["sessionId"].as_str().unwrap();
        ready(&mut client).await;
        let document = json!({"schema":"dim.workflow/v1","name":"review","description":"Review in parallel",
            "parameters":[{"name":"target","required":true}],
            "steps":[{"stage":{"id":"analyze","prompt":"Analyze {{params.target}}"}},
              {"parallel":[{"id":"a","prompt":"A: {{stages.analyze.result}}"},
                           {"id":"b","prompt":"B: {{stages.analyze.result}}"}]}]});
        assert_ok(
            &client
                .rpc(
                    "workflow.save",
                    json!({"source":"user","document":document}),
                )
                .await,
        );
        let listed = client.rpc("workflow.list", json!({"cwd":home})).await;
        assert_ok(&listed);
        assert_eq!(listed["result"]["definitions"][0]["valid"], true);
        let started = client
            .rpc(
                "workflow.start",
                json!({"name":"review","cwd":home,"params":{"target":"file.ts"},
            "report_to":{"session_id":starter,"branch_id":"main"}}),
            )
            .await;
        assert_ok(&started);
        let run = settled(&mut client, &started["result"]["run_id"]).await;
        assert_eq!(run["state"], "completed", "{run}");
        assert_eq!(run["stages"].as_array().unwrap().len(), 3);
        assert_eq!(run["policy_cap"]["preset"], "read-only");
        for stage in run["stages"].as_array().unwrap() {
            let session = agent
                .get_session(stage["session_id"].as_str().unwrap())
                .unwrap();
            assert!(!session.config().interactive);
            assert_eq!(
                session
                    .config()
                    .tools
                    .iter()
                    .flatten()
                    .any(|name| name == "workflow"),
                include_workflow
            );
            assert_eq!(
                session.config().model.provider_id,
                "018ec73f-0000-7000-8000-000000000007"
            );
            assert_eq!(session.config().model.model_id, "fixture");
            assert_eq!(
                session.config().permission_policy.preset,
                dim_agent_types::PermissionPreset::ReadOnly
            );
            assert!(stage["result_text"].as_str().unwrap().contains("file.ts"));
            // The stage's request carries the stage marker and, from the same
            // Session part, the dim-workflow entry.
            let system = super::reload::last_system(&session, "main").await;
            assert!(system.contains(WORKFLOW_ENTRY), "{system}");
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while stored(&home)["runs"][0]["report"]["state"] != "delivered" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let reports = agent
            .get_session(starter)
            .unwrap()
            .local("main", ..)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| {
                if let EventKind::InputMessage(data) = &event.kind
                    && data.blocks.iter().any(|block| {
                        matches!(block,
                        dim_agent_types::InputBlock::Raw { text, .. }
                        if text.starts_with("<dim-workflow kind=\"report\"")
                            && text.contains(started["result"]["run_id"].as_str().unwrap()))
                    })
                {
                    assert!(
                        data.user.is_none(),
                        "the report is a single raw context block"
                    );
                    true
                } else {
                    false
                }
            })
            .count();
        assert_eq!(
            reports, 1,
            "one published report in the actual starter Timeline"
        );
        // The request that consumes the report carries its entry.
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":starter,"branchId":"main","text":"Check the workflow result"}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":starter,"branchId":"main"}),
                )
                .await,
        );
        let system = super::reload::last_system(&agent.get_session(starter).unwrap(), "main").await;
        assert!(system.contains(WORKFLOW_ENTRY), "{system}");
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

async fn tool_result(
    client: &mut Client,
    session: &dim_agent_runtime::Session,
    arguments: Value,
) -> ToolResult {
    let id = session.id();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main",
        "text":format!("tool:workflow {arguments}")}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    session
        .local("main", ..)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|event| {
            if let EventKind::ToolResult(batch) = &event.kind {
                batch.results.first().map(|entry| entry.result.clone())
            } else {
                None
            }
        })
        .expect("Workflow tool result")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_project_enablement_invalid_sources_and_tool_policy() {
    let home = std::env::temp_dir().join(format!("dim-workflow-sources-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let package_a = home.join("package-a");
    let package_b = home.join("package-b");
    let packaged = json!({"schema":"dim.workflow/v1","name":"shared","description":"Package source",
        "steps":[{"stage":{"id":"one","prompt":"package contents"}}]});
    for package in [&package_a, &package_b] {
        std::fs::create_dir_all(package.join("workflows")).unwrap();
        std::fs::write(
            package.join("workflows/shared.yaml"),
            serde_json::to_vec(&packaged).unwrap(),
        )
        .unwrap();
    }
    let mut unique = packaged.clone();
    unique["name"] = json!("packaged");
    std::fs::write(
        package_a.join("workflows/packaged.yaml"),
        serde_json::to_vec(&unique).unwrap(),
    )
    .unwrap();
    let mut parts = probe_rows();
    parts.rows[0]["config"]["packages"] = json!([
        {"id":"a","root":package_a},{"id":"b","root":package_b}
    ]);
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(parts),
        file_roots: vec![package_a, package_b],
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "workflow".into()]);
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let created = client.rpc("session.create", json!({"cwd":home,"tools":["workflow"],
        "permissionPolicy":{"preset":"workspace-write","allowedTools":["workflow"],"disallowedTools":[]}})).await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    ready(&mut client).await;
    let document = json!({"schema":"dim.workflow/v1","name":"project","description":"Project source",
        "steps":[{"stage":{"id":"one","prompt":"first contents"}}]});
    assert_ok(
        &client
            .rpc(
                "workflow.save",
                json!({"source":"project","cwd":home,"document":document}),
            )
            .await,
    );
    let near_limit_path = home.join(".dimcode/workflows/near-limit.yaml");
    {
        let mut near_limit = document.clone();
        near_limit["name"] = json!("near-limit");
        let definition = serde_json::to_vec(&near_limit).unwrap();
        let mut bytes = vec![b'x'; 8 * 1024 * 1024 - 1];
        bytes[0] = b'#';
        let offset = bytes.len() - definition.len();
        bytes[offset - 1] = b'\n';
        bytes[offset..].copy_from_slice(&definition);
        std::fs::write(&near_limit_path, bytes).unwrap();
    }
    for (path, length) in [
        (
            home.join(".dimcode/workflows/oversized.yaml"),
            13 * 1024 * 1024,
        ),
        (
            home.join("package-a/workflows/packaged-large.yaml"),
            8 * 1024 * 1024 + 1,
        ),
    ] {
        std::fs::File::create(path)
            .unwrap()
            .set_len(length)
            .unwrap();
    }
    let listed = client.rpc("workflow.list", json!({"cwd":home})).await;
    assert_ok(&listed);
    let entries = listed["result"]["definitions"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry["name"] == "project")
            .unwrap()["enabled"],
        false
    );
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry["name"] == "near-limit")
            .unwrap()["valid"],
        true
    );
    std::fs::remove_file(near_limit_path).unwrap();
    {
        let path = home.join(".dimcode/workflows/project.yaml");
        let original = std::fs::read(&path).unwrap();
        let mut oversized = document.clone();
        oversized["description"] = json!("x".repeat(13 * 1024 * 1024));
        let rejected = client
            .rpc(
                "workflow.save",
                json!({"source":"project","cwd":home,"document":oversized}),
            )
            .await;
        assert_eq!(rejected["error"]["data"]["appError"], "invalid_definition");
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
    let refused = client
        .rpc("workflow.start", json!({"name":"project","cwd":home}))
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "definition_not_found");
    assert_eq!(stored(&home)["runs"], json!([]));
    std::fs::write(
        home.join(".dimcode/workflows/mismatched.yaml"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    let settings = client
        .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
        .await;
    assert_ok(&settings);
    let mut source = settings["result"]["document"].clone();
    source["projectEnabled"] =
        json!({home.to_str().unwrap():{"project":true,"mismatched":true,"oversized":true}});
    source["max_concurrent_runs"] = json!(2);
    source["max_concurrent_stages"] = json!(1);
    let mut invalid = document.clone();
    invalid["name"] = json!("invalid");
    invalid["steps"][0]["stage"]["prompt"] = json!("{{stages.missing.result}}");
    let mut many = document.clone();
    many["name"] = json!("many");
    many["steps"][0]["stage"]["prompt"] = json!("{{params.missing}}".repeat(20));
    let mut fallback = document.clone();
    fallback["name"] = json!("mismatched");
    source["definitions"] =
        json!({"invalid":invalid,"mismatch":document,"many":many,"mismatched":fallback});
    assert_ok(
        &client
            .rpc(
                "settings.update",
                json!({"source":"workflow","scope":"user",
        "expectedRevision":settings["result"]["revision"],"document":source}),
            )
            .await,
    );
    let revision = client
        .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
        .await;
    let listed = client.rpc("workflow.list", json!({"cwd":home})).await;
    assert_ok(&listed);
    for name in [
        "invalid",
        "mismatch",
        "many",
        "mismatched",
        "shared",
        "oversized",
        "packaged-large",
    ] {
        let entry = listed["result"]["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| {
                entry["name"] == name && (name != "mismatched" || entry["source"] == "project")
            })
            .unwrap();
        assert_eq!(entry["valid"], false);
        assert!(entry["issue_count"].as_u64().unwrap() > 0);
        assert_eq!(
            client
                .rpc("workflow.start", json!({"name":name,"cwd":home}))
                .await["error"]["data"]["appError"],
            "invalid_definition"
        );
    }
    let result = tool_result(&mut client, &starter, json!({"action":"list"})).await;
    let dim_agent_types::ToolContent::Text(content) = &result.content else {
        panic!("Workflow returns text")
    };
    assert!(content.contains("invalid reference"));
    assert!(content.contains("Document name must match mismatch"));
    let visible: Value = serde_json::from_str(content).unwrap();
    let definitions = visible["definitions"].as_array().unwrap();
    assert_eq!(visible["truncated"], false);
    let many = definitions
        .iter()
        .find(|entry| entry["name"] == "many")
        .unwrap();
    assert_eq!(many["issue_count"], 20);
    assert_eq!(many["issues"].as_array().unwrap().len(), 10);
    assert_eq!(
        definitions
            .iter()
            .filter(|entry| entry["name"] == "shared")
            .map(|entry| entry["package_id"].clone())
            .collect::<Vec<_>>(),
        vec![json!("a"), json!("b")]
    );
    let invalid = tool_result(
        &mut client,
        &starter,
        json!({"action":"start","name":"invalid"}),
    )
    .await;
    assert_eq!(invalid.is_error, Some(true));
    let dim_agent_types::ToolContent::Text(content) = invalid.content else {
        panic!("Workflow returns text")
    };
    assert!(content.contains("Use list"));
    assert_eq!(
        stored(&home)["runs"],
        json!([]),
        "invalid definitions create no run or stage"
    );
    assert_eq!(
        client
            .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
            .await["result"]["revision"],
        revision["result"]["revision"]
    );
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "workflowprobe.configure",
                json!({"phase":"stage_input",
        "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
            )
            .await,
    );
    let started = tool_result(
        &mut client,
        &starter,
        json!({"action":"start","name":"project"}),
    )
    .await;
    assert_ne!(started.is_error, Some(true), "{started:?}");
    let id = started.structured_content.as_ref().unwrap()["run_id"].clone();
    let dim_agent_types::ToolContent::Text(content) = &started.content else {
        panic!("Workflow returns text")
    };
    assert!(content.contains(id.as_str().unwrap()));
    let (mut release, _) = tokio::time::timeout(Duration::from_secs(5), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let unrelated = client.rpc("session.create", json!({"cwd":home,"tools":["workflow"],
        "permissionPolicy":{"preset":"workspace-write","allowedTools":["workflow"],"disallowedTools":[]}})).await;
    assert_ok(&unrelated);
    let unrelated = agent
        .get_session(unrelated["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    for action in ["status", "stop"] {
        let denied = tool_result(
            &mut client,
            &unrelated,
            json!({"action":action,"run_id":id}),
        )
        .await;
        assert_eq!(denied.is_error, Some(true));
        let dim_agent_types::ToolContent::Text(content) = denied.content else {
            panic!("Workflow returns text")
        };
        assert!(content.contains("run_not_found"));
        assert!(denied.structured_content.is_none());
    }
    assert_eq!(
        client.rpc("workflow.status", json!({"run_id":id})).await["result"]["state"],
        "running"
    );
    let related = tool_result(
        &mut client,
        &starter,
        json!({"action":"status","run_id":id}),
    )
    .await;
    assert_ne!(related.is_error, Some(true));
    assert_eq!(
        related.structured_content.as_ref().unwrap()["run"]["run_id"],
        id
    );
    let mut edited = document.clone();
    edited["steps"][0]["stage"]["prompt"] = json!("edited contents");
    assert_ok(
        &client
            .rpc(
                "workflow.save",
                json!({"source":"project","cwd":home,"document":edited}),
            )
            .await,
    );
    release.write_all(b"input").await.unwrap();
    drop(release);
    let run = settled(&mut client, &id).await;
    assert_eq!(run["policy_cap"]["preset"], "workspace-write");
    assert!(
        run["stages"][0]["result_text"]
            .as_str()
            .unwrap()
            .contains("first contents")
    );
    for (name, selected, expected) in [
        ("project", "project", "edited contents"),
        ("packaged", "plugin", "package contents"),
        ("mismatched", "user", "first contents"),
    ] {
        let started = client
            .rpc(
                "workflow.start",
                json!({"name":name,"source":selected,"cwd":home}),
            )
            .await;
        assert_ok(&started);
        let run = settled(&mut client, &started["result"]["run_id"]).await;
        assert_eq!(run["source"], selected);
        assert!(
            run["stages"][0]["result_text"]
                .as_str()
                .unwrap()
                .contains(expected)
        );
    }
    let mut nested = document.clone();
    nested["name"] = json!("nested");
    nested["policy"] = json!("full_access");
    nested["steps"][0]["stage"]["prompt"] = json!("tool:workflow {\"action\":\"list\"}");
    assert_ok(
        &client
            .rpc("workflow.save", json!({"source":"user","document":nested}))
            .await,
    );
    let started = tool_result(
        &mut client,
        &starter,
        json!({"action":"start","name":"nested"}),
    )
    .await;
    assert_ne!(started.is_error, Some(true), "{started:?}");
    let run = settled(
        &mut client,
        &started.structured_content.as_ref().unwrap()["run_id"],
    )
    .await;
    assert!(
        run["stages"][0]["result_text"]
            .as_str()
            .unwrap()
            .contains("only in interactive Sessions"),
        "{run}"
    );
    let denied = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["workflow"],
        "permissionPolicy":{"preset":"read-only"}}),
        )
        .await;
    assert_ok(&denied);
    let session = agent
        .get_session(denied["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let result = tool_result(&mut client, &session, json!({"action":"list"})).await;
    assert_eq!(
        result.is_error,
        Some(true),
        "ReadOnly denies Workflow's unclassified tool risk"
    );
    let settings = client
        .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
        .await;
    assert_ok(&settings);
    assert_eq!(settings["result"]["document"]["max_concurrent_runs"], 2);
    assert_eq!(settings["result"]["document"]["max_concurrent_stages"], 1);
    assert_eq!(
        settings["result"]["document"]["projectEnabled"],
        source["projectEnabled"]
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_rejects_invalid_inner_records_without_changing_bytes() {
    let record = json!({
        "run_id":"record", "name":"record", "source":"user", "params":{}, "cwd":"/tmp",
        "definition":{"schema":"dim.workflow/v1","name":"record","description":"Records",
            "steps":[{"stage":{"id":"one","prompt":"done"}}]},
        "started_by":"management", "created_at":"2026-10-22T00:00:00.000Z",
        "ended_at":"2026-10-22T00:00:01.000Z", "state":"completed", "log":[],
        "stages":[{"id":"one","step":0,"state":"completed","result_text":"done"}],
        "report_to":{"session_id":"starter","branch_id":"main"},
        "report":{"state":"pending","submission_id":"workflow:record"}
    });
    for inner in [
        json!({"version":1,"runs":[]}).to_string(),
        json!({"version":99,"runs":[]}).to_string(),
        json!({"version":2,"runs":[record]}).to_string(),
        "{broken".into(),
    ] {
        let home =
            std::env::temp_dir().join(format!("dim-workflow-invalid-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("workflow")).unwrap();
        let bytes = serde_json::to_vec(&json!({"version":1,"text":inner})).unwrap();
        let path = home.join("workflow/runs.json");
        std::fs::write(&path, &bytes).unwrap();
        let (server, _) = launch_with_agent(
            Arc::new(MockTools),
            ServerConfig {
                product_home: Some(home.clone()),
                database_path: Some(home.join("native.sqlite")),
                session_typescript: Some(rows()),
                ..Default::default()
            },
        )
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = client.rpc("workflow.status", json!({})).await;
                if session_typescript::waiting(&reply) {
                    tokio::task::yield_now().await;
                    continue;
                }
                assert_eq!(
                    reply["error"]["data"]["appError"], "invalid_data",
                    "{reply}"
                );
                break;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "invalid inner bytes are never reset or migrated"
        );
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_captures_live_first_admission_and_survives_starter_and_index_changes() {
    let home = std::env::temp_dir().join(format!("dim-workflow-policy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(probe_rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "workflow".into()]);
    config.session_defaults.permission_policy =
        serde_json::from_value(json!({"preset":"full-access"})).unwrap();
    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    ready(&mut client).await;
    assert_ok(&client.rpc("workflow.save", json!({"source":"user","document":{
        "schema":"dim.workflow/v1","name":"policy","description":"Admission authority","policy":"full_access",
        "steps":[{"parallel":[{"id":"a","prompt":"first"},{"id":"b","prompt":"second"}]}]}})).await);
    for close_before_admission in [false, true] {
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"tools":["workflow"],
            "permissionPolicy":{"preset":"full-access"}}),
            )
            .await;
        assert_ok(&created);
        let starter = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let (_, _, loader, _) = controls.recv().await.unwrap();
        ready(&mut client).await;
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "workflowprobe.configure",
                    json!({"phase":"before_launch","count":2,
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
                )
                .await,
        );
        let started = tool_result(
            &mut client,
            &starter,
            json!({"action":"start","name":"policy"}),
        )
        .await;
        assert_ne!(started.is_error, Some(true), "{started:?}");
        let id = &started.structured_content.as_ref().unwrap()["run_id"];
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let run = client.rpc("workflow.status", json!({"run_id":id})).await;
        assert_ok(&run);
        assert!(run["result"].get("policy_cap").is_none());
        assert!(
            run["result"]["stages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|stage| stage.get("session_id").is_none())
        );
        if close_before_admission {
            assert_ok(
                &client
                    .rpc("session.close", json!({"sessionId":starter.id()}))
                    .await,
            );
            release.write_all(b"launch").await.unwrap();
            drop(release);
            let run = settled(&mut client, id).await;
            assert_eq!(run["state"], "failed", "{run}");
            assert!(run.get("policy_cap").is_none());
            assert!(
                run["stages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|stage| stage.get("session_id").is_none())
            );
            continue;
        }
        let policy = json!({"preset":"workspace-write","allowedTools":["workflow"],"disallowedTools":["wait"]});
        assert_ok(
            &client
                .rpc(
                    "session.permission.update",
                    json!({"sessionId":starter.id(),"branchId":"main",
            "expectedVersion":0,"policy":policy}),
                )
                .await,
        );
        release.write_all(b"launch").await.unwrap();
        drop(release);
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let run = client.rpc("workflow.status", json!({"run_id":id})).await;
        assert_ok(&run);
        assert_eq!(run["result"]["policy_cap"], policy);
        assert!(
            run["result"]["stages"][1].get("session_id").is_none(),
            "first cap precedes sibling admission"
        );
        let first = agent
            .get_session(run["result"]["stages"][0]["session_id"].as_str().unwrap())
            .unwrap();
        let original_instance = first.instance().unwrap();
        let owner = loader.get("workflow").unwrap().view.unwrap();
        server.restart("Agents").await.unwrap();
        assert_eq!(owner.state().state, rutis::host::FiberState::Active);
        assert_eq!(
            agent.get_session(first.id()).unwrap().instance().unwrap(),
            original_instance
        );
        assert_eq!(
            client.rpc("workflow.status", json!({"run_id":id})).await["result"]["policy_cap"],
            policy
        );
        assert_ok(
            &client
                .rpc("session.close", json!({"sessionId":starter.id()}))
                .await,
        );
        release.write_all(b"launch").await.unwrap();
        drop(release);
        let run = settled(&mut client, id).await;
        assert_eq!(run["state"], "completed", "{run}");
        assert_eq!(run["policy_cap"], policy);
        for stage in run["stages"].as_array().unwrap() {
            let session = agent
                .get_session(stage["session_id"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                serde_json::to_value(&session.config().permission_policy).unwrap(),
                policy
            );
            assert!(!session.config().interactive);
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = client.rpc("workflow.status", json!({"run_id":id})).await;
                if status["result"]["report"]["state"] == "undeliverable" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let _ = controls.recv().await.unwrap();
        let _ = controls.recv().await.unwrap();
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_stop_and_reload_settle_held_consumed_and_completed_work() {
    let home = std::env::temp_dir().join(format!("dim-workflow-stop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(probe_rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["confirm".into(), "workflow".into()]);
    config.session_defaults.permission_policy =
        serde_json::from_value(json!({"preset":"workspace-write"})).unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let (server, agent) = launch_with(
        Arc::new(ConfirmTool {
            calls: Arc::default(),
            seen: Arc::default(),
        }),
        config,
        |builder| {
            builder.require_feature(session_typescript::ControlledGate::feature(send));
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["workflow"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    ready(&mut client).await;
    for mode in ["held", "consumed", "completed", "reload"] {
        let prompt = if mode == "consumed" || mode == "reload" {
            "tool:confirm {}"
        } else {
            "stage result"
        };
        assert_ok(&client.rpc("workflow.save", json!({"source":"user","document":{
            "schema":"dim.workflow/v1","name":mode,"description":"Owned stop","policy":"workspace_write",
            "steps":[{"stage":{"id":"one","prompt":prompt}},{"stage":{"id":"later","prompt":"must not launch"}}]}})).await);
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "workflowprobe.configure",
                    json!({"phase":if mode == "held" {"stage_input"} else {"stage_history"},
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
                )
                .await,
        );
        let started =
            tool_result(&mut client, &starter, json!({"action":"start","name":mode})).await;
        assert_ne!(started.is_error, Some(true), "{started:?}");
        let id = &started.structured_content.as_ref().unwrap()["run_id"];
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let status = client.rpc("workflow.status", json!({"run_id":id})).await;
        assert_ok(&status);
        let stage = agent
            .get_session(
                status["result"]["stages"][0]["session_id"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        if mode == "held" {
            assert_ok(
                &client
                    .rpc(
                        "session.submit",
                        json!({"sessionId":stage.id(),"branchId":"main","text":"tool:confirm {}"}),
                    )
                    .await,
            );
        }
        if mode == "completed" {
            stage.idle("main").await.unwrap();
        } else {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !stage
                    .local("main", ..)
                    .await
                    .unwrap()
                    .iter()
                    .any(|event| matches!(&event.kind, EventKind::ApprovalRequest(_)))
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        if mode == "held" {
            let history_gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            assert_ok(
                &client
                    .rpc(
                        "workflowprobe.configure",
                        json!({"phase":"stage_history",
                "host":"127.0.0.1","port":history_gate.local_addr().unwrap().port()}),
                    )
                    .await,
            );
            release.write_all(b"input").await.unwrap();
            drop(release);
            release = tokio::time::timeout(Duration::from_secs(5), history_gate.accept())
                .await
                .unwrap()
                .unwrap()
                .0;
        }
        let mut idle = tokio::spawn({
            let stage = stage.clone();
            async move { stage.idle("main").await }
        });
        if mode != "completed" {
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut idle)
                    .await
                    .is_err(),
                "the real approval holds idle pending"
            );
        }
        if mode == "reload" {
            let original_instance = stage.instance().unwrap();
            let stops_before_reload = stage
                .local("main", ..)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(&event.kind, EventKind::StopInterrupt(_)))
                .count();
            let sessions = client.rpc("session.list", json!({})).await;
            assert_ok(&sessions);
            let probe = client.rpc("workflowprobe.status", json!({})).await;
            assert_ok(&probe);
            kill_node(probe["result"]["pid"].as_u64().unwrap()).await;
            drop(release);
            loader.restart("session-node").await.unwrap();
            ready(&mut client).await;
            let run = settled(&mut client, id).await;
            assert_eq!(run["state"], "interrupted", "{run}");
            assert_eq!(run["stages"][0]["state"], "lost");
            assert_eq!(run["stages"][0]["session_id"], stage.id());
            assert_eq!(run["stages"][1]["state"], "skipped");
            assert_eq!(
                agent.get_session(stage.id()).unwrap().instance().unwrap(),
                original_instance
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                while client.rpc("workflow.status", json!({"run_id":id})).await["result"]["report"]
                    ["state"]
                    != "delivered"
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(report_count(&starter, id.as_str().unwrap()).await, 1);
            assert_eq!(
                client.rpc("session.list", json!({})).await["result"]
                    .as_array()
                    .unwrap()
                    .len(),
                sessions["result"].as_array().unwrap().len(),
                "recovery launches no stage Session"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut idle)
                    .await
                    .is_err(),
                "recovery leaves the stage's existing approval and turn untouched"
            );
            assert_eq!(
                stage
                    .local("main", ..)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|event| matches!(&event.kind, EventKind::StopInterrupt(_)))
                    .count(),
                stops_before_reload
            );
            assert_ok(
                &client
                    .rpc(
                        "session.stop",
                        json!({"sessionId":stage.id(),"branchId":"main"}),
                    )
                    .await,
            );
            tokio::time::timeout(Duration::from_secs(5), idle)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                client.rpc("workflow.status", json!({"run_id":id})).await["result"]["stages"][0]["state"],
                "lost"
            );
            continue;
        }
        let stopping = client.send("workflow.stop", json!({"run_id":id})).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.rpc("workflow.status", json!({"run_id":id})).await["result"]["state"]
                != "stopping"
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        release.write_all(b"history").await.unwrap();
        drop(release);
        let stopped = tokio::time::timeout(Duration::from_secs(5), client.response(stopping))
            .await
            .unwrap();
        assert_ok(&stopped);
        assert_eq!(stopped["result"]["state"], "stopped", "{stopped}");
        assert_eq!(stopped["result"]["stages"][1]["state"], "skipped");
        assert_eq!(stopped["result"]["report"]["state"], "none");
        assert_eq!(report_count(&starter, id.as_str().unwrap()).await, 0);
        let events = stage.local("main", ..).await.unwrap();
        let own = stopped["result"]["stages"][0]["input_event_id"]
            .as_str()
            .unwrap();
        let interrupts = events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::StopInterrupt(_)))
            .count();
        if mode == "held" {
            assert_eq!(stopped["result"]["stages"][0]["state"], "stopped");
            assert!(events.iter().any(
                |event| matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == own)
            ));
            assert_eq!(
                interrupts, 0,
                "withdrawing our held input must not interrupt the unrelated approval"
            );
            assert!(!idle.is_finished());
            stage
                .interrupt("main", "end the test's unrelated approval".into())
                .unwrap();
        } else if mode == "consumed" {
            assert_eq!(stopped["result"]["stages"][0]["state"], "stopped");
            assert_eq!(interrupts, 1);
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == own)));
            assert!(
                events
                    .iter()
                    .any(|event| matches!(&event.kind, EventKind::ApprovalCancelled(_)))
            );
        } else {
            assert_eq!(stopped["result"]["stages"][0]["state"], "completed");
            assert!(
                stopped["result"]["stages"][0]["result_text"]
                    .as_str()
                    .unwrap()
                    .contains("stage result")
            );
            assert_eq!(
                interrupts, 0,
                "the committed turn result wins over a later stop"
            );
        }
        tokio::time::timeout(Duration::from_secs(5), idle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            client.rpc("workflow.stop", json!({"run_id":id})).await["result"],
            stopped["result"],
            "repeated stop is idempotent"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_report_wakes_wait_and_is_adopted_after_its_start_batch() {
    use dim_agent_capabilities::{
        adapters::{DefaultPrepareModelPlugin, EchoModel},
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelChunk, ModelConnection,
            ModelDriver, ModelHost, ModelStopReason, ModelStream, PrepareContextHost,
        },
    };
    use dim_agent_types::{MessageContent, MessagePart, ModelRequest, Role};
    struct BatchModel;
    impl ModelDriver for BatchModel {
        fn invoke(
            &self,
            request: Arc<ModelRequest>,
            connection: ModelConnection,
            cancel: CancellationToken,
        ) -> HostFuture<'_, ModelStream> {
            let batch = request.messages.last().is_some_and(|message| message.role == Role::User && match &message.content {
                MessageContent::Text(text) => text == "workflow batch",
                MessageContent::Parts(parts) => parts.iter().any(|part| matches!(part, MessagePart::Text {text, ..} if text == "workflow batch")),
            });
            if !batch {
                return EchoModel.invoke(request, connection, cancel);
            }
            Box::pin(async {
                let mut chunks = vec![ModelChunk::MessageStart {
                    message_id: "batch".into(),
                }];
                for (id, name, input) in [
                    (
                        "start",
                        "workflow",
                        json!({"action":"start","name":"batch"}),
                    ),
                    ("wait", "wait", json!({"timeoutMs":10000})),
                ] {
                    chunks.push(ModelChunk::ToolStart {
                        id: id.into(),
                        name: name.into(),
                    });
                    chunks.push(ModelChunk::ToolUse(ToolCall {
                        id: id.into(),
                        name: name.into(),
                        input: serde_json::from_value(input).unwrap(),
                    }));
                }
                chunks.push(ModelChunk::MessageStop {
                    message_id: "batch".into(),
                    reason: ModelStopReason::ToolUse,
                });
                Ok(Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok))) as ModelStream)
            })
        }
    }
    let home = std::env::temp_dir().join(format!("dim-workflow-batch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let mut config = ServerConfig {
        unix_path: Some(PathBuf::from("/tmp").join(format!(
            "dim-workflow-batch-{}/server.sock",
            uuid::Uuid::new_v4()
        ))),
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(probe_rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["workflow".into(), "wait".into()]);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    dim_agent_capabilities::wait::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(BatchModel))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(
        &mut builder,
        Arc::new(ConfirmTool {
            calls: Arc::default(),
            seen: Arc::default(),
        }),
    );
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    ready(&mut client).await;
    assert_ok(
        &client
            .rpc(
                "workflow.save",
                json!({"source":"user","document":{
        "schema":"dim.workflow/v1","name":"batch","description":"Batch report",
        "steps":[{"stage":{"id":"one","prompt":"stage finished"}}]}}),
            )
            .await,
    );
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["workflow","wait"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":starter.id(),"branchId":"main","text":"workflow batch"}),
            )
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), starter.idle("main"))
        .await
        .unwrap()
        .unwrap();
    let events = starter.local("main", ..).await.unwrap();
    let (result, batch) = events
        .iter()
        .find_map(|event| {
            if let EventKind::ToolResult(data) = &event.kind
                && data.results.iter().any(|entry| entry.call_id == "start")
            {
                Some((event, data))
            } else {
                None
            }
        })
        .expect("both tools settle in their actual batch");
    let start = &batch
        .results
        .iter()
        .find(|entry| entry.call_id == "start")
        .unwrap()
        .result;
    assert_ne!(start.is_error, Some(true), "{start:?}");
    let run = &start.structured_content.as_ref().unwrap()["run_id"];
    let wait = &batch
        .results
        .iter()
        .find(|entry| entry.call_id == "wait")
        .unwrap()
        .result;
    assert_eq!(
        wait.structured_content.as_ref().unwrap()["reason"],
        "user_steer"
    );
    let report = events.iter().find(|event| matches!(&event.kind, EventKind::InputMessage(data)
        if data.blocks.iter().any(|block| matches!(block, dim_agent_types::InputBlock::Raw {text, ..}
            if text.starts_with("<dim-workflow kind=\"report\"") && text.contains(run.as_str().unwrap()))))).unwrap();
    let adopted = events.iter().find(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == report.id)).unwrap();
    assert!(
        report.sequence < result.sequence,
        "the report wakes the batch's wait"
    );
    assert!(
        result.sequence < adopted.sequence,
        "the model receives the run id before adopting the report"
    );
    assert_eq!(report_count(&starter, run.as_str().unwrap()).await, 1);
    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "workflowprobe.configure",
                json!({"phase":"stage_input",
        "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":starter.id(),"branchId":"main","text":"workflow batch"}),
            )
            .await,
    );
    let (mut release, _) = tokio::time::timeout(Duration::from_secs(5), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let running = client.rpc("workflow.status", json!({})).await;
    assert_ok(&running);
    let interrupted = running["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["state"] == "running")
        .unwrap()["run_id"]
        .clone();
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId":starter.id(),"branchId":"main"}),
            )
            .await,
    );
    assert_eq!(
        client
            .rpc("workflow.status", json!({"run_id":interrupted}))
            .await["result"]["state"],
        "running",
        "a starter interrupt cannot revoke accepted process work"
    );
    release.write_all(b"input").await.unwrap();
    drop(release);
    assert_eq!(
        settled(&mut client, &interrupted).await["state"],
        "completed"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while client
            .rpc("workflow.status", json!({"run_id":interrupted}))
            .await["result"]["report"]["state"]
            != "delivered"
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        report_count(&starter, interrupted.as_str().unwrap()).await,
        1
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

async fn native_retention(fault: &str) {
    use std::os::unix::fs::PermissionsExt;
    let home =
        std::env::temp_dir().join(format!("dim-workflow-retention-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("workflow")).unwrap();
    let home = home.canonicalize().unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(probe_rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "workflow".into()]);
    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let external = client
        .rpc("session.create", json!({"cwd":home,"tools":[]}))
        .await;
    assert_ok(&external);
    let external = external["result"]["sessionId"].as_str().unwrap().to_owned();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    ready(&mut client).await;
    assert_ok(
        &client
            .rpc(
                "workflow.save",
                json!({"source":"user","document":{
        "schema":"dim.workflow/v1","name":"history","description":"Native retention",
        "steps":[{"stage":{"id":"one","prompt":"retained conversation"}}]}}),
            )
            .await,
    );
    let mut completed = Vec::new();
    for _ in 0..2 {
        let started = client
            .rpc("workflow.start", json!({"name":"history","cwd":home}))
            .await;
        assert_ok(&started);
        completed.push(settled(&mut client, &started["result"]["run_id"]).await);
    }
    let owned = completed[0]["stages"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let retained = completed[1]["stages"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let original = agent.get_session(&owned).unwrap();
    original.idle("main").await.unwrap();
    let cutoff = original
        .local("main", ..)
        .await
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    let fork = client
        .rpc("session.fork", json!({"sessionId":owned,"at":cutoff}))
        .await;
    assert_ok(&fork);
    let fork = fork["result"]["sessionId"].as_str().unwrap().to_owned();

    session_typescript::fixture_layer(
        &loader,
        "retention-fixture",
        Some(vec![
            serde_json::from_value(json!({"id":"workflow","disabled":true})).unwrap(),
        ]),
    )
    .await;
    assert!(loader.get("workflow").unwrap().view.is_none());
    let runs: Vec<_> = (0..102)
        .map(|index| {
            let mut run = completed[usize::from(index != 0)].clone();
            run["run_id"] = json!(format!("history-{index:03}"));
            run["created_at"] = json!("2026-01-01T00:00:00.000Z");
            run["ended_at"] = json!(format!("2026-01-01T00:00:00.{index:03}Z"));
            run
        })
        .collect();
    let path = home.join("workflow/runs.json");
    std::fs::write(
        &path,
        serde_json::to_vec(
            &json!({"version":1,"text":json!({"version":2,"runs":runs}).to_string()}),
        )
        .unwrap(),
    )
    .unwrap();
    let output = home.join("command-output").join(&owned).join("locked");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("exec.log"), b"owned output").unwrap();
    let permissions = std::fs::metadata(&output).unwrap().permissions();
    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        std::fs::write(output.join("probe"), b"")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    session_typescript::fixture_layer(&loader, "retention-fixture", None).await;
    ready(&mut client).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = stored(&home);
            if state["runs"].as_array().unwrap().len() == 101
                && state["runs"].as_array().unwrap().iter().any(|run| {
                    run["run_id"] == "history-000"
                        && run["log"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|entry| entry["message"] == "Retention delete failed: failed")
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let listed = client.rpc("session.list", json!({})).await;
    assert!(
        listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["sessionId"] == *retained),
        "the newest retained reference protects its native Session"
    );
    assert!(
        !stored(&home)["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run_id"] == "history-001")
    );
    let snapshot = client
        .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
        .await;
    assert_ok(&snapshot);
    let mut document = snapshot["result"]["document"].clone();
    document["definitions"] = json!({});
    assert_ok(
        &client
            .rpc(
                "settings.update",
                json!({"source":"workflow","scope":"user",
        "expectedRevision":snapshot["result"]["revision"],"document":document}),
            )
            .await,
    );
    let before = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&output, permissions).unwrap();

    if fault == "record" {
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "workflowprobe.configure",
                    json!({"phase":"delete_reply",
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}),
                )
                .await,
        );
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(75), gate.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !output.exists(),
            "the real Session deletion completes before record removal"
        );
        let directory = home.join("workflow");
        let mode = std::fs::metadata(&directory).unwrap().permissions();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        release.write_all(b"record").await.unwrap();
        drop(release);
        let observed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let probe = client.rpc("workflowprobe.status", json!({})).await;
                assert_ok(&probe);
                if probe["result"]["replacementError"] == "io" {
                    break probe;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let unchanged = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&directory, mode).unwrap();
        assert_eq!(
            unchanged, before,
            "failed atomic replacement retains the last ownership record"
        );
        kill_node(observed["result"]["pid"].as_u64().unwrap()).await;
        loader.restart("session-node").await.unwrap();
        ready(&mut client).await;
    } else if fault == "read" {
        let mode = std::fs::metadata(&path).unwrap().permissions();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let observed = tokio::time::timeout(Duration::from_secs(75), async {
            loop {
                let probe = client.rpc("workflowprobe.status", json!({})).await;
                assert_ok(&probe);
                if !probe["result"]["readError"].is_null() {
                    break probe;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        let outputs_preserved = output.join("exec.log").exists();
        std::fs::set_permissions(&path, mode).unwrap();
        assert_eq!(observed["result"]["readError"], "io");
        assert!(
            outputs_preserved,
            "a failed store read must prevent native deletion"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    tokio::time::timeout(Duration::from_secs(75), async {
        while stored(&home)["runs"].as_array().unwrap().len() != 100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("maintenance retries native cleanup without active runs or definitions");
    assert!(!output.exists());
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    let sessions = summaries["result"].as_array().unwrap();
    assert!(!sessions.iter().any(|session| session["sessionId"] == owned));
    for id in [&retained, &external, &fork] {
        assert!(sessions.iter().any(|session| session["sessionId"] == *id));
    }
    let forked = client
        .rpc(
            "session.history",
            json!({"sessionId":fork,"branchId":"main","after":0,"limit":1000}),
        )
        .await;
    assert_ok(&forked);
    assert!(
        forked["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "input" && event["name"] == "message"),
        "deleting the owned parent preserves the user's forked conversation"
    );
    let history = stored(&home);
    assert!(
        history["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["state"] == "completed")
    );
    assert_eq!(history["runs"][0]["run_id"], "history-002");

    if fault == "delete" {
        native_retention_live_references(&mut client, &agent, &home, &external).await;
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

async fn native_retention_live_references(
    client: &mut Client,
    agent: &dim_agent_runtime::Agent,
    home: &Path,
    destination: &str,
) {
    for name in ["active", "pending", "completed"] {
        assert_ok(
            &client
                .rpc(
                    "workflow.save",
                    json!({"source":"user","document":{
            "schema":"dim.workflow/v1","name":name,"description":"Protected retention",
            "steps":[{"stage":{"id":"one","prompt":name}}]}}),
                )
                .await,
        );
    }
    let active_gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "workflowprobe.configure",
                json!({"phase":"stage_history",
        "host":"127.0.0.1","port":active_gate.local_addr().unwrap().port()}),
            )
            .await,
    );
    let active = client
        .rpc("workflow.start", json!({"name":"active","cwd":home}))
        .await;
    assert_ok(&active);
    let (mut active_release, _) =
        tokio::time::timeout(Duration::from_secs(10), active_gate.accept())
            .await
            .unwrap()
            .unwrap();
    let report_gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "workflowprobe.configure",
                json!({"phase":"before_input",
        "host":"127.0.0.1","port":report_gate.local_addr().unwrap().port()}),
            )
            .await,
    );
    let pending = client
        .rpc(
            "workflow.start",
            json!({"name":"pending","cwd":home,
        "report_to":{"session_id":destination,"branch_id":"main"}}),
        )
        .await;
    assert_ok(&pending);
    let (mut report_release, _) =
        tokio::time::timeout(Duration::from_secs(10), report_gate.accept())
            .await
            .unwrap()
            .unwrap();
    let completed = client
        .rpc("workflow.start", json!({"name":"completed","cwd":home}))
        .await;
    assert_ok(&completed);
    settled(client, &completed["result"]["run_id"]).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(home)["runs"].as_array().unwrap().len() != 102 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let state = client.rpc("workflow.status", json!({})).await;
    assert_ok(&state);
    for (id, report) in [
        (&active["result"]["run_id"], "none"),
        (&pending["result"]["run_id"], "pending"),
    ] {
        let run = state["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["run_id"] == *id)
            .unwrap();
        assert_eq!(run["report"]["state"], report);
        assert!(
            agent
                .get_session(run["stages"][0]["session_id"].as_str().unwrap())
                .is_some()
        );
    }
    assert_eq!(
        state["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["run_id"] == active["result"]["run_id"])
            .unwrap()["state"],
        "running"
    );
    let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let document: String = database
        .query_row(
            "SELECT document FROM settings_documents WHERE source='user/workflow'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        database
            .execute(
                "UPDATE settings_documents SET document='{' WHERE source='user/workflow'",
                []
            )
            .unwrap(),
        1
    );
    let unavailable = client.rpc("workflow.list", json!({"cwd":home})).await;
    report_release.write_all(b"report").await.unwrap();
    drop(report_release);
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(home)["runs"].as_array().unwrap().len() != 101 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let retained = stored(home);
    active_release.write_all(b"history").await.unwrap();
    drop(active_release);
    settled(client, &active["result"]["run_id"]).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(home)["runs"].as_array().unwrap().len() != 100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let preserved: String = database
        .query_row(
            "SELECT document FROM settings_documents WHERE source='user/workflow'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    database
        .execute(
            "UPDATE settings_documents SET document=?1 WHERE source='user/workflow'",
            [&document],
        )
        .unwrap();
    assert_eq!(
        unavailable["error"]["data"]["appError"],
        "definition_source_failed"
    );
    assert_eq!(
        preserved, "{",
        "retention never resets the unreadable definition source"
    );
    assert!(
        retained["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run_id"] == active["result"]["run_id"] && run["state"] == "running")
    );
    assert!(
        retained["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run_id"] == pending["result"]["run_id"]
                && run["report"]["state"] == "delivered")
    );
    assert_eq!(
        report_count(
            &agent.get_session(destination).unwrap(),
            pending["result"]["run_id"].as_str().unwrap()
        )
        .await,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_retention_retries_native_deletion_and_preserves_live_references() {
    native_retention("delete").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_retention_recovers_native_deletion_before_failed_record_removal() {
    native_retention("record").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_retention_retries_failed_native_store_reads_without_deleting_sessions() {
    native_retention("read").await;
}

/// The report's guidance line and the `invalid_definition` recovery hint
/// as the factory configuration: a candidate replaces each built-in text,
/// and unset keeps the built-ins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_factory_texts_reach_the_report_and_the_refusal() {
    const GUIDANCE: &str = "Ask workflow status with {run_id} later.";
    const RECOVERY: &str = "Open list to read the issues.";
    for configured in [true, false] {
        let home =
            std::env::temp_dir().join(format!("dim-workflow-factory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("workflow")).unwrap();
        let home: PathBuf = home.canonicalize().unwrap();
        let mut parts = rows();
        if configured {
            parts.rows[0]["config"] = json!({
                "packages": [],
                "report_guidance": GUIDANCE,
                "invalid_definition_guidance": RECOVERY,
            });
        }
        let mut config = ServerConfig {
            product_home: Some(home.clone()),
            database_path: Some(home.join("native.sqlite")),
            session_typescript: Some(parts),
            ..Default::default()
        };
        config.session_defaults.tools = Some(vec!["echo".into(), "wait".into(), "workflow".into()]);
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"tools":["workflow","wait","echo"],
                    "permissionPolicy":{"preset":"full-access"}}),
            )
            .await;
        assert_ok(&created);
        let starter = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let starter_session = agent.get_session(&starter).unwrap();
        ready(&mut client).await;

        // Two user definitions: one the catalog keeps but reports issues for,
        // and one that runs and reports to this branch.
        let settings = client
            .rpc("settings.read", json!({"source":"workflow","scope":"user"}))
            .await;
        assert_ok(&settings);
        let mut source = settings["result"]["document"].clone();
        source["definitions"] = json!({
            "invalid": {"schema":"dim.workflow/v1","name":"invalid","description":"Broken reference",
                "steps":[{"stage":{"id":"only","prompt":"{{stages.missing.result}}"}}]},
            "guided": {"schema":"dim.workflow/v1","name":"guided","description":"One stage",
                "steps":[{"stage":{"id":"only","prompt":"Say hi"}}]},
        });
        assert_ok(
            &client
                .rpc(
                    "settings.update",
                    json!({"source":"workflow","scope":"user",
                        "expectedRevision":settings["result"]["revision"],"document":source}),
                )
                .await,
        );

        // The refusal the tool carries.
        let refused = tool_result(
            &mut client,
            &starter_session,
            json!({"action":"start","name":"invalid"}),
        )
        .await;
        assert_eq!(refused.is_error, Some(true), "{refused:?}");
        let dim_agent_types::ToolContent::Text(text) = refused.content else {
            panic!("the refusal is text")
        };
        if configured {
            assert_eq!(text, format!("workflow: invalid_definition. {RECOVERY}"));
        } else {
            assert_eq!(
                text,
                "workflow: invalid_definition. Use list to inspect the definition issues."
            );
        }

        // The report the tool's start sends to this branch.
        let started = tool_result(
            &mut client,
            &starter_session,
            json!({"action":"start","name":"guided"}),
        )
        .await;
        assert_ne!(started.is_error, Some(true), "{started:?}");
        let dim_agent_types::ToolContent::Text(content) = started.content else {
            panic!("Workflow returns text")
        };
        let run_id = content.split('\n').next().unwrap();
        let run_id: Value = serde_json::from_str(run_id).unwrap();
        let run_id = run_id["run_id"].as_str().unwrap().to_owned();
        let run = settled(&mut client, &json!(run_id)).await;
        assert_eq!(run["state"], "completed", "{run}");
        let text = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let history = starter_session.local("main", ..).await.unwrap();
                if let Some(text) = history.iter().find_map(|event| match &event.kind {
                    EventKind::InputMessage(data) => data.blocks.first().and_then(
                        |dim_agent_types::InputBlock::Raw { text, .. }| {
                            text.contains(&run_id).then(|| text.clone())
                        },
                    ),
                    _ => None,
                }) {
                    break text;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the report reaches the starter");
        if configured {
            assert!(
                text.ends_with(&format!(
                    "Ask workflow status with {run_id} later.</dim-workflow>"
                )),
                "{text}"
            );
        } else {
            assert!(
                text.ends_with(&format!(
                    "Use workflow status with run_id {run_id} for stored stage results.</dim-workflow>"
                )),
                "{text}"
            );
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}
