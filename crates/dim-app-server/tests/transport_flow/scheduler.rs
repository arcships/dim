//! Scheduler's optional package through actual Settings, C4, S2 and native history.
use super::*;
use dim_agent_capabilities::input_queue::InputQueue;
use dim_agent_runtime::SessionIdentity;
use rutis::host::CtxHost;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// The `dim-scheduler` entry (Scheduler §5.2), byte for byte.
const SCHEDULER_ENTRY: &str = r#"<dim-scheduler kind="scheduled-run" job="nightly-build" run="run-7f2">: a scheduled job's saved prompt, delivered when the job runs, and <dim-scheduler kind="report" job="nightly-build" run="run-7f2">: how that run ended. job names the job, run names the run, in both. In the scheduled-run tag the body is the job's name, and the message after the tag is the prompt saved for the job: the request this run carries out, in the operator's channel. In the report tag the body states the run's outcome and, when known, the session it ran in: the scheduler's account of how the run ended, information, not a request. The report's last line is not part of that account: it is guidance on what to do next."#;

struct AppliedInputGate {
    held: Arc<AtomicBool>,
    entered: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    release: Arc<tokio::sync::Notify>,

    dispatcher: rutis::host::Key<dim_agent_runtime::core::Dispatcher>,
    queue: rutis::host::Key<InputQueue>,
    identity: rutis::host::Key<SessionIdentity>,
}
impl AppliedInputGate {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        held: Arc<AtomicBool>,
        entered: tokio::sync::mpsc::UnboundedSender<(String, String)>,
        release: Arc<tokio::sync::Notify>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            held,
            entered,
            release,
            dispatcher: names.key()?,
            queue: names.key()?,
            identity: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for AppliedInputGate {
    fn name(&self) -> &str {
        "test.scheduler-applied-input"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.queue.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let release = self.release.clone();
            ctx.effect(move || {
                release.notify_waiters();
                async { Ok(()) }
            })?;
            let (held, entered, release) = (
                self.held.clone(),
                self.entered.clone(),
                self.release.clone(),
            );
            dispatcher.subscribe(ctx, "test.scheduler-applied-input", &["input/message"], move |event| {
                let (held, entered, release, session) = (held.clone(), entered.clone(), release.clone(), identity.session_id.clone());
                Box::pin(async move {
                    let scheduled = matches!(&event.kind, EventKind::InputMessage(data) if data.blocks.iter().any(|block|
                        matches!(block, dim_agent_types::InputBlock::Raw { text, .. } if text.starts_with("<dim-scheduler kind=\"scheduled-run\""))));
                    if scheduled && !held.swap(true, Ordering::SeqCst) {
                        entered.send((session, event.id.clone())).unwrap();
                        release.notified().await;
                    }
                    Ok(())
                })
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}
fn rows() -> dim_app_server::session_typescript::SessionTypeScriptConfig {
    let mut parts = session_typescript::fixture();
    parts.rows = vec![session_typescript::manifest_row("scheduler")];
    parts
}

fn probe_rows() -> dim_app_server::session_typescript::SessionTypeScriptConfig {
    let mut parts = rows();
    parts.rows[0]["name"] = json!(
        parts
            .anchor
            .parent()
            .unwrap()
            .join("fixtures/scheduler-package.ts")
    );
    parts
}

fn stored(home: &Path) -> Value {
    let outer: Value =
        serde_json::from_slice(&std::fs::read(home.join("scheduler/runs.json")).unwrap()).unwrap();
    assert_eq!(outer["version"], 1);
    serde_json::from_str(outer["text"].as_str().unwrap()).unwrap()
}

fn home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("dim-scheduler-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(path.join("scheduler")).unwrap();
    path.canonicalize().unwrap()
}

fn config(home: &Path) -> ServerConfig {
    let mut config = ServerConfig {
        product_home: Some(home.to_owned()),
        database_path: Some(home.join("native.sqlite")),
        session_typescript: Some(rows()),
        ..Default::default()
    };
    config.session_defaults.tools = Some(vec!["echo".into(), "schedule".into()]);
    config.session_defaults.permission_policy = dim_agent_types::PermissionPolicy {
        preset: dim_agent_types::PermissionPreset::FullAccess,
        ..Default::default()
    };
    config
}

async fn settled(client: &mut Client, job: &Value, run: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let reply = client
                .rpc("schedule.history", json!({"job_id":job,"limit":50}))
                .await;
            assert_ok(&reply);
            if let Some(found) = reply["result"]["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|value| value["run_id"] == *run)
                && !matches!(
                    found["state"].as_str(),
                    Some("launching" | "submitted" | "running")
                )
            {
                return found.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Scheduler settles its accepted run")
}

async fn window(client: &mut Client, job: &Value, at: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc("schedule.history", json!({"job_id":job})).await;
            assert_ok(&reply);
            if let Some(run) = reply["result"]["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| {
                    run["window"] == at
                        && !matches!(
                            run["state"].as_str(),
                            Some("launching" | "submitted" | "running")
                        )
                })
            {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a due native Scheduler window settles")
}

async fn tool(
    client: &mut Client,
    session: &dim_agent_runtime::Session,
    arguments: Value,
) -> ToolResult {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session.id(),"branchId":"main",
                    "text":format!("tool:schedule {arguments}")}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session.id(),"branchId":"main"}),
            )
            .await,
    );
    let events = session.local("main", ..).await.unwrap();
    events
        .iter()
        .rev()
        .find_map(|event| {
            if let EventKind::ToolResult(batch) = &event.kind {
                batch.results.first().map(|entry| entry.result.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("schedule tool result missing: {events:#?}"))
}

async fn reports(session: &dim_agent_runtime::Session, id: &str) -> usize {
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| {
            if let EventKind::InputMessage(data) = &event.kind
                && data.blocks.iter().any(|block| {
                    matches!(block, dim_agent_types::InputBlock::Raw { text, .. }
                        if text.starts_with("<dim-scheduler kind=\"report\"") && text.contains(id))
                })
            {
                assert!(
                    data.user.is_none(),
                    "one raw report, without duplicate user text"
                );
                assert_eq!(data.mode, Some(dim_agent_types::InputMode::Steer));
                true
            } else {
                false
            }
        })
        .count()
}

async fn input_count(client: &mut Client, session: &Value, submission: &str) -> usize {
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":"main","after":0,"limit":1000}),
        )
        .await;
    assert_ok(&history);
    assert_eq!(history["result"]["done"], true);
    history["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["submission_id"] == submission
        })
        .count()
}

async fn approval_on(client: &mut Client, session: &Value) -> Value {
    loop {
        let notice = client.notice("approval.request").await;
        if notice["params"]["sessionId"] == *session && notice["params"]["branchId"] == "main" {
            return notice;
        }
    }
}

async fn host(home: &Path) -> (tokio::process::Child, Client) {
    use std::process::Stdio;
    let mut process = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "scheduler::node_scheduler_complete_host_restart_reconciles_input_and_never_replays_terminal_reports", "--ignored", "--nocapture"])
        .env("DIM_SCHEDULER_PACKAGE_HOST", home)
        .env("DIMCODE_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap());
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
                return PathBuf::from(socket);
            }
        }
    })
    .await
    .unwrap();
    process.stdout = Some(output.into_inner());
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
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    (process, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_complete_host_restart_reconciles_input_and_never_replays_terminal_reports()
{
    if let Some(home) = std::env::var_os("DIM_SCHEDULER_PACKAGE_HOST") {
        let home = PathBuf::from(home);
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let server = launch_with_agent(Arc::new(MockTools), config).await.0;
        println!("\n{}", json!({"socket":server.unix_path}));
        server.join().await.unwrap();
        return;
    }
    for (phase, tool_saved) in [
        ("launch_reply", true),
        ("binding_before", true),
        ("binding_reply", true),
        ("input_before", true),
        ("input_reply", true),
        ("report_input", true),
        ("report_reply", true),
        ("launch_reply", false),
        ("binding_before", false),
        ("binding_reply", false),
        ("input_before", false),
    ] {
        let reuse = matches!(phase, "binding_before" | "binding_reply" | "input_before");
        let home = home();
        let (mut process, mut client) = host(&home).await;
        process_typescript::select_model(&mut client).await;
        let starter = client
            .rpc("session.create", json!({"cwd":home,"tools":["echo"]}))
            .await;
        assert_ok(&starter);
        let starter = &starter["result"]["sessionId"];
        let job = client
            .rpc(
                "schedule.save",
                json!({"job":{"name":phase,"cwd":home,"mode":if reuse {"reuse"} else {"create"},
            "prompt":"completed","schedule":{"at":"2030-01-01T00:00:00Z"}}}),
            )
            .await;
        assert_ok(&job);
        let job = &job["result"]["job"]["id"];
        if tool_saved {
            let settings = client
                .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                .await;
            assert_ok(&settings);
            let mut document = settings["result"]["document"].clone();
            document["jobs"][0]["created_by"] =
                json!({"tool":{"session_id":starter,"branch_id":"main"}});
            document["jobs"][0]["revision"] = json!(2);
            assert_ok(
                &client
                    .rpc(
                        "settings.update",
                        json!({"source":"schedule","scope":"user",
            "expectedRevision":settings["result"]["revision"],"document":document}),
                    )
                    .await,
            );
        }
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"gate":{"phase":phase,
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}}),
                )
                .await,
        );
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let id = &accepted["result"]["run_id"];
        let (_release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let before = stored(&home)["runs"][0].clone();
        if tool_saved {
            assert_eq!(before["report_to"]["session_id"], *starter);
        } else {
            assert!(before.get("report_to").is_none());
        }
        let listed = client.rpc("session.summaries", json!({})).await;
        assert_ok(&listed);
        let sessions = listed["result"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        let run_session = sessions
            .iter()
            .find(|session| session["sessionId"] != *starter)
            .unwrap()["sessionId"]
            .clone();
        if phase == "launch_reply" {
            assert!(before.get("session_id").is_none());
            assert!(before.get("policy").is_none());
        } else {
            assert_eq!(before["session_id"], run_session);
        }
        if matches!(
            phase,
            "launch_reply" | "binding_before" | "binding_reply" | "input_before" | "input_reply"
        ) {
            assert_eq!(before["state"], "launching");
            assert!(before.get("input_event_id").is_none());
        } else {
            assert_eq!(before["state"], "completed");
        }
        if phase == "input_reply" {
            assert_ok(
                &client
                    .rpc(
                        "session.idle",
                        json!({"sessionId":before["session_id"],"branchId":"main"}),
                    )
                    .await,
            );
        }
        let submission = format!("scheduler:{}", id.as_str().unwrap());
        let report_submission = format!("scheduler-report:{}", id.as_str().unwrap());
        let uncertain = matches!(
            phase,
            "launch_reply" | "binding_before" | "binding_reply" | "input_before"
        );
        if reuse {
            let settings = client
                .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                .await;
            assert_ok(&settings);
            let binding = settings["result"]["document"]["jobs"][0].get("session_id");
            assert_eq!(binding, (phase != "binding_before").then_some(&run_session));
        }
        let expected_inputs = usize::from(!uncertain);
        assert_eq!(
            input_count(&mut client, &run_session, &submission).await,
            expected_inputs
        );
        assert_eq!(
            input_count(&mut client, starter, &report_submission).await,
            usize::from(tool_saved && phase == "report_reply")
        );
        let old_pid = process.id().unwrap();
        process.kill().await.unwrap();
        process.wait().await.unwrap();
        drop(client);
        let (mut process, mut client) = host(&home).await;
        assert_ne!(process.id().unwrap(), old_pid);
        let recovered = settled(&mut client, job, id).await;
        assert_eq!(
            recovered["state"],
            if uncertain { "lost" } else { "completed" },
            "phase={phase}; tool_saved={tool_saved}; run={recovered}; native diagnostic={}",
            client.rpc("schedulerprobe.status", json!({})).await
        );
        assert_eq!(recovered["session_id"], before["session_id"]);
        assert_eq!(recovered["deadline_at"], before["deadline_at"]);
        if uncertain {
            assert_eq!(
                recovered["detail"],
                if phase == "launch_reply" {
                    "launch_identity_unknown"
                } else {
                    "input_unknown"
                }
            );
        }
        let listed = client.rpc("session.summaries", json!({})).await;
        assert_ok(&listed);
        let sessions = listed["result"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions
                .iter()
                .any(|session| session["sessionId"] == run_session)
        );
        assert!(
            sessions
                .iter()
                .any(|session| session["sessionId"] == *starter)
        );
        assert_eq!(
            input_count(&mut client, &run_session, &submission).await,
            expected_inputs
        );
        if tool_saved && !matches!(phase, "report_input" | "report_reply") {
            tokio::time::timeout(Duration::from_secs(10), async {
                while input_count(&mut client, starter, &report_submission).await == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(
            input_count(&mut client, starter, &report_submission).await,
            usize::from(tool_saved && phase != "report_input")
        );
        if !tool_saved {
            assert!(recovered.get("report_to").is_none());
            assert!(
                !recovered["detail"]
                    .as_str()
                    .unwrap()
                    .contains("report_failed")
            );
        }
        if reuse {
            let next = client.rpc("schedule.run_now", json!({"job_id":job})).await;
            assert_ok(&next);
            let next = settled(&mut client, job, &next["result"]["run_id"]).await;
            assert_eq!(next["state"], "completed");
            if phase == "binding_before" {
                assert_ne!(
                    next["session_id"], run_session,
                    "recovery does not infer a binding from the old receipt"
                );
            } else {
                assert_eq!(
                    next["session_id"], run_session,
                    "later runs use the binding actually committed in Settings"
                );
            }
        }
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
async fn node_scheduler_saves_live_authority_runs_noninteractive_and_reports_to_the_latest_saver() {
    let home = home();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config(&home)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let ready = process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    assert_eq!(ready["jobs"], json!([]));
    let settings = client
        .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
        .await;
    assert_ok(&settings);
    assert_eq!(
        settings["result"]["document"],
        json!({"jobs":[],"max_concurrent_runs":4})
    );
    assert_ok(&client.rpc("settings.update", json!({"source":"schedule","scope":"user",
        "expectedRevision":settings["result"]["revision"],"document":{"jobs":[],"max_concurrent_runs":2}})).await);
    let created = client.rpc("session.create", json!({"cwd":home,"tools":["schedule","echo"],
        "permissionPolicy":{"preset":"workspace-write","allowedTools":["schedule"],"disallowedTools":["blocked"]}})).await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let result = tool(
        &mut client,
        &starter,
        json!({"action":"create","job":{
        "name":"review","schedule":{"at":"2030-01-01T00:00:00Z"},"prompt":"Review the project"}}),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let dim_agent_types::ToolContent::Text(content) = &result.content else {
        panic!("schedule returns text")
    };
    let saved: Value = serde_json::from_str(content).unwrap();
    let job = &saved["job"]["id"];
    assert_eq!(saved["job"]["cwd"], json!(home));
    assert_eq!(
        saved["job"]["created_by"]["tool"]["session_id"],
        starter.id()
    );
    assert_eq!(
        saved["job"]["policy_cap"],
        json!({"preset":"workspace-write","allowedTools":["schedule"],"disallowedTools":["blocked"]})
    );
    let permission = client
        .rpc(
            "session.permission.get",
            json!({"sessionId":starter.id(),"branchId":"main"}),
        )
        .await;
    assert_ok(&permission);
    assert_ok(&client.rpc("session.permission.update", json!({"sessionId":starter.id(),"branchId":"main",
        "expectedVersion":permission["result"]["version"],"policy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}})).await);
    let started = client.rpc("schedule.run_now", json!({"job_id":job})).await;
    assert_ok(&started);
    let run = settled(&mut client, job, &started["result"]["run_id"]).await;
    assert_eq!(run["state"], "completed", "{run}");
    assert_eq!(run["session_origin"], "launch");
    assert_eq!(run["policy"], saved["job"]["policy_cap"]);
    let executing = agent
        .get_session(run["session_id"].as_str().unwrap())
        .unwrap();
    assert!(!executing.config().interactive);
    assert_eq!(
        executing.config().permission_policy.preset,
        dim_agent_types::PermissionPreset::WorkspaceWrite
    );
    let history = executing.local("main", ..).await.unwrap();
    assert!(
        history
            .iter()
            .any(|event| matches!(&event.kind, EventKind::InputMessage(data)
        if data.blocks.iter().any(|block| matches!(block, dim_agent_types::InputBlock::Raw{text,..}
            if text.starts_with("<dim-scheduler kind=\"scheduled-run\"")))))
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while reports(&starter, run["run_id"].as_str().unwrap()).await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(reports(&starter, run["run_id"].as_str().unwrap()).await, 1);
    let refused = tool(&mut client, &executing, json!({"action":"list"})).await;
    assert_eq!(refused.is_error, Some(true));
    let dim_agent_types::ToolContent::Text(content) = &refused.content else {
        panic!("schedule returns text")
    };
    assert!(content.contains("interactive leader"));

    let management = client
        .rpc("schedule.save", json!({"job":{"id":job,"name":"operator"}}))
        .await;
    assert_ok(&management);
    assert_eq!(management["result"]["job"]["created_by"], "user");
    assert_eq!(
        management["result"]["transition"]["to"]["effective_policy"]["preset"],
        "read-only"
    );
    let managed = client.rpc("schedule.run_now", json!({"job_id":job})).await;
    assert_ok(&managed);
    let managed = settled(&mut client, job, &managed["result"]["run_id"]).await;
    assert_eq!(managed["policy"]["preset"], "read-only");
    assert!(managed.get("report_to").is_none());
    assert_eq!(
        reports(&starter, managed["run_id"].as_str().unwrap()).await,
        0
    );
    let created_b = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["schedule","echo"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created_b);
    let branch_b = agent
        .get_session(created_b["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let updated = tool(
        &mut client,
        &branch_b,
        json!({"action":"update","job_id":job,"job":{"name":"branch B"}}),
    )
    .await;
    assert_ne!(updated.is_error, Some(true));
    let dim_agent_types::ToolContent::Text(content) = &updated.content else {
        panic!("schedule returns text")
    };
    let updated: Value = serde_json::from_str(content).unwrap();
    assert_eq!(
        updated["transition"]["from"]["effective_policy"]["preset"],
        "read-only"
    );
    assert_eq!(
        updated["transition"]["to"]["effective_policy"]["preset"],
        "full-access"
    );
    let current = client
        .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
        .await;
    assert_ok(&current);
    assert_eq!(current["result"]["document"]["max_concurrent_runs"], 2);
    let mut invalid = current["result"]["document"].clone();
    invalid["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("enabled_at");
    let refused = client
        .rpc(
            "settings.update",
            json!({"source":"schedule","scope":"user",
        "expectedRevision":current["result"]["revision"],"document":invalid}),
        )
        .await;
    assert!(
        refused.get("error").is_some(),
        "enabled jobs need their persisted anchor: {refused}"
    );
    let unchanged = client
        .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
        .await;
    assert_eq!(unchanged["result"], current["result"]);
    let outer: Value =
        serde_json::from_slice(&std::fs::read(home.join("scheduler/runs.json")).unwrap()).unwrap();
    assert_eq!(outer["version"], 1);
    let inner: Value = serde_json::from_str(outer["text"].as_str().unwrap()).unwrap();
    assert_eq!(inner["version"], 1);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Scheduler enabled while a Session is open: its Session part registers
/// the `dim-scheduler` entry mid-Session, on the adopted path, and the
/// request that consumes the report carries it (Prompt §6.3, model-facing
/// prompts §6.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_installed_mid_session_carries_its_entry() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    let home = std::env::temp_dir().join(format!(
        "ds-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let native = home.join("rust");
    std::fs::create_dir_all(&native).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: native.join("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let mut parts = session_typescript::fixture();
    parts.rows.clear();
    let mut config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(native.clone()),
        unix_path: Some(native.join("daemon.sock")),
        session_typescript: Some(parts),
        ..Default::default()
    };
    config.session_defaults.tools = Some(["echo", "schedule"].map(String::from).to_vec());
    config.session_defaults.permission_policy = dim_agent_types::PermissionPolicy {
        preset: dim_agent_types::PermissionPreset::FullAccess,
        ..Default::default()
    };
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    // The Session opens without the Scheduler and adopts a System without
    // its entry.
    let created = client.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":starter.id(),"branchId":"main","text":"start"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":starter.id(),"branchId":"main"}),
            )
            .await,
    );
    let system = super::reload::last_system(&starter, "main").await;
    assert!(!system.contains(SCHEDULER_ENTRY), "{system}");
    let package = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/plugins/scheduler")
        .canonicalize()
        .unwrap();
    let installed = client
        .rpc("plugin.install", json!({"source":{"path":package}}))
        .await;
    assert_ok(&installed);
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":starter.id(),"branchId":"main"}),
            )
            .await,
    );
    let saved = tool(
        &mut client,
        &starter,
        json!({"action":"create","job":{"name":"mid-session","cwd":home,
            "prompt":"Mid-session entry","schedule":{"at":"2040-01-01T00:00:00Z"}}}),
    )
    .await;
    assert_ne!(saved.is_error, Some(true), "{saved:?}");
    let job = saved.structured_content.unwrap()["job"]["id"].clone();
    let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
    assert_ok(&accepted);
    let run = settled(&mut client, &job, &accepted["result"]["run_id"]).await;
    assert_eq!(run["state"], "completed", "{run}");
    let submission = format!("scheduler-report:{}", run["run_id"].as_str().unwrap());
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = starter.local("main", ..).await.unwrap();
            let report = history.iter().find(|event| {
                matches!(&event.kind,
                    EventKind::InputMessage(data) if data.submission_id.as_deref() == Some(&submission))
            });
            if let Some(report) = report
                && history.iter().any(|event| {
                    matches!(&event.kind,
                        EventKind::InputApplied(data) if data.input_id == report.id)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the report is adopted in the Session opened before the enable");
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":starter.id(),"branchId":"main"}),
            )
            .await,
    );
    let system = super::reload::last_system(&starter, "main").await;
    assert!(system.contains(SCHEDULER_ENTRY), "{system}");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_job_edits_cas_and_local_source_writes_preserve_accepted_snapshots() {
    use tokio::io::AsyncWriteExt;
    for mode in [
        "before_launch",
        "before_input",
        "other_job",
        "same_revision",
        "cas_conflict",
    ] {
        let home = home();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"permissionPolicy":{
            "preset":"workspace-write","allowedTools":["schedule"],"disallowedTools":["blocked"]}}),
            )
            .await;
        assert_ok(&created);
        let starter = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let saved = tool(
            &mut client,
            &starter,
            json!({"action":"create","job":{"name":"original",
            "prompt":"original snapshot prompt","schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
        assert_ne!(saved.is_error, Some(true));
        let original = &saved.structured_content.as_ref().unwrap()["job"];
        let job = &original["id"];
        let other = if matches!(mode, "other_job" | "cas_conflict") {
            let reply = client
                .rpc(
                    "schedule.save",
                    json!({"job":{"name":"other","cwd":home,
                "prompt":"other job","schedule":{"at":"2030-01-01T00:00:00Z"}}}),
                )
                .await;
            assert_ok(&reply);
            Some(reply["result"]["job"]["id"].clone())
        } else {
            None
        };
        let next_saver = if mode == "same_revision" {
            let workspace = home.join("another-workspace");
            std::fs::create_dir_all(&workspace).unwrap();
            let reply = client.rpc("session.create", json!({"cwd":workspace})).await;
            assert_ok(&reply);
            Some(
                agent
                    .get_session(reply["result"]["sessionId"].as_str().unwrap())
                    .unwrap(),
            )
        } else {
            None
        };
        let sessions_before = client.rpc("session.summaries", json!({})).await;
        assert_ok(&sessions_before);
        let phase = match mode {
            "before_launch" => "launching_commit",
            "cas_conflict" => "policy_reply",
            _ => "launch_reply",
        };
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"gate":{"phase":phase,
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}}),
                )
                .await,
        );
        let pending = if mode == "cas_conflict" {
            client
                .send(
                    "schedule.save",
                    json!({"job":{"id":job,"name":"must not commit"}}),
                )
                .await
        } else {
            client.send("schedule.run_now", json!({"job_id":job})).await
        };
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        match mode {
            "other_job" | "cas_conflict" => {
                assert_ok(
                    &client
                        .rpc(
                            "schedule.save",
                            json!({"job":{"id":other.as_ref().unwrap(),"name":"changed other"}}),
                        )
                        .await,
                );
            }
            "same_revision" => {
                let settings = client
                    .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                    .await;
                assert_ok(&settings);
                let mut document = settings["result"]["document"].clone();
                let selected = document["jobs"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|item| item["id"] == *job)
                    .unwrap();
                selected["name"] = json!("configured next saver");
                selected["prompt"] = json!("new configured prompt");
                selected["created_by"] = json!({"tool":{"session_id":next_saver.as_ref().unwrap().id(),"branch_id":"main"}});
                selected["policy_cap"] =
                    json!({"preset":"full-access","allowedTools":[],"disallowedTools":[]});
                assert_eq!(selected["revision"], original["revision"]);
                assert_ok(
                    &client
                        .rpc(
                            "settings.update",
                            json!({"source":"schedule","scope":"user",
                    "expectedRevision":settings["result"]["revision"],"document":document}),
                        )
                        .await,
                );
            }
            _ => {
                assert_ok(
                    &client
                        .rpc(
                            "schedule.save",
                            json!({"job":{"id":job,"name":"changed selected"}}),
                        )
                        .await,
                );
            }
        }
        release.write_all(b"continue").await.unwrap();
        drop(release);
        let reply = client.response(pending).await;
        if mode == "cas_conflict" {
            assert_eq!(
                reply["error"]["data"]["appError"], "schedule_conflict",
                "{reply}"
            );
            let current = client
                .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                .await;
            assert_ok(&current);
            let selected = current["result"]["document"]["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == *job)
                .unwrap();
            assert_eq!(
                selected, original,
                "a stale authorizing save preserves the old origin/cap and job"
            );
        } else {
            assert_ok(&reply);
            let run = settled(&mut client, job, &reply["result"]["run_id"]).await;
            if matches!(mode, "before_launch" | "before_input") {
                assert_eq!(run["state"], "skipped");
                assert_eq!(run["detail"], "changed");
                assert_eq!(reports(&starter, run["run_id"].as_str().unwrap()).await, 0);
                assert!(run.get("input_event_id").is_none());
                if mode == "before_launch" {
                    assert!(run.get("session_id").is_none());
                    let sessions_after = client.rpc("session.summaries", json!({})).await;
                    assert_ok(&sessions_after);
                    assert_eq!(
                        sessions_after["result"].as_array().unwrap().len(),
                        sessions_before["result"].as_array().unwrap().len()
                    );
                } else {
                    assert_eq!(run["session_origin"], "launch");
                    let executing = agent
                        .get_session(run["session_id"].as_str().unwrap())
                        .unwrap();
                    assert!(
                        !executing
                            .local("main", ..)
                            .await
                            .unwrap()
                            .iter()
                            .any(|event| matches!(event.kind, EventKind::InputMessage(_)))
                    );
                }
            } else {
                assert_eq!(run["state"], "completed", "{run}");
                assert_eq!(run["policy"], original["policy_cap"]);
                assert_eq!(run["report_to"]["session_id"], starter.id());
                tokio::time::timeout(Duration::from_secs(10), async {
                    while reports(&starter, run["run_id"].as_str().unwrap()).await == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                let executing = agent
                    .get_session(run["session_id"].as_str().unwrap())
                    .unwrap();
                assert!(executing.local("main", ..).await.unwrap().iter().any(|event| matches!(&event.kind,
                    EventKind::InputMessage(data) if data.user.as_ref().is_some_and(|user| matches!(user.content.as_slice(),
                        [dim_agent_types::ContentBlock::Text { text }] if text == "original snapshot prompt")))));
                if let Some(next_saver) = next_saver {
                    let global = tool(
                        &mut client,
                        &next_saver,
                        json!({"action":"get","job_id":job}),
                    )
                    .await;
                    assert_ne!(global.is_error, Some(true));
                    assert_eq!(
                        global.structured_content.as_ref().unwrap()["job"]["created_by"]["tool"]["session_id"],
                        next_saver.id()
                    );
                    let next = client.rpc("schedule.run_now", json!({"job_id":job})).await;
                    assert_ok(&next);
                    let next = settled(&mut client, job, &next["result"]["run_id"]).await;
                    assert_eq!(next["state"], "completed");
                    assert_eq!(next["policy"]["preset"], "full-access");
                    assert_eq!(next["report_to"]["session_id"], next_saver.id());
                    tokio::time::timeout(Duration::from_secs(10), async {
                        while reports(&next_saver, next["run_id"].as_str().unwrap()).await == 0 {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    assert_eq!(reports(&starter, next["run_id"].as_str().unwrap()).await, 0);
                }
            }
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_shared_policy_vectors_reach_native_save_launch_and_reuse() {
    let vectors: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/permission-policy-cap.json"
    ))
    .unwrap();
    let unrestricted = json!({"preset":"full-access","allowedTools":[],"disallowedTools":[]});
    let mut bounds = vec![unrestricted.clone()];
    for bound in vectors["cap"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|vector| vector["usage"] == "job-save-and-launch")
        .map(|vector| &vector["bound"])
        .chain(
            vectors["reuse"]
                .as_array()
                .unwrap()
                .iter()
                .map(|vector| &vector["liveBound"]),
        )
    {
        if !bounds.contains(bound) {
            bounds.push(bound.clone());
        }
    }
    for bound in bounds {
        let home = home();
        let mut config = config(&home);
        config.session_defaults.permission_policy = serde_json::from_value(bound.clone()).unwrap();
        let (server, _) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let before = client
            .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
            .await;
        assert_ok(&before);
        let rejected = client
            .rpc(
                "schedule.save",
                json!({"job":{"name":"overflow","cwd":home,"prompt":"never admitted",
            "max_run_minutes":5_000_000_000_u64,"schedule":{"at":"2030-01-01T00:00:00Z"}}}),
            )
            .await;
        assert_eq!(
            rejected["error"]["data"]["appError"], "schedule_invalid",
            "{rejected}"
        );
        let after = client
            .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
            .await;
        assert_ok(&after);
        assert_eq!(after["result"], before["result"]);
        for vector in vectors["cap"].as_array().unwrap().iter().filter(|vector| {
            if vector["usage"] == "job-save-and-launch" {
                vector["bound"] == bound
            } else {
                bound == unrestricted
            }
        }) {
            let authorizing_save = vector["usage"] == "job-save-and-launch";
            let before = client
                .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                .await;
            assert_ok(&before);
            let mut job = json!({"name":vector["name"],"cwd":home,"prompt":"policy check",
                "schedule":{"at":"2030-01-01T00:00:00Z"}});
            if authorizing_save {
                job["policy"] = vector["requested"]["preset"].clone();
            }
            let saved = client.rpc("schedule.save", json!({"job":job})).await;
            if authorizing_save && vector["presetWithin"] == false {
                assert_eq!(
                    saved["error"]["data"]["appError"], "schedule_invalid",
                    "{saved}"
                );
                let after = client
                    .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                    .await;
                assert_ok(&after);
                assert_eq!(after["result"], before["result"]);
            } else {
                assert_ok(&saved);
                let job = &saved["result"]["job"]["id"];
                if authorizing_save {
                    assert_eq!(saved["result"]["job"]["policy_cap"], vector["bound"]);
                    assert_eq!(
                        saved["result"]["transition"]["to"]["effective_policy"],
                        vector["capped"]
                    );
                } else {
                    let starter = client
                        .rpc(
                            "session.create",
                            json!({"cwd":home,"tools":[],"permissionPolicy":vector["bound"]}),
                        )
                        .await;
                    assert_ok(&starter);
                    let settings = client
                        .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                        .await;
                    assert_ok(&settings);
                    let mut document = settings["result"]["document"].clone();
                    let selected = document["jobs"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|entry| entry["id"] == *job)
                        .unwrap();
                    selected["policy_cap"] = vector["requested"].clone();
                    selected["policy_cap"]["fileAccess"] = json!("read-only");
                    selected["policy_cap"]["toolApproval"] = json!("ask");
                    selected["created_by"] = json!({"tool":{"session_id":starter["result"]["sessionId"],"branch_id":"main"}});
                    selected["revision"] = json!(2);
                    assert_ok(
                        &client
                            .rpc(
                                "settings.update",
                                json!({"source":"schedule","scope":"user",
                        "expectedRevision":settings["result"]["revision"],"document":document}),
                            )
                            .await,
                    );
                }
                let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
                assert_ok(&accepted);
                let run = settled(&mut client, job, &accepted["result"]["run_id"]).await;
                assert_eq!(run["state"], "completed", "{vector}: {run}");
                assert_eq!(run["policy"], vector["capped"], "{vector}");
                let actual = client
                    .rpc(
                        "session.permission.get",
                        json!({"sessionId":run["session_id"],"branchId":"main"}),
                    )
                    .await;
                assert_ok(&actual);
                assert_eq!(actual["result"]["policy"], vector["capped"]);
            }
        }
        for vector in vectors["reuse"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|vector| vector["liveBound"] == bound)
        {
            let target = client
                .rpc(
                    "session.create",
                    json!({"cwd":home,"tools":[],"permissionPolicy":vector["x"]}),
                )
                .await;
            assert_ok(&target);
            let target = &target["result"]["sessionId"];
            let saved = client.rpc("schedule.save", json!({"job":{"name":vector["name"],"cwd":home,
                "prompt":"reuse policy check","schedule":{"at":"2030-01-01T00:00:00Z"},"mode":"reuse","session_id":target}})).await;
            assert_ok(&saved);
            let job = &saved["result"]["job"]["id"];
            let settings = client
                .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
                .await;
            assert_ok(&settings);
            let mut document = settings["result"]["document"].clone();
            let selected = document["jobs"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["id"] == *job)
                .unwrap();
            selected["policy_cap"] = vector["savedEffective"].clone();
            selected["policy"] = vector["savedEffective"]["preset"].clone();
            selected["revision"] = json!(2);
            assert_ok(
                &client
                    .rpc(
                        "settings.update",
                        json!({"source":"schedule","scope":"user",
                "expectedRevision":settings["result"]["revision"],"document":document}),
                    )
                    .await,
            );
            let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
            assert_ok(&accepted);
            let run = settled(&mut client, job, &accepted["result"]["run_id"]).await;
            let allowed = vector["withinSaved"] == true && vector["withinLive"] == true;
            assert_eq!(
                run["state"],
                if allowed { "completed" } else { "skipped" },
                "{vector}: {run}"
            );
            if allowed {
                assert_eq!(run["session_origin"], "reuse");
            } else {
                assert_eq!(run["detail"], "policy");
            }
            assert_eq!(
                input_count(&mut client, target, run["submission_id"].as_str().unwrap()).await,
                usize::from(allowed)
            );
            let actual = client
                .rpc(
                    "session.permission.get",
                    json!({"sessionId":target,"branchId":"main"}),
                )
                .await;
            assert_ok(&actual);
            assert_eq!(
                actual["result"]["policy"], vector["x"],
                "reuse must not change the target policy"
            );
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_not_held_rereads_native_deletion_completion_and_consumption() {
    use tokio::io::AsyncWriteExt;
    for outcome in ["deleted", "completed", "consumed"] {
        let home = home();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        config.session_defaults.tools = Some(vec!["write".into(), "schedule".into()]);
        let (server, agent) = launch_with_agent(Arc::new(NamedTools(calls.clone())), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let target = client
            .rpc(
                "session.create",
                json!({"cwd":home,"permissionPolicy":{"preset":"workspace-write"}}),
            )
            .await;
        assert_ok(&target);
        let target = &target["result"]["sessionId"];
        let executing = agent.get_session(target.as_str().unwrap()).unwrap();
        assert_ok(
            &client
                .rpc("session.attach", json!({"sessionId":target}))
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":target,"branchId":"main","text":"tool:write {}"}),
                )
                .await,
        );
        let first = client.notice("approval.request").await;
        let saved = client.rpc("schedule.save", json!({"job":{"name":outcome,"cwd":home,
            "prompt":if outcome == "consumed" { "tool:write {}" } else { "queued completion" },
            "schedule":{"at":"2030-01-01T00:00:00Z"},"max_run_minutes":1,"mode":"reuse","session_id":target,"policy":"workspace-write"}})).await;
        assert_ok(&saved);
        let job = &saved["result"]["job"]["id"];
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let submitted = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let history = client.rpc("schedule.history", json!({"job_id":job})).await;
                assert_ok(&history);
                let run = &history["result"]["runs"][0];
                if run["input_event_id"].is_string() {
                    return run.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let input = submitted["input_event_id"].as_str().unwrap();
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"now":submitted["deadline_at"],"gate":{
            "phase":"history_reply","host":"127.0.0.1","port":gate.local_addr().unwrap().port()}}),
                )
                .await,
        );
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        if outcome == "deleted" {
            let withdrawn = client
                .rpc(
                    "session.input.withdraw",
                    json!({"sessionId":target,"branchId":"main","inputId":input}),
                )
                .await;
            assert_ok(&withdrawn);
            assert_eq!(withdrawn["result"]["status"], "deleted");
        } else {
            assert_ok(
                &client
                    .rpc(
                        "approval.respond",
                        json!({"sessionId":target,"branchId":"main",
                "requestId":first["params"]["requestId"],"decision":"allow"}),
                    )
                    .await,
            );
            if outcome == "consumed" {
                client.notice("approval.request").await;
            } else {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let events = executing.local("main", ..).await.unwrap();
                        if let Some(index) = events.iter().position(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == input))
                            && events[index + 1..].iter().any(|event| matches!(event.kind, EventKind::TurnEnd(_))) { break; }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.unwrap();
            }
        }
        release.write_all(b"continue").await.unwrap();
        drop(release);
        let ended = settled(&mut client, job, &accepted["result"]["run_id"]).await;
        assert_eq!(
            ended["state"],
            match outcome {
                "deleted" => "cancelled",
                "completed" => "completed",
                _ => "timed_out",
            },
            "{ended}"
        );
        if outcome == "deleted" {
            assert_eq!(ended["detail"], "withdrawn");
        }
        let events = executing.local("main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
                .count(),
            usize::from(outcome == "consumed")
        );
        assert_eq!(events.iter().filter(|event| matches!(&event.kind, EventKind::InputWithdraw(data) if data.input_id == input)).count(), 1 + usize::from(outcome == "deleted"));
        assert_eq!(events.iter().filter(|event| matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == input)).count(), usize::from(outcome == "deleted"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(outcome != "deleted")
        );
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_applied_inputs_interrupted_before_consumption_expire_without_another_stop()
{
    for (reuse, stale_prefix) in [(false, false), (false, true), (true, false), (true, true)] {
        let home = home();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let (entered, mut events) = tokio::sync::mpsc::unbounded_channel();
        let held = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Notify::new());
        let for_plugin = release.clone();
        let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
            builder.require_feature(
                dim_agent_runtime::Feature::new("SchedulerAppliedInputGate").branch(
                    "test.scheduler-applied-input",
                    move |names| {
                        Ok(Box::new(AppliedInputGate::new(
                            names,
                            held.clone(),
                            entered.clone(),
                            for_plugin.clone(),
                        )?))
                    },
                ),
            );
        })
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let mut job = json!({"name":"retained input","cwd":home,"prompt":"retained scheduled input",
            "schedule":{"at":"2030-01-01T00:00:00Z"},"max_run_minutes":1});
        if reuse {
            let target = client
                .rpc(
                    "session.create",
                    json!({"cwd":home,"permissionPolicy":{"preset":"read-only"}}),
                )
                .await;
            assert_ok(&target);
            job["mode"] = json!("reuse");
            job["session_id"] = target["result"]["sessionId"].clone();
        }
        let saved = client.rpc("schedule.save", json!({"job":job})).await;
        assert_ok(&saved);
        let job = &saved["result"]["job"]["id"];
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let (session, input) = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "Scheduler input gate (reuse={reuse}, stale_prefix={stale_prefix}): accepted={accepted}, runs={}, plugins={:?}",
                    stored(&home),
                    server.diagnostics().plugins
                )
            })
            .unwrap();
        let executing = agent.get_session(&session).unwrap();
        let history_reply = if stale_prefix {
            let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            assert_ok(&client.rpc("schedulerprobe.configure", json!({"gate":{
                "phase":"history_reply","host":"127.0.0.1","port":gate.local_addr().unwrap().port()}})).await);
            Some(
                tokio::time::timeout(Duration::from_secs(10), gate.accept())
                    .await
                    .unwrap()
                    .unwrap()
                    .0,
            )
        } else {
            None
        };
        executing
            .interrupt("main", "user interrupt before consumption".into())
            .unwrap();
        release.notify_one();
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":session,"branchId":"main"}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"now":accepted["result"]["deadline_at"]}),
                )
                .await,
        );
        if let Some(mut reply) = history_reply {
            reply.write_all(b"prefix before interrupt").await.unwrap();
        }
        let ended = settled(&mut client, job, &accepted["result"]["run_id"]).await;
        assert_eq!(ended["state"], "timed_out", "{ended}");
        assert_eq!(ended["detail"], "interrupted_before_consumption");
        let before = executing.local("main", ..).await.unwrap();
        assert!(before.iter().any(
            |event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == input)
        ));
        assert!(!before.iter().any(
            |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == input)
        ));
        let unexpected: Vec<_> = before
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::InputDeleted(_) => Some(("input deleted", event.sequence)),
                EventKind::ModelCall(_) => Some(("model called", event.sequence)),
                _ => None,
            })
            .collect();
        assert!(
            unexpected.is_empty(),
            "applied input changed: {unexpected:?}"
        );
        assert_eq!(
            before
                .iter()
                .filter(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
                .count(),
            1
        );
        if reuse {
            assert_ok(
                &client
                    .rpc(
                        "session.submit",
                        json!({"sessionId":session,"branchId":"main","text":"later driving input"}),
                    )
                    .await,
            );
            assert_ok(
                &client
                    .rpc(
                        "session.idle",
                        json!({"sessionId":session,"branchId":"main"}),
                    )
                    .await,
            );
            let after = executing.local("main", ..).await.unwrap();
            assert!(after.iter().any(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == input)));
            let current = settled(&mut client, job, &ended["run_id"]).await;
            assert_eq!(
                current, ended,
                "later consumption cannot change a terminal run"
            );
        } else {
            let next = client.rpc("schedule.run_now", json!({"job_id":job})).await;
            assert_ok(&next);
            assert_eq!(
                settled(&mut client, job, &next["result"]["run_id"]).await["state"],
                "completed"
            );
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_overlap_capacity_and_busy_reuse_follow_native_input_boundaries() {
    let home = home();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = config(&home);
    config.session_typescript = Some(probe_rows());
    config.session_defaults.tools = Some(vec!["write".into(), "schedule".into()]);
    let (server, _) = launch_with_agent(Arc::new(NamedTools(calls.clone())), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-10-22T00:00:00Z"}),
            )
            .await,
    );
    let settings = client
        .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
        .await;
    assert_ok(&settings);
    assert_ok(&client.rpc("settings.update", json!({"source":"schedule","scope":"user",
        "expectedRevision":settings["result"]["revision"],"document":{"jobs":[],"max_concurrent_runs":1}})).await);
    let saved = client.rpc("schedule.save", json!({"job":{"name":"overlap","cwd":home,
        "prompt":"tool:write {}","policy":"workspace-write","max_run_minutes":3,"schedule":{"every_minutes":1}}})).await;
    assert_ok(&saved);
    let job = &saved["result"]["job"]["id"];
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = client.rpc("schedule.history", json!({"job_id":job})).await;
            assert_ok(&history);
            if history["result"]["runs"][0]["input_event_id"].is_string() {
                return history["result"]["runs"][0].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":first["session_id"]}))
            .await,
    );
    approval_on(&mut client, &first["session_id"]).await;
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-10-22T00:01:00Z"}),
            )
            .await,
    );
    let skipped = window(&mut client, job, "2026-10-22T00:01:00.000Z").await;
    assert_eq!(skipped["state"], "skipped");
    assert_eq!(skipped["detail"], "overlap");
    let other = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"capacity","cwd":home,
        "prompt":"later","schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
    assert_ok(&other);
    let other = &other["result"]["job"]["id"];
    let capacity = client
        .rpc("schedule.run_now", json!({"job_id":other}))
        .await;
    assert_ok(&capacity);
    assert_eq!(capacity["result"]["state"], "skipped");
    assert_eq!(capacity["result"]["detail"], "capacity");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_ok(
        &client
            .rpc(
                "session.stop",
                json!({"sessionId":first["session_id"],"branchId":"main"}),
            )
            .await,
    );
    assert_eq!(
        settled(&mut client, job, &first["run_id"]).await["state"],
        "cancelled"
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-10-22T00:02:00Z"}),
            )
            .await,
    );
    let next = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = client.rpc("schedule.history", json!({"job_id":job})).await;
            assert_ok(&history);
            if let Some(run) = history["result"]["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| {
                    run["window"] == "2026-10-22T00:02:00.000Z" && run["input_event_id"].is_string()
                })
            {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":next["session_id"]}))
            .await,
    );
    let approval = approval_on(&mut client, &next["session_id"]).await;
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":next["session_id"],"branchId":"main",
        "requestId":approval["params"]["requestId"],"decision":"allow"}),
            )
            .await,
    );
    assert_eq!(
        settled(&mut client, job, &next["run_id"]).await["state"],
        "completed"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let allowed = client
        .rpc("schedule.run_now", json!({"job_id":other}))
        .await;
    assert_ok(&allowed);
    assert_eq!(
        settled(&mut client, other, &allowed["result"]["run_id"]).await["state"],
        "completed"
    );

    let target = client
        .rpc(
            "session.create",
            json!({"cwd":home,"permissionPolicy":{"preset":"workspace-write"}}),
        )
        .await;
    assert_ok(&target);
    let target = &target["result"]["sessionId"];
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":target}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":target,"branchId":"main","text":"tool:write {}"}),
            )
            .await,
    );
    let first_approval = approval_on(&mut client, target).await;
    let queued = client.rpc("schedule.save", json!({"job":{"name":"queued","cwd":home,"prompt":"tool:write {}",
        "policy":"workspace-write","schedule":{"at":"2030-01-01T00:00:00Z"},"mode":"reuse","session_id":target}})).await;
    assert_ok(&queued);
    let queued = &queued["result"]["job"]["id"];
    let accepted = client
        .rpc("schedule.run_now", json!({"job_id":queued}))
        .await;
    assert_ok(&accepted);
    let waiting = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = client
                .rpc("schedule.history", json!({"job_id":queued}))
                .await;
            assert_ok(&history);
            let run = &history["result"]["runs"][0];
            if run["input_event_id"].is_string() {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(waiting["state"], "submitted");
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":target,"branchId":"main","after":0,"limit":1000}),
        )
        .await;
    assert_ok(&history);
    assert!(
        !history["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["type"] == "input"
                    && event["name"] == "consumed"
                    && event["data"]["input_id"] == waiting["input_event_id"]
            })
    );
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":target,"branchId":"main",
        "requestId":first_approval["params"]["requestId"],"decision":"allow"}),
            )
            .await,
    );
    let second_approval = approval_on(&mut client, target).await;
    assert_ne!(
        second_approval["params"]["requestId"],
        first_approval["params"]["requestId"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let active = client
        .rpc("schedule.history", json!({"job_id":queued}))
        .await;
    assert_ok(&active);
    assert!(
        matches!(
            active["result"]["runs"][0]["state"].as_str(),
            Some("submitted" | "running")
        ),
        "the earlier turn end is not this run's outcome: {active}"
    );
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":target,"branchId":"main",
        "requestId":second_approval["params"]["requestId"],"decision":"allow"}),
            )
            .await,
    );
    let done = settled(&mut client, queued, &accepted["result"]["run_id"]).await;
    assert_eq!(done["state"], "completed");
    assert_eq!(done["session_id"], *target);
    assert_eq!(done["session_origin"], "reuse");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_late_native_admission_and_policy_replies_preserve_identity_without_input() {
    use tokio::io::AsyncWriteExt;
    for phase in ["launch_reply", "reuse_reply", "policy_reply"] {
        let home = home();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let mut job = json!({"name":phase,"cwd":home,"prompt":"must not be submitted",
            "schedule":{"at":"2030-01-01T00:00:00Z"},"max_run_minutes":1});
        if phase != "launch_reply" {
            let target = client
                .rpc(
                    "session.create",
                    json!({"cwd":home,"permissionPolicy":{"preset":"read-only"}}),
                )
                .await;
            assert_ok(&target);
            job["mode"] = json!("reuse");
            job["session_id"] = target["result"]["sessionId"].clone();
        }
        let job = client.rpc("schedule.save", json!({"job":job})).await;
        assert_ok(&job);
        let job = &job["result"]["job"]["id"];
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"gate":{"phase":phase,
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}}),
                )
                .await,
        );
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"now":accepted["result"]["deadline_at"]}),
                )
                .await,
        );
        release.write_all(b"continue").await.unwrap();
        drop(release);
        let run = settled(&mut client, job, &accepted["result"]["run_id"]).await;
        assert_eq!(run["state"], "timed_out", "{run}");
        assert_eq!(run["detail"], "deadline_before_input");
        assert_eq!(
            run["session_origin"],
            if phase == "launch_reply" {
                "launch"
            } else {
                "reuse"
            }
        );
        assert_eq!(run["policy"]["preset"], "read-only");
        assert_eq!(run["deadline_at"], accepted["result"]["deadline_at"]);
        assert!(run.get("input_event_id").is_none());
        let session = agent
            .get_session(run["session_id"].as_str().unwrap())
            .unwrap();
        let events = session.local("main", ..).await.unwrap();
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            EventKind::InputMessage(_) | EventKind::InputWithdraw(_) | EventKind::StopInterrupt(_)
        )));
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_reports_only_selected_tool_outcomes_with_model_visible_identity() {
    for tool_saved in [false, true] {
        let home = home();
        let calls = Arc::new(AtomicUsize::new(0));
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        config.session_defaults.tools = Some(vec!["write".into(), "schedule".into()]);
        let (server, agent) = launch_with(Arc::new(NamedTools(calls.clone())), config, |builder| {
            builder.require_feature(session_typescript::ControlledGate::feature(send.clone()));
        })
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"now":"2030-01-01T00:00:00Z"}),
                )
                .await,
        );
        let created = client.rpc("session.create", json!({"cwd":home})).await;
        assert_ok(&created);
        let starter = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let (_, _, loader, _) = controls.recv().await.unwrap();
        let mut finished = Vec::new();
        let mut expected_attempts = 0;
        for outcome in [
            "completed",
            "failed",
            "cancelled",
            "timed_out",
            "skipped",
            "missed",
        ] {
            let mut job = json!({"name":outcome,"cwd":home,"prompt":"completed",
                "schedule":{"at":"2040-01-01T00:00:00Z"},"max_run_minutes":1});
            if outcome == "failed" {
                job["cwd"] = json!(home.join("missing-workspace"));
            } else if matches!(outcome, "cancelled" | "timed_out") {
                job["policy"] = json!("workspace-write");
                job["prompt"] = json!("tool:write {}");
            } else if outcome == "missed" {
                job["schedule"] = json!({"at":"2030-01-01T01:00:00Z"});
            }
            let saved = if tool_saved {
                let result =
                    tool(&mut client, &starter, json!({"action":"create","job":job})).await;
                assert_ne!(result.is_error, Some(true), "{result:?}");
                result.structured_content.unwrap()["job"].clone()
            } else {
                let result = client.rpc("schedule.save", json!({"job":job})).await;
                assert_ok(&result);
                result["result"]["job"].clone()
            };
            let job = &saved["id"];
            if outcome == "skipped" {
                let disabled = client
                    .rpc(
                        "schedule.set_enabled",
                        json!({"job_id":job,"enabled":false}),
                    )
                    .await;
                assert_ok(&disabled);
                assert_eq!(disabled["result"]["job"]["created_by"], saved["created_by"]);
                assert_eq!(disabled["result"]["job"]["policy_cap"], saved["policy_cap"]);
            }
            assert_eq!(
                saved["created_by"],
                if tool_saved {
                    json!({"tool":{"session_id":starter.id(),"branch_id":"main"}})
                } else {
                    json!("user")
                }
            );
            let id = if outcome == "missed" {
                assert_ok(
                    &client
                        .rpc(
                            "schedulerprobe.configure",
                            json!({"now":"2030-01-01T01:02:00Z"}),
                        )
                        .await,
                );
                window(&mut client, job, "2030-01-01T01:00:00.000Z").await["run_id"].clone()
            } else {
                let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
                assert_ok(&accepted);
                accepted["result"]["run_id"].clone()
            };
            if matches!(outcome, "cancelled" | "timed_out") {
                let submitted = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let history = client.rpc("schedule.history", json!({"job_id":job})).await;
                        assert_ok(&history);
                        let run = &history["result"]["runs"][0];
                        if run["input_event_id"].is_string() {
                            break run.clone();
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                let session = &submitted["session_id"];
                assert_ok(
                    &client
                        .rpc("session.attach", json!({"sessionId":session}))
                        .await,
                );
                approval_on(&mut client, session).await;
                if outcome == "cancelled" {
                    assert_ok(
                        &client
                            .rpc(
                                "session.stop",
                                json!({"sessionId":session,"branchId":"main"}),
                            )
                            .await,
                    );
                } else {
                    assert_ok(
                        &client
                            .rpc(
                                "schedulerprobe.configure",
                                json!({"now":submitted["deadline_at"]}),
                            )
                            .await,
                    );
                }
            }
            let run = settled(&mut client, job, &id).await;
            assert_eq!(run["state"], outcome, "tool_saved={tool_saved}; run={run}");
            assert!(!run["detail"].as_str().unwrap().contains("report_failed"));
            let expected_reports =
                usize::from(tool_saved && !matches!(outcome, "skipped" | "missed"));
            if expected_reports == 1 {
                assert_eq!(
                    run["report_to"],
                    json!({"session_id":starter.id(),"branch_id":"main"})
                );
                tokio::time::timeout(Duration::from_secs(10), async {
                    while reports(&starter, id.as_str().unwrap()).await == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                let history = starter.local("main", ..).await.unwrap();
                let submission = format!("scheduler-report:{}", id.as_str().unwrap());
                let text = history
                    .iter()
                    .find_map(|event| {
                        if let EventKind::InputMessage(data) = &event.kind
                            && data.submission_id.as_deref() == Some(&submission)
                        {
                            data.blocks
                                .first()
                                .map(|dim_agent_types::InputBlock::Raw { text, .. }| text)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                assert!(
                    text.starts_with(&format!(
                        "<dim-scheduler kind=\"report\" job=\"{}\" run=\"{}\">",
                        job.as_str().unwrap(),
                        id.as_str().unwrap()
                    )),
                    "{text}"
                );
                assert!(
                    text.ends_with("\nInspect schedule.history for this job.</dim-scheduler>"),
                    "{text}"
                );
                assert!(text.contains(job.as_str().unwrap()));
                assert!(text.contains(id.as_str().unwrap()));
                assert!(text.contains(outcome));
                if let Some(session) = run["session_id"].as_str() {
                    assert!(text.contains(session));
                }
            } else {
                assert!(run.get("report_to").is_none());
            }
            assert_eq!(
                reports(&starter, id.as_str().unwrap()).await,
                expected_reports
            );
            expected_attempts += expected_reports;
            let status = client.rpc("schedulerprobe.status", json!({})).await;
            assert_ok(&status);
            assert_eq!(status["result"]["reportAttempts"], expected_attempts);
            finished.push((run, expected_reports));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        loader.restart("scheduler").await.unwrap();
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        for (run, expected_reports) in finished {
            assert_eq!(
                settled(&mut client, &run["job_id"], &run["run_id"]).await,
                run
            );
            assert_eq!(
                reports(&starter, run["run_id"].as_str().unwrap()).await,
                expected_reports
            );
        }
        let status = client.rpc("schedulerprobe.status", json!({})).await;
        assert_ok(&status);
        assert_eq!(status["result"]["reportAttempts"], 0);
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_failed_reports_keep_the_outcome_and_never_retry_or_redirect() {
    for (failure, code) in [
        ("closed", "session_closed"),
        ("deleted", "session_closed"),
        ("reply_lost", "bridge_error"),
    ] {
        let home = home();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
            builder.require_feature(session_typescript::ControlledGate::feature(send.clone()));
        })
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let created = client.rpc("session.create", json!({"cwd":home})).await;
        assert_ok(&created);
        let target = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let (_, _, loader, _) = controls.recv().await.unwrap();
        let unrelated = client.rpc("session.create", json!({"cwd":home})).await;
        assert_ok(&unrelated);
        let unrelated = agent
            .get_session(unrelated["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let saved = tool(
            &mut client,
            &target,
            json!({"action":"create","job":{"name":failure,"prompt":"completed",
                "schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
        assert_ne!(saved.is_error, Some(true), "{saved:?}");
        let job = &saved.structured_content.as_ref().unwrap()["job"]["id"];
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"gate":{"phase":"report_input","host":"127.0.0.1",
                        "port":gate.local_addr().unwrap().port()},"loseReportReply":failure=="reply_lost"}),
                )
                .await,
        );
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let id = &accepted["result"]["run_id"];
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let before = stored(&home)["runs"][0].clone();
        assert_eq!(before["state"], "completed");
        assert_eq!(
            before["report_to"],
            json!({"session_id":target.id(),"branch_id":"main"})
        );
        assert_eq!(reports(&target, id.as_str().unwrap()).await, 0);
        if failure != "reply_lost" {
            assert_ok(
                &client
                    .rpc(
                        if failure == "closed" {
                            "session.close"
                        } else {
                            "session.delete"
                        },
                        json!({"sessionId":target.id()}),
                    )
                    .await,
            );
        }
        release.write_all(b"report").await.unwrap();
        drop(release);
        let mut expected = before.clone();
        expected["detail"] = json!(format!(
            "{}; report_failed: {code}",
            before["detail"].as_str().unwrap()
        ));
        let failed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let run = settled(&mut client, job, id).await;
                if run["detail"] == expected["detail"] {
                    break run;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("report failure is recorded without changing the outcome");
        assert_eq!(failed, expected);
        let status = client.rpc("schedulerprobe.status", json!({})).await;
        assert_ok(&status);
        assert_eq!(status["result"]["reportAttempts"], 1);
        assert_eq!(reports(&unrelated, id.as_str().unwrap()).await, 0);
        if failure == "reply_lost" {
            assert_eq!(reports(&target, id.as_str().unwrap()).await, 1);
            let input = input_count(
                &mut client,
                &json!(target.id()),
                &format!("scheduler-report:{}", id.as_str().unwrap()),
            )
            .await;
            assert_eq!(input, 1);
        }
        loader.restart("scheduler").await.unwrap();
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        assert_eq!(settled(&mut client, job, id).await, expected);
        let status = client.rpc("schedulerprobe.status", json!({})).await;
        assert_ok(&status);
        assert_eq!(status["result"]["reportAttempts"], 0);
        assert_eq!(reports(&unrelated, id.as_str().unwrap()).await, 0);
        if failure == "reply_lost" {
            assert_eq!(reports(&target, id.as_str().unwrap()).await, 1);
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_report_lands_once_across_row_or_runtime_cleanup() {
    use tokio::io::AsyncWriteExt;
    for kill_runtime in [false, true] {
        let home = home();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
            builder.require_feature(session_typescript::ControlledGate::feature(send.clone()));
        })
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let created = client.rpc("session.create", json!({"cwd":home})).await;
        assert_ok(&created);
        let target = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let (_, _, loader, _) = controls.recv().await.unwrap();
        let saved = tool(
            &mut client,
            &target,
            json!({"action":"create","job":{"name":"drain",
            "prompt":"completed","schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
        assert_ne!(saved.is_error, Some(true));
        let job = &saved.structured_content.as_ref().unwrap()["job"]["id"];
        let before = client.rpc("schedulerprobe.status", json!({})).await;
        assert_ok(&before);
        let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"gate":{"phase":"report_input",
            "host":"127.0.0.1","port":gate.local_addr().unwrap().port()}}),
                )
                .await,
        );
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let id = accepted["result"]["run_id"].as_str().unwrap();
        let (mut release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
            .await
            .unwrap()
            .unwrap();
        let run = stored(&home)["runs"][0].clone();
        assert_eq!(run["state"], "completed");
        assert_ok(&client.rpc("schedule.delete", json!({"job_id":job})).await);
        assert_eq!(stored(&home)["runs"][0]["run_id"], id);
        let connection = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        release.write_all(b"continue").await.unwrap();
        drop(release);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !target.publication("main").unwrap().has_pending() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if kill_runtime {
            assert!(
                std::process::Command::new("kill")
                    .args([
                        "-KILL",
                        &before["result"]["pid"].as_u64().unwrap().to_string()
                    ])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let (restarted, ()) = tokio::join!(
            loader.restart(if kill_runtime {
                "session-node"
            } else {
                "scheduler"
            }),
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                connection.execute_batch("COMMIT").unwrap();
            }
        );
        restarted.unwrap();
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while target.publication("main").unwrap().has_pending() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(reports(&target, id).await, 1);
        tokio::time::timeout(Duration::from_secs(10), async {
            while !stored(&home)["runs"].as_array().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let summaries = client.rpc("session.summaries", json!({})).await;
        assert_ok(&summaries);
        assert!(
            !summaries["result"]
                .as_array()
                .unwrap()
                .iter()
                .any(|session| session["sessionId"] == run["session_id"])
        );
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_missing_reuse_target_fails_but_unreadable_authority_skips() {
    for removed in ["starter", "target"] {
        let home = home();
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config(&home)).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let created = client.rpc("session.create", json!({"cwd":home})).await;
        assert_ok(&created);
        let starter = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let saved = tool(
            &mut client,
            &starter,
            json!({"action":"create","job":{
            "name":"live authority","cwd":home,"mode":"reuse","prompt":"first native input",
            "schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
        assert_ne!(saved.is_error, Some(true), "{saved:?}");
        let job = saved.structured_content.unwrap()["job"]["id"].clone();
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let first = settled(&mut client, &job, &accepted["result"]["run_id"]).await;
        assert_eq!(first["state"], "completed");
        starter.idle("main").await.unwrap();
        let target = first["session_id"].clone();
        let before = client
            .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
            .await;
        assert_ok(&before);
        assert_eq!(
            before["result"]["document"]["jobs"][0]["session_id"],
            target
        );
        let deleted = if removed == "starter" {
            json!(starter.id())
        } else {
            target.clone()
        };
        let survivor = if removed == "starter" {
            target
        } else {
            json!(starter.id())
        };
        assert_ok(
            &client
                .rpc("session.delete", json!({"sessionId":deleted}))
                .await,
        );
        let policy = client
            .rpc(
                "session.permission.get",
                json!({"sessionId":survivor,"branchId":"main"}),
            )
            .await;
        assert_ok(&policy);
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let failed = settled(&mut client, &job, &accepted["result"]["run_id"]).await;
        let (state, detail) = if removed == "target" {
            ("failed", "session_closed")
        } else {
            ("skipped", "policy_read_failed")
        };
        assert_eq!(failed["state"], state, "{removed}: {failed}");
        assert_eq!(failed["detail"], detail);
        if removed == "target" {
            tokio::time::timeout(Duration::from_secs(10), async {
                while reports(&starter, failed["run_id"].as_str().unwrap()).await == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                reports(&starter, failed["run_id"].as_str().unwrap()).await,
                1
            );
        }
        for field in ["session_id", "session_origin", "policy", "input_event_id"] {
            assert!(
                failed.get(field).is_none(),
                "failed authority reads do not invent {field}"
            );
        }
        assert_eq!(
            input_count(
                &mut client,
                &survivor,
                failed["submission_id"].as_str().unwrap()
            )
            .await,
            0
        );
        let after = client
            .rpc("settings.read", json!({"source":"schedule","scope":"user"}))
            .await;
        assert_ok(&after);
        assert_eq!(
            after["result"], before["result"],
            "failed reads preserve the saved job, origin and full cap"
        );
        let current = client
            .rpc(
                "session.permission.get",
                json!({"sessionId":survivor,"branchId":"main"}),
            )
            .await;
        assert_ok(&current);
        assert_eq!(current["result"]["policy"], policy["result"]["policy"]);
        let summaries = client.rpc("session.summaries", json!({})).await;
        assert_ok(&summaries);
        assert_eq!(
            summaries["result"].as_array().unwrap().len(),
            1,
            "unavailable policy does not launch a replacement"
        );
        assert_eq!(summaries["result"][0]["sessionId"], survivor);
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_retention_preserves_shared_receipts_forks_and_unreadable_sources() {
    use std::os::unix::fs::PermissionsExt;
    let home = home();
    let mut config = config(&home);
    config.session_typescript = Some(probe_rows());
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send.clone()));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let external = client
        .rpc("session.create", json!({"cwd":home,"tools":[]}))
        .await;
    assert_ok(&external);
    let external = external["result"]["sessionId"].clone();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    let job = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"history","cwd":home,
            "prompt":"retained conversation","schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
    assert_ok(&job);
    let job = job["result"]["job"]["id"].clone();
    let mut completed = Vec::new();
    for _ in 0..2 {
        let started = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&started);
        completed.push(settled(&mut client, &job, &started["result"]["run_id"]).await);
    }
    let shared = completed[0]["session_id"].clone();
    let expired = completed[1]["session_id"].clone();
    let original = agent.get_session(expired.as_str().unwrap()).unwrap();
    original.idle("main").await.unwrap();
    let cutoff = original
        .local("main", ..)
        .await
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    let fork = client
        .rpc("session.fork", json!({"sessionId":expired,"at":cutoff}))
        .await;
    assert_ok(&fork);
    let fork = fork["result"]["sessionId"].clone();
    let reference = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"reference","cwd":home,"prompt":"reuse",
            "schedule":{"at":"2030-01-01T00:00:00Z"},"mode":"reuse","session_id":shared}}),
        )
        .await;
    assert_ok(&reference);
    let reference = reference["result"]["job"]["id"].clone();
    let external_job = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"external","cwd":home,"prompt":"external history",
            "schedule":{"at":"2030-01-01T00:00:00Z"},"mode":"reuse",
            "session_id":external,"policy":"full-access"}}),
        )
        .await;
    assert_ok(&external_job);
    let external_job = external_job["result"]["job"]["id"].clone();
    let started = client
        .rpc("schedule.run_now", json!({"job_id":external_job}))
        .await;
    assert_ok(&started);
    let reused = settled(&mut client, &external_job, &started["result"]["run_id"]).await;
    assert_eq!(reused["state"], "completed");
    assert_eq!(reused["session_origin"], "reuse");

    session_typescript::fixture_layer(
        &loader,
        "retention-fixture",
        Some(vec![
            serde_json::from_value(json!({"id":"scheduler","disabled":true})).unwrap(),
        ]),
    )
    .await;
    assert!(loader.get("scheduler").unwrap().view.is_none());
    let mut runs: Vec<_> = (0..102)
        .map(|index| {
            let mut run = completed[usize::from(index == 1)].clone();
            run["run_id"] = json!(format!("history-{index:03}"));
            run["submission_id"] = json!(format!("scheduler:history-{index:03}"));
            run["created_at"] = json!("2026-01-01T00:00:00.000Z");
            run["ended_at"] = json!(format!("2026-01-01T00:00:00.{index:03}Z"));
            if index >= 2 {
                run["session_origin"] = json!("reuse");
            }
            run
        })
        .collect();
    let path = home.join("scheduler/runs.json");
    let persist = |runs: &[Value]| {
        std::fs::write(
            &path,
            serde_json::to_vec(
                &json!({"version":1,"text":json!({"version":1,"runs":runs}).to_string()}),
            )
            .unwrap(),
        )
        .unwrap()
    };
    persist(&runs);
    session_typescript::fixture_layer(&loader, "retention-fixture", None).await;
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(&home)["runs"].as_array().unwrap().len() != 101 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background retention settles before its result is checked");
    let retained = stored(&home);
    assert_eq!(retained["runs"].as_array().unwrap().len(), 101);
    assert!(
        retained["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run_id"] == "history-000")
    );
    assert!(
        !retained["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run_id"] == "history-001")
    );
    assert!(agent.get_session(shared.as_str().unwrap()).is_some());
    let sessions = client.rpc("session.summaries", json!({})).await;
    assert_ok(&sessions);
    assert!(
        !sessions["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["sessionId"] == expired)
    );

    session_typescript::fixture_layer(
        &loader,
        "retention-fixture",
        Some(vec![
            serde_json::from_value(json!({"id":"scheduler","disabled":true})).unwrap(),
        ]),
    )
    .await;
    runs.remove(1);
    for run in runs.iter_mut().skip(1) {
        run["session_id"] = external.clone();
        run["session_origin"] = json!("reuse");
        run["policy"] = reused["policy"].clone();
        run["input_event_id"] = reused["input_event_id"].clone();
    }
    persist(&runs);
    session_typescript::fixture_layer(&loader, "retention-fixture", None).await;
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    assert_eq!(
        stored(&home)["runs"],
        json!(runs),
        "a reuse job keeps the final launch receipt after its run references expire"
    );

    let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    database.busy_timeout(Duration::from_secs(5)).unwrap();
    let document: String = database
        .query_row(
            "SELECT document FROM settings_documents WHERE source='user/schedule'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        database
            .execute(
                "UPDATE settings_documents SET document='{' WHERE source='user/schedule'",
                [],
            )
            .unwrap(),
        1
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2027-01-01T00:00:00Z"}),
            )
            .await,
    );
    let observed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let probe = client.rpc("schedulerprobe.status", json!({})).await;
            assert_ok(&probe);
            if !probe["result"]["settingsReadError"].is_null() {
                break probe;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let unavailable = client.rpc("schedule.list", json!({})).await;
    assert_eq!(
        unavailable["error"]["data"]["appError"],
        observed["result"]["settingsReadError"]
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(agent.get_session(shared.as_str().unwrap()).is_some());
    let unchanged: String = database
        .query_row(
            "SELECT document FROM settings_documents WHERE source='user/schedule'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        unchanged, "{",
        "retention does not reset an unreadable source"
    );

    let document: Value = serde_json::from_str(&document).unwrap();
    database
        .execute(
            "UPDATE settings_documents SET document=?1 WHERE source='user/schedule'",
            [document.to_string()],
        )
        .unwrap();
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_ok(
        &client
            .rpc("schedule.delete", json!({"job_id": reference}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2027-01-01T00:01:00Z"}),
            )
            .await,
    );
    let observed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let probe = client.rpc("schedulerprobe.status", json!({})).await;
            assert_ok(&probe);
            if !probe["result"]["readError"].is_null() {
                break probe;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let preserved = agent.get_session(shared.as_str().unwrap()).is_some();
    std::fs::set_permissions(&path, permissions).unwrap();
    assert_eq!(observed["result"]["readError"], "io");
    assert!(
        preserved,
        "failed store reads prevent deletion after the job reference disappears"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let probe = client.rpc("schedulerprobe.configure", json!({"now":"2027-01-01T00:02:00Z",
        "gate":{"phase":"delete_reply","host":"127.0.0.1","port":gate.local_addr().unwrap().port()}})).await;
    assert_ok(&probe);
    let (release, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "native deletion precedes forgetting its ownership record"
    );
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    assert!(
        !summaries["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["sessionId"] == shared)
    );
    assert!(
        std::process::Command::new("kill")
            .args([
                "-KILL",
                &probe["result"]["pid"].as_u64().unwrap().to_string(),
            ])
            .status()
            .unwrap()
            .success()
    );
    drop(release);
    loader.restart("session-node").await.unwrap();
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(&home)["runs"].as_array().unwrap().len() != 100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recovered retention finishes in the background");
    let retained = stored(&home);
    assert_eq!(retained["runs"].as_array().unwrap().len(), 100);
    assert_eq!(retained["runs"][0]["run_id"], "history-002");
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    let sessions = summaries["result"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        2,
        "reconciliation does not launch replacement Sessions"
    );
    for id in [&external, &fork] {
        assert!(sessions.iter().any(|item| item["sessionId"] == *id));
    }
    assert!(
        agent
            .get_session(fork.as_str().unwrap())
            .unwrap()
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::InputMessage(_))),
        "retention preserves the user's fork and its conversation"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_retention_keeps_active_runs_and_deleted_jobs_unsettled_reports() {
    let home = home();
    let mut config = config(&home);
    config.session_typescript = Some(probe_rows());
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send.clone()));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    let created = client.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    let mut jobs = Vec::new();
    for name in ["history", "pending"] {
        let saved = tool(
            &mut client,
            &starter,
            json!({"action":"create","job":{
            "name":name,"cwd":home,"prompt":name,"schedule":{"at":"2030-01-01T00:00:00Z"}}}),
        )
        .await;
        assert_ne!(saved.is_error, Some(true), "{saved:?}");
        jobs.push(saved.structured_content.unwrap()["job"]["id"].clone());
    }
    let accepted = client
        .rpc("schedule.run_now", json!({"job_id":jobs[0]}))
        .await;
    assert_ok(&accepted);
    let completed = settled(&mut client, &jobs[0], &accepted["result"]["run_id"]).await;
    session_typescript::fixture_layer(
        &loader,
        "retention-fixture",
        Some(vec![
            serde_json::from_value(json!({"id":"scheduler","disabled":true})).unwrap(),
        ]),
    )
    .await;
    let runs: Vec<_> = (0..100)
        .map(|index| {
            let mut run = completed.clone();
            run["run_id"] = json!(format!("history-{index:03}"));
            run["submission_id"] = json!(format!("scheduler:history-{index:03}"));
            run["created_at"] = json!("2026-01-01T00:00:00.000Z");
            run["ended_at"] = json!(format!("2026-01-01T00:00:00.{index:03}Z"));
            run
        })
        .collect();
    std::fs::write(
        home.join("scheduler/runs.json"),
        serde_json::to_vec(
            &json!({"version":1,"text":json!({"version":1,"runs":runs}).to_string()}),
        )
        .unwrap(),
    )
    .unwrap();
    session_typescript::fixture_layer(&loader, "retention-fixture", None).await;
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    let history_gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"gate":{"phase":"history_reply",
        "host":"127.0.0.1","port":history_gate.local_addr().unwrap().port()}}),
            )
            .await,
    );
    let active = client
        .rpc("schedule.run_now", json!({"job_id":jobs[0]}))
        .await;
    assert_ok(&active);
    let (mut history_release, _) =
        tokio::time::timeout(Duration::from_secs(10), history_gate.accept())
            .await
            .unwrap()
            .unwrap();
    let report_gate = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"gate":{"phase":"report_input",
        "host":"127.0.0.1","port":report_gate.local_addr().unwrap().port()}}),
            )
            .await,
    );
    let pending = client
        .rpc("schedule.run_now", json!({"job_id":jobs[1]}))
        .await;
    assert_ok(&pending);
    let (mut report_release, _) =
        tokio::time::timeout(Duration::from_secs(10), report_gate.accept())
            .await
            .unwrap()
            .unwrap();
    let pending_id = pending["result"]["run_id"].as_str().unwrap();
    assert_ok(
        &client
            .rpc("schedule.delete", json!({"job_id":jobs[1]}))
            .await,
    );
    let protected = stored(&home);
    assert_eq!(protected["runs"].as_array().unwrap().len(), 102);
    let active_run = protected["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run_id"] == active["result"]["run_id"])
        .unwrap();
    let pending_run = protected["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["run_id"] == pending_id)
        .unwrap();
    assert!(matches!(
        active_run["state"].as_str(),
        Some("submitted" | "running")
    ));
    assert_eq!(pending_run["state"], "completed");
    let pending_session = pending_run["session_id"].clone();
    for run in [active_run, pending_run] {
        assert!(
            agent
                .get_session(run["session_id"].as_str().unwrap())
                .is_some()
        );
    }
    assert_eq!(reports(&starter, pending_id).await, 0);
    assert_ok(
        &client
            .rpc("schedule.delete", json!({"job_id":jobs[0]}))
            .await,
    );
    let remaining = stored(&home);
    assert_eq!(remaining["runs"].as_array().unwrap().len(), 2);
    for id in [&active["result"]["run_id"], &pending["result"]["run_id"]] {
        assert!(
            remaining["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|run| run["run_id"] == *id)
        );
    }
    assert!(
        agent
            .get_session(active_run["session_id"].as_str().unwrap())
            .is_some()
    );
    history_release.write_all(b"history").await.unwrap();
    drop(history_release);
    tokio::time::timeout(Duration::from_secs(10), async {
        while stored(&home)["runs"].as_array().unwrap().len() != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        reports(&starter, active["result"]["run_id"].as_str().unwrap()).await,
        1
    );
    assert!(
        stored(&home)["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run_id"] == pending_id)
    );
    assert!(
        agent
            .get_session(pending_session.as_str().unwrap())
            .is_some()
    );
    report_release.write_all(b"report").await.unwrap();
    drop(report_release);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !stored(&home)["runs"].as_array().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(reports(&starter, pending_id).await, 1);
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    assert!(
        !summaries["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["sessionId"] == pending_session)
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_retention_retries_real_delete_and_record_failures_with_no_jobs() {
    use std::os::unix::fs::PermissionsExt;
    for fault in ["delete", "record"] {
        let home = home();
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        let (server, _) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let saved = client
            .rpc(
                "schedule.save",
                json!({"job":{"name":"owner","cwd":home,"prompt":"completed",
            "schedule":{"at":"2030-01-01T00:00:00Z"}}}),
            )
            .await;
        assert_ok(&saved);
        let job = &saved["result"]["job"]["id"];
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let run = settled(&mut client, job, &accepted["result"]["run_id"]).await;
        let owned = &run["session_id"];
        let reused = client
            .rpc(
                "schedule.save",
                json!({"job":{"name":"reference","cwd":home,"prompt":"later",
            "schedule":{"at":"2030-01-01T00:00:00Z"},"mode":"reuse","session_id":owned}}),
            )
            .await;
        assert_ok(&reused);
        let reused = &reused["result"]["job"]["id"];
        assert_ok(&client.rpc("schedule.delete", json!({"job_id":job})).await);
        assert_eq!(stored(&home)["runs"][0]["session_id"], *owned);
        let external = client
            .rpc("session.create", json!({"cwd":home,"tools":[]}))
            .await;
        assert_ok(&external);
        let external = &external["result"]["sessionId"];
        let output = home
            .join("command-output")
            .join(owned.as_str().unwrap())
            .join("locked");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("exec.log"), b"owned output").unwrap();
        let locked = if fault == "delete" {
            output.clone()
        } else {
            home.join("scheduler")
        };
        let permissions = std::fs::metadata(&locked).unwrap().permissions();
        let bytes = std::fs::read(home.join("scheduler/runs.json")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let denied = std::fs::write(locked.join("probe"), b"")
            .unwrap_err()
            .kind();
        let deleted = client
            .rpc("schedule.delete", json!({"job_id":reused}))
            .await;
        let retained = std::fs::read(home.join("scheduler/runs.json")).unwrap();
        let observed = client.rpc("schedulerprobe.status", json!({})).await;
        let availability = if fault == "delete" {
            client
                .rpc("session.attach", json!({"sessionId":owned}))
                .await
        } else {
            Value::Null
        };
        std::fs::set_permissions(&locked, permissions).unwrap();
        assert_eq!(denied, std::io::ErrorKind::PermissionDenied);
        assert_ok(&deleted);
        assert_ok(&observed);
        assert_eq!(retained, bytes);
        if fault == "record" {
            assert_eq!(observed["result"]["replacementError"], "io");
        } else {
            assert!(output.join("exec.log").exists());
            assert_eq!(availability["error"]["data"]["appError"], "session_closed");
        }
        let listed = client.rpc("schedule.list", json!({})).await;
        assert_eq!(listed["result"]["jobs"], json!([]));
        assert_ok(
            &client
                .rpc(
                    "schedulerprobe.configure",
                    json!({"now":run["deadline_at"]}),
                )
                .await,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while !stored(&home)["runs"].as_array().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!output.exists());
        let summaries = client.rpc("session.summaries", json!({})).await;
        assert_ok(&summaries);
        let sessions = summaries["result"].as_array().unwrap();
        assert!(
            !sessions
                .iter()
                .any(|session| session["sessionId"] == *owned)
        );
        assert!(
            sessions
                .iter()
                .any(|session| session["sessionId"] == *external)
        );
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_native_deadlines_withdraw_held_input_and_stop_only_consumed_work() {
    for mode in ["held", "consumed", "user_stop"] {
        let home = home();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut config = config(&home);
        config.session_typescript = Some(probe_rows());
        config.session_defaults.tools = Some(vec!["write".into(), "schedule".into()]);
        let (server, agent) = launch_with_agent(Arc::new(NamedTools(calls.clone())), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let mut job = json!({"name":mode,"cwd":home,"prompt":"tool:write {}",
            "schedule":{"at":"2030-01-01T00:00:00Z"},"policy":"workspace-write","max_run_minutes":1});
        let mut first_approval = Value::Null;
        if mode == "held" {
            let target = client
                .rpc(
                    "session.create",
                    json!({"cwd":home,"permissionPolicy":{"preset":"workspace-write"}}),
                )
                .await;
            assert_ok(&target);
            let target = &target["result"]["sessionId"];
            assert_ok(
                &client
                    .rpc("session.attach", json!({"sessionId":target}))
                    .await,
            );
            assert_ok(
                &client
                    .rpc(
                        "session.submit",
                        json!({"sessionId":target,"branchId":"main","text":"tool:write {}"}),
                    )
                    .await,
            );
            first_approval = client.notice("approval.request").await["params"]["requestId"].clone();
            job["mode"] = json!("reuse");
            job["session_id"] = target.clone();
            job["prompt"] = json!("queued run");
        }
        let job = client.rpc("schedule.save", json!({"job":job})).await;
        assert_ok(&job);
        let job = &job["result"]["job"]["id"];
        let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
        assert_ok(&accepted);
        let id = &accepted["result"]["run_id"];
        let submitted = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let history = client.rpc("schedule.history", json!({"job_id":job})).await;
                assert_ok(&history);
                let run = &history["result"]["runs"][0];
                if run["input_event_id"].is_string() {
                    return run.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let session = &submitted["session_id"];
        let executing = agent.get_session(session.as_str().unwrap()).unwrap();
        if mode != "held" {
            assert_ok(
                &client
                    .rpc("session.attach", json!({"sessionId":session}))
                    .await,
            );
            client.notice("approval.request").await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let own_input = submitted["input_event_id"].as_str().unwrap();
        let events = executing.local("main", ..).await.unwrap();
        assert_eq!(events.iter().any(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == own_input)), mode != "held");
        if mode == "user_stop" {
            assert_ok(
                &client
                    .rpc(
                        "session.stop",
                        json!({"sessionId":session,"branchId":"main"}),
                    )
                    .await,
            );
        } else {
            assert_ok(
                &client
                    .rpc(
                        "schedulerprobe.configure",
                        json!({"now":submitted["deadline_at"]}),
                    )
                    .await,
            );
        }
        let ended = settled(&mut client, job, id).await;
        assert_eq!(
            ended["state"],
            if mode == "user_stop" {
                "cancelled"
            } else {
                "timed_out"
            },
            "{ended}"
        );
        assert_eq!(ended["deadline_at"], submitted["deadline_at"]);
        assert!(ended.get("report_to").is_none());
        let events = executing.local("main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
                .count(),
            usize::from(mode != "held")
        );
        if mode == "held" {
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == own_input)));
            assert_ok(
                &client
                    .rpc(
                        "approval.respond",
                    json!({"sessionId":session,"branchId":"main","requestId":first_approval,"decision":"allow"}),
                    )
                    .await,
            );
            assert_ok(
                &client
                    .rpc(
                        "session.idle",
                        json!({"sessionId":session,"branchId":"main"}),
                    )
                    .await,
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let events = executing.local("main", ..).await.unwrap();
            assert!(!events.iter().any(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == own_input)));
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_native_clock_preserves_anchors_collapses_gaps_and_handles_dst_once() {
    let home = home();
    let mut config = config(&home);
    config.session_typescript = Some(probe_rows());
    let (server, _) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-03-08T06:59:00Z"}),
            )
            .await,
    );
    let gap = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"gap","cwd":home,"prompt":"clock run",
        "schedule":{"cron":"30 2 * * *","tz":"America/New_York"}}}),
        )
        .await;
    assert_ok(&gap);
    let gap = &gap["result"]["job"]["id"];
    let listed = client.rpc("schedule.list", json!({})).await;
    assert_eq!(
        listed["result"]["jobs"][0]["next_window"],
        "2026-03-09T06:30:00.000Z"
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-03-08T07:30:00Z"}),
            )
            .await,
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let history = client.rpc("schedule.history", json!({"job_id":gap})).await;
    assert_eq!(history["result"]["runs"], json!([]));
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-03-09T06:30:00Z"}),
            )
            .await,
    );
    assert_eq!(
        window(&mut client, gap, "2026-03-09T06:30:00.000Z").await["state"],
        "completed"
    );
    assert_ok(
        &client
            .rpc(
                "schedule.set_enabled",
                json!({"job_id":gap,"enabled":false}),
            )
            .await,
    );

    let once = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"once","cwd":home,"prompt":"one run",
        "schedule":{"at":"2026-03-09T06:31:00Z"}}}),
        )
        .await;
    assert_ok(&once);
    let once = &once["result"]["job"]["id"];
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-03-09T06:31:00Z"}),
            )
            .await,
    );
    assert_eq!(
        window(&mut client, once, "2026-03-09T06:31:00.000Z").await["state"],
        "completed"
    );
    let job = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let listed = client.rpc("schedule.list", json!({})).await;
            assert_ok(&listed);
            let job = listed["result"]["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == *once)
                .unwrap();
            if job["enabled"] == false {
                break job.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the timer disables the one-off job");
    assert_eq!(job["enabled"], false);
    assert!(job.get("enabled_at").is_none());

    let every = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"every","cwd":home,"prompt":"interval run",
        "schedule":{"every_minutes":2}}}),
        )
        .await;
    assert_ok(&every);
    let original = &every["result"]["job"];
    let every = &original["id"];
    assert_eq!(
        window(&mut client, every, "2026-03-09T06:31:00.000Z").await["state"],
        "completed"
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-03-09T06:32:30Z"}),
            )
            .await,
    );
    let renamed = client
        .rpc(
            "schedule.save",
            json!({"job":{"id":every,"name":"same anchor"}}),
        )
        .await;
    assert_ok(&renamed);
    assert_eq!(
        renamed["result"]["job"]["enabled_at"],
        original["enabled_at"]
    );
    assert_ok(
        &client
            .rpc(
                "schedule.set_enabled",
                json!({"job_id":every,"enabled":false}),
            )
            .await,
    );
    let enabled = client
        .rpc(
            "schedule.set_enabled",
            json!({"job_id":every,"enabled":true}),
        )
        .await;
    assert_ok(&enabled);
    assert_eq!(
        enabled["result"]["job"]["enabled_at"],
        "2026-03-09T06:32:30.000Z"
    );
    assert_eq!(
        window(&mut client, every, "2026-03-09T06:32:30.000Z").await["state"],
        "completed"
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-03-09T06:40:00Z"}),
            )
            .await,
    );
    assert_eq!(
        window(&mut client, every, "2026-03-09T06:38:30.000Z").await["state"],
        "missed"
    );
    let history = client
        .rpc("schedule.history", json!({"job_id":every}))
        .await;
    assert_eq!(history["result"]["runs"].as_array().unwrap().len(), 3);
    assert_ok(
        &client
            .rpc(
                "schedule.set_enabled",
                json!({"job_id":every,"enabled":false}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "schedule.set_enabled",
                json!({"job_id":once,"enabled":true}),
            )
            .await,
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let history = client.rpc("schedule.history", json!({"job_id":once})).await;
    assert_eq!(history["result"]["runs"].as_array().unwrap().len(), 1);

    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-11-01T05:29:00Z"}),
            )
            .await,
    );
    let fold = client
        .rpc(
            "schedule.save",
            json!({"job":{"name":"fold","cwd":home,"prompt":"fold run",
        "schedule":{"cron":"30 1 * * *","tz":"America/New_York"}}}),
        )
        .await;
    assert_ok(&fold);
    let fold = &fold["result"]["job"]["id"];
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-11-01T05:30:00Z"}),
            )
            .await,
    );
    assert_eq!(
        window(&mut client, fold, "2026-11-01T05:30:00.000Z").await["state"],
        "completed"
    );
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-11-01T06:30:00Z"}),
            )
            .await,
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let history = client.rpc("schedule.history", json!({"job_id":fold})).await;
    assert_eq!(history["result"]["runs"].as_array().unwrap().len(), 1);
    assert_ok(
        &client
            .rpc(
                "schedulerprobe.configure",
                json!({"now":"2026-11-02T06:30:00Z"}),
            )
            .await,
    );
    assert_eq!(
        window(&mut client, fold, "2026-11-02T06:30:00.000Z").await["state"],
        "completed"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_owner_reload_tracks_the_existing_native_run_without_replaying_input_or_reports()
 {
    let owner_name = rows().rows[0]["name"].clone();
    let home = home();
    let calls = Arc::new(AtomicUsize::new(0));
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut settings = config(&home);
    settings.session_defaults.tools = Some(vec!["write".into(), "schedule".into()]);
    let (server, agent) = launch_with(Arc::new(NamedTools(calls.clone())), settings, |builder| {
        builder.require_feature(session_typescript::ControlledGate::feature(send.clone()));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    process_typescript::select_model(&mut client).await;
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    let created = client.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let starter = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let saved = tool(
        &mut client,
        &starter,
        json!({"action":"create","job":{
        "name":"pending approval","schedule":{"at":"2030-01-01T00:00:00Z"},
        "prompt":"tool:write {}","policy":"workspace-write"}}),
    )
    .await;
    let dim_agent_types::ToolContent::Text(content) = &saved.content else {
        panic!("schedule returns text")
    };
    assert_ne!(saved.is_error, Some(true), "{saved:?}");
    let saved: Value = serde_json::from_str(content).unwrap();
    let job = &saved["job"]["id"];
    let accepted = client.rpc("schedule.run_now", json!({"job_id":job})).await;
    assert_ok(&accepted);
    let run_id = &accepted["result"]["run_id"];
    let before = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc("schedule.history", json!({"job_id":job})).await;
            assert_ok(&reply);
            if reply["result"]["runs"][0]["state"] == "running" {
                return reply["result"]["runs"][0].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let executing = agent
        .get_session(before["session_id"].as_str().unwrap())
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = executing.local("main", ..).await.unwrap();
            if let Some(request) = events.iter().find_map(|event| match &event.kind {
                EventKind::ApprovalRequest(request) => Some(request.approval_id.clone()),
                _ => None,
            }) {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("WorkspaceWrite run requires approval for write");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let generation = || {
        server
            .diagnostics()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == owner_name)
            .unwrap()
            .generation
    };
    let loop_generation = || {
        executing
            .branch_diagnostics("main")
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == "Loop")
            .unwrap()
            .generation
    };
    let (owner_before, loop_before) = (generation(), loop_generation());
    server.restart("Agents").await.unwrap();
    assert_eq!(
        generation(),
        owner_before,
        "the stable service does not retire Scheduler on index reload"
    );
    assert_eq!(
        loop_generation(),
        loop_before,
        "the existing native run remains loaded"
    );
    loader.restart("scheduler").await.unwrap();
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    let listed = client.rpc("schedule.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(
        listed["result"]["jobs"][0]["enabled_at"],
        saved["job"]["enabled_at"]
    );
    let current = client.rpc("schedule.history", json!({"job_id":job})).await;
    assert_ok(&current);
    assert_eq!(
        current["result"]["runs"][0]["deadline_at"],
        before["deadline_at"]
    );
    assert_eq!(
        current["result"]["runs"][0]["input_event_id"],
        before["input_event_id"]
    );
    let events = executing.local("main", ..).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        EventKind::StopInterrupt(_) | EventKind::TurnEnd(_) | EventKind::ApprovalCancelled(_)
    )));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":executing.id(),"branchId":"main",
        "requestId":request,"decision":"allow"}),
            )
            .await,
    );
    let completed = settled(&mut client, job, run_id).await;
    assert_eq!(completed["state"], "completed", "{completed}");
    assert_eq!(completed["session_id"], before["session_id"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while reports(&starter, run_id.as_str().unwrap()).await == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    loader.restart("scheduler").await.unwrap();
    process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
    assert_eq!(reports(&starter, run_id.as_str().unwrap()).await, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_invalid_inner_records_preserve_bytes_and_do_not_block_native_sessions() {
    let owner_name = rows().rows[0]["name"].clone();
    for text in [
        "{}",
        "{\"version\":2,\"runs\":[]}",
        "{\"version\":1,\"runs\":[{\"run_id\":\"incomplete\"}]}",
        "{\"version\":1,\"runs\":[{\"run_id\":\"done\",\"job_id\":\"job\",\"window\":\"manual\",\"state\":\"completed\",\"submission_id\":\"scheduler:done\",\"created_at\":\"2026-10-22T00:00:00.000Z\",\"deadline_at\":\"2026-10-22T01:00:00.000Z\",\"ended_at\":\"2026-10-22T00:00:01.000Z\",\"detail\":\"completed\"}]}",
    ] {
        let home = home();
        let bytes = serde_json::to_vec(&json!({"version":1,"text":text})).unwrap();
        std::fs::write(home.join("scheduler/runs.json"), &bytes).unwrap();
        let (server, _) = launch_with_agent(Arc::new(MockTools), config(&home)).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        let failure = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if server.diagnostics().plugins.iter().any(|part| {
                    part.name == owner_name && part.state == rutis::host::FiberState::Failed
                }) {
                    return client.rpc("schedule.list", json!({})).await;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("failed Scheduler owner: {:?}", server.diagnostics().plugins));
        assert_eq!(
            failure["error"]["data"]["appError"], "service_unavailable",
            "{failure}"
        );
        let diagnostics = server.diagnostics();
        let owner = diagnostics
            .plugins
            .iter()
            .find(|part| part.name == owner_name)
            .unwrap();
        assert_eq!(owner.state, rutis::host::FiberState::Failed, "{owner:?}");
        assert!(
            format!("{:?}", owner.error).contains("invalid_data"),
            "{owner:?}"
        );
        for name in ["sessions", "sessionPermissions", "settings", "pluginFiles"] {
            assert!(
                owner
                    .injects
                    .iter()
                    .any(|dependency| dependency.key
                        == rutis_bridge::session::host_key(name).erase()),
                "module metadata retains required {name}: {owner:?}"
            );
        }
        assert_ok(
            &client
                .rpc("session.create", json!({"cwd":home,"tools":["echo"]}))
                .await,
        );
        server.shutdown();
        server.join().await.unwrap();
        assert_eq!(
            std::fs::read(home.join("scheduler/runs.json")).unwrap(),
            bytes
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

/// The tool description and the report's guidance line as the factory
/// configuration: a candidate replaces each built-in text in the request, and
/// unset keeps the built-ins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_scheduler_factory_texts_reach_the_declaration_and_the_report() {
    const DESCRIPTION: &str = "A candidate schedule description.";
    const GUIDANCE: &str = "Read the run history before deciding.";
    for configured in [true, false] {
        let home = home();
        let mut parts = rows();
        if configured {
            parts.rows[0]["config"] =
                json!({"description": DESCRIPTION, "report_guidance": GUIDANCE});
        }
        let mut config = config(&home);
        config.session_typescript = Some(parts);
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        process_typescript::select_model(&mut client).await;
        process_typescript::files_available(&mut client, "schedule.list", json!({})).await;
        let created = client
            .rpc(
                "session.create",
                json!({"cwd":home,"interactive":true,"tools":["echo","schedule"]}),
            )
            .await;
        assert_ok(&created);
        let starter = agent
            .get_session(created["result"]["sessionId"].as_str().unwrap())
            .unwrap();
        let saved = tool(
            &mut client,
            &starter,
            json!({"action":"create","job":{"name":"factory",
                "schedule":{"at":"2030-01-01T00:00:00Z"},"prompt":"Review the project"}}),
        )
        .await;
        assert_ne!(saved.is_error, Some(true), "{saved:?}");
        let dim_agent_types::ToolContent::Text(content) = saved.content else {
            panic!("schedule returns text")
        };
        let saved: Value = serde_json::from_str(&content).unwrap();

        // The declaration the model request carries.
        let history = starter.local("main", ..).await.unwrap();
        let description = history
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => data.request.tools.as_deref().and_then(|tools| {
                    tools
                        .iter()
                        .find(|tool| tool.name == "schedule")
                        .map(|tool| tool.description.clone())
                }),
                _ => None,
            })
            .expect("the schedule declaration");
        if configured {
            assert_eq!(description, DESCRIPTION);
        } else {
            assert!(
                description.starts_with("Manage scheduled jobs across this host"),
                "{description}"
            );
        }

        // The report the saving branch receives.
        let started = client
            .rpc("schedule.run_now", json!({"job_id":saved["job"]["id"]}))
            .await;
        assert_ok(&started);
        let run = settled(
            &mut client,
            &saved["job"]["id"],
            &started["result"]["run_id"],
        )
        .await;
        assert_eq!(run["state"], "completed", "{run}");
        let submission = format!("scheduler-report:{}", run["run_id"].as_str().unwrap());
        let text = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let history = starter.local("main", ..).await.unwrap();
                if let Some(text) = history.iter().find_map(|event| match &event.kind {
                    EventKind::InputMessage(data)
                        if data.submission_id.as_deref() == Some(&submission) =>
                    {
                        data.blocks
                            .first()
                            .map(|dim_agent_types::InputBlock::Raw { text, .. }| text.clone())
                    }
                    _ => None,
                }) {
                    break text;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the report reaches the saving branch");
        if configured {
            assert!(
                text.ends_with(&format!("\n{GUIDANCE}</dim-scheduler>")),
                "{text}"
            );
            assert!(
                !text.contains("Inspect schedule.history for this job."),
                "{text}"
            );
        } else {
            assert!(
                text.ends_with("\nInspect schedule.history for this job.</dim-scheduler>"),
                "{text}"
            );
        }
        server.shutdown();
        server.join().await.unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}
