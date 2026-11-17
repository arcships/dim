//! The `schedule` tool through a real App Server: a scripted model calls the
//! tool from a Session, the process owner saves a `session` target at that
//! branch, a due window writes one input there, and the declarations and
//! refusals are as the design states (A.2, §5.1).
use dim_agent_capabilities::{defaults, history::HistoryRenderer, host::*};
use dim_agent_runtime::{AgentBuilder, AgentConfig, CollaborationRole, SessionConfig};
use dim_agent_scheduler::install;
use dim_agent_types::*;
use dim_app_server::{ServerConfig, serve};
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
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn texts(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect()
    }
    /// The user texts of the first request recorded after the first `count`
    /// that carries `marker`. A run id is written by its own delivery only, so
    /// that request is the delivered turn's.
    fn delivered_turn(&self, count: usize, marker: &str) -> Option<Vec<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .skip(count)
            .map(|request| {
                request
                    .messages
                    .iter()
                    .filter(|message| message.role == Role::User)
                    .flat_map(|message| match &message.content {
                        MessageContent::Text(text) => vec![text.clone()],
                        MessageContent::Parts(parts) => parts
                            .iter()
                            .filter_map(|part| match part {
                                MessagePart::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>()
            })
            .find(|texts| texts.iter().any(|text| text.contains(marker)))
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
            // Recorded and claimed in one step: a recorded request has already
            // taken its batch, whatever else is pushed for later Sessions.
            let (id, batch) = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                let id = format!("m{}", requests.len());
                (id, self.batches.lock().unwrap().pop_front())
            };
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let reason = match batch {
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

fn call(id: &str, input: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "schedule".into(),
        input: input.as_object().unwrap().clone(),
    }
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

/// A selected model, as the App Server's profile needs before a Session runs.
/// The scripted model answers every request, so the endpoint is never called.
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

async fn create_session(client: &mut Client, home: &Path, interactive: bool) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["schedule"],
                "interactive": interactive,
                "permissionPolicy": {"preset": "full-access", "allowedTools": [], "disallowedTools": []}
            }),
        )
        .await
        .unwrap();
    created["sessionId"].as_str().unwrap().to_owned()
}

/// The protocol sentences §5.1 puts in the `schedule` declaration.
/// The `dim-scheduler` guide entry's own sentence (design §5.2).
const GUIDE_ENTRY: &str = "a scheduled job's saved prompt, delivered when the job runs";

const PROTOCOL_SENTENCES: [&str; 4] = [
    "Manage scheduled jobs across this host, including other workspaces.",
    "A job delivers its saved prompt to the branch that saved it, on its schedule.",
    "A job saved by a client may launch a new Session instead.",
    "A Session may save at most 16 jobs.",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_schedule_tool_saves_jobs_for_interactive_sessions_only() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();

    let model = Arc::new(Script::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
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
        client_name: "scheduler-tool-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;

    let interactive = create_session(&mut client, home.path(), true).await;
    model.push(vec![call(
        "c1",
        json!({"action": "create", "job": {
            "name": "nightly", "schedule": {"at": "2030-01-01T00:00:00Z"},
            "prompt": "Summarize the day.", "input_mode": "steer"}}),
    )]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": interactive, "branchId": "main", "text": "schedule it"}),
        )
        .await
        .unwrap();
    wait_for("the saved job", || {
        model.texts().contains("prompt_truncated")
    })
    .await;

    let texts = model.texts();
    for sentence in PROTOCOL_SENTENCES {
        assert!(
            texts.contains(sentence),
            "the declaration states: {sentence}"
        );
    }
    assert!(
        !texts.contains("Inspect the old-to-new authority transition"),
        "the declaration carries no policy sentence"
    );
    assert!(
        texts.contains(GUIDE_ENTRY),
        "the dim-scheduler guide entry reaches the model"
    );
    assert!(
        !texts.contains("transition"),
        "a tool save carries no authority transition: {texts}"
    );

    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let saved = &listed["jobs"][0];
    assert_eq!(saved["name"], "nightly", "{listed}");
    assert_eq!(
        saved["created_by"],
        json!({"tool": {"session_id": interactive, "branch_id": "main"}}),
        "{listed}"
    );
    assert_eq!(
        saved["target"],
        json!({"kind": "session", "session_id": interactive, "branch_id": "main",
            "input_mode": "steer"}),
        "{listed}"
    );
    assert!(saved["policy_cap"].is_null(), "{listed}");
    assert!(saved["cwd"].is_null(), "{listed}");

    let job_id = listed["jobs"][0]["id"].as_str().unwrap().to_owned();
    let before = model.count();
    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    assert_eq!(accepted["state"], "delivering", "{accepted}");
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();
    let run = wait_for_delivery(&mut client, &job_id, &run_id).await;
    assert_eq!(run["state"], "delivered", "{run}");
    assert!(run["input_event_id"].is_string(), "{run}");
    assert_eq!(run["branch_id"], "main", "{run}");
    assert!(run["session_origin"].is_null(), "{run}");
    assert!(run["report_to"].is_null(), "{run}");

    let delivered = run["input_event_id"].as_str().unwrap();
    let input = delivered_input(&mut client, &interactive, delivered).await;
    assert!(
        input.as_deref().is_some_and(|text| text.contains(&format!(
            "<dim-scheduler kind=\"scheduled-run\" job=\"{job_id}\" run=\"{run_id}\">"
        ))),
        "the delivery writes its own marker: {input:?}"
    );
    assert!(
        input
            .as_deref()
            .is_some_and(|text| text.contains("Summarize the day.")),
        "the delivery writes the saved prompt: {input:?}"
    );
    wait_for("this run's marker to reach the model", || {
        model.delivered_turn(before, &run_id).is_some()
    })
    .await;
    let turn = model.delivered_turn(before, &run_id).unwrap();
    assert!(
        turn.iter().any(|text| text.contains(&format!(
            "<dim-scheduler kind=\"scheduled-run\" job=\"{job_id}\" run=\"{run_id}\">"
        ))),
        "the delivered turn carries this run's marker: {turn:?}"
    );
    assert!(
        turn.iter().any(|text| text == "Summarize the day."),
        "the delivered turn carries the saved prompt: {turn:?}"
    );
    assert!(
        !turn.iter().any(|text| text.contains("scheduler-report:")),
        "a session delivery sends no report: {turn:?}"
    );

    let member = create_session(&mut client, home.path(), false).await;
    model.push(vec![call("c2", json!({"action": "list"}))]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": member, "branchId": "main", "text": "list"}),
        )
        .await
        .unwrap();
    wait_for("the refusal", || {
        model
            .texts()
            .contains("available only in interactive leader Sessions")
    })
    .await;

    // The tool cannot name a target, a model, an Agent or a policy.
    model.push(vec![call(
        "c3",
        json!({"action": "update", "job_id": job_id,
            "job": {"target": {"kind": "launch", "cwd": home.path()}}}),
    )]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": interactive, "branchId": "main", "text": "retarget it"}),
        )
        .await
        .unwrap();
    wait_for("the retarget refusal", || {
        model.texts().contains("schedule: schedule_invalid")
    })
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Appendix D item 15: a Collaboration member Session is refused too, even
/// when it is interactive and its model asks the tool directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_schedule_tool_is_refused_in_a_member_session() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();

    let model = Arc::new(Script::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
            session_defaults: SessionConfig {
                collaboration_role: CollaborationRole::Member,
                ..Default::default()
            },
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
        client_name: "scheduler-member-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;

    let member = create_session(&mut client, home.path(), true).await;
    model.push(vec![call("m1", json!({"action": "list"}))]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": member, "branchId": "main", "text": "list the jobs"}),
        )
        .await
        .unwrap();
    wait_for("the member refusal", || {
        model
            .texts()
            .contains("available only in interactive leader Sessions")
    })
    .await;

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Waits for the delivery to settle and returns it.
async fn wait_for_delivery(client: &mut Client, job_id: &str, run_id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id}))
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
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the delivery settles")
}

/// The text one delivered input carries, read from its branch by the event id
/// the delivery reports: the marker block and the prompt, in that order.
async fn delivered_input(client: &mut Client, session_id: &str, input_id: &str) -> Option<String> {
    let mut after = 0;
    let mut through = None;
    let mut events = Vec::new();
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
            None => break,
        }
    }
    let event = events.iter().find(|event| event["id"] == json!(input_id))?;
    let data = &event["data"];
    let mut text = String::new();
    for block in data["blocks"].as_array().into_iter().flatten() {
        text.push_str(block["text"].as_str().unwrap_or_default());
        text.push('\n');
    }
    for block in data["user"]["content"].as_array().into_iter().flatten() {
        text.push_str(block["text"].as_str().unwrap_or_default());
        text.push('\n');
    }
    Some(text)
}

/// The `description` member of [`SchedulerTexts`] is what the `schedule`
/// declaration carries: a profile that replaces the description changes the
/// declaration and nothing else (design §5.1, model-prompts §8.1). The
/// evaluation's two arms differ only through this member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_factory_description_replaces_the_declaration() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    let model = Arc::new(Script::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    let texts = dim_agent_scheduler::SchedulerTexts {
        description: "CANDIDATE DESCRIPTION for the schedule tool.".into(),
    };
    dim_agent_scheduler::install_with(&mut builder, texts).unwrap();
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
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
        client_name: "scheduler-description-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;
    let session = create_session(&mut client, home.path(), true).await;
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "hello"}),
        )
        .await
        .unwrap();
    wait_for("the candidate description", || {
        model
            .texts()
            .contains("CANDIDATE DESCRIPTION for the schedule tool.")
    })
    .await;
    let texts = model.texts();
    assert!(
        !texts.contains(PROTOCOL_SENTENCES[0]),
        "the shipped description is replaced, not appended: {texts}"
    );
    assert!(
        texts.contains(GUIDE_ENTRY),
        "the dim-scheduler guide entry is unchanged"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_seventeenth_tool_job_of_a_session_is_refused() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    let model = Arc::new(Script::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
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
        client_name: "scheduler-tool-limit-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;
    let session = create_session(&mut client, home.path(), true).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let jobs = (0..16)
        .map(|index| {
            json!({
                "id": format!("seed-{index}"),
                "name": format!("seed {index}"),
                "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Seed.",
                "enabled": false,
                "revision": 1,
                "created_by": {"tool": {"session_id": session, "branch_id": "main"}},
            })
        })
        .collect::<Vec<_>>();
    rusqlite::Connection::open(home.path().join("dimcode.sqlite"))
        .unwrap()
        .execute(
            "UPDATE settings_documents SET document = ?1 WHERE source = 'user/schedule'",
            [json!({"jobs": jobs, "max_concurrent_runs": 4}).to_string()],
        )
        .unwrap();

    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::connect(&config).await.unwrap();
    model.push(vec![call(
        "c1",
        json!({"action": "create", "job": {"name": "one more",
            "schedule": {"at": "2030-01-01T00:00:00Z"}, "prompt": "One more."}}),
    )]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "schedule one more"}),
        )
        .await
        .unwrap();
    wait_for("the bound refusal", || {
        model.texts().contains("a Session may save at most 16 jobs")
    })
    .await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 16, "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_update_keeps_the_stored_launch_target() {
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
    let model = Arc::new(Script::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model.clone());
    install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_secs(60),
            database_path: Some(home.path().join("dimcode.sqlite")),
            product_home: Some(home.path().to_owned()),
            unix_path: Some(home.path().join("native.sock")),
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
        client_name: "scheduler-tool-target-test",
        protocol_version: 4,
    };
    let mut client = Client::connect(&config).await.unwrap();
    select_model(&mut client).await;

    let saved: Value = client
        .typed(
            "schedule.save",
            json!({"job": {"name": "client job", "schedule": {"at": "2030-01-01T00:00:00Z"},
                "prompt": "Check the inbox.",
                "target": {"kind": "launch", "cwd": home.path()}}}),
        )
        .await
        .unwrap();
    let job_id = saved["job"]["id"].as_str().unwrap().to_owned();

    let session = create_session(&mut client, home.path(), true).await;
    model.push(vec![call(
        "c1",
        json!({"action": "update", "job_id": job_id, "job": {"name": "renamed by tool"}}),
    )]);
    client
        .rpc(
            "session.submit",
            json!({"sessionId": session, "branchId": "main", "text": "rename it"}),
        )
        .await
        .unwrap();
    wait_for("the renamed job", || {
        model.texts().contains("renamed by tool")
    })
    .await;
    let listed: Value = client.typed("schedule.list", json!({})).await.unwrap();
    let job = &listed["jobs"][0];
    assert_eq!(job["name"], "renamed by tool", "{listed}");
    assert_eq!(job["target"]["kind"], "launch", "{listed}");

    let accepted: Value = client
        .typed("schedule.run_now", json!({"job_id": job_id}))
        .await
        .unwrap();
    assert_eq!(accepted["state"], "delivering", "{accepted}");
    let run = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let history: Value = client
                .typed("schedule.history", json!({"job_id": job_id, "limit": 50}))
                .await
                .unwrap();
            if let Some(run) = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["run_id"] == accepted["run_id"])
                && run["state"] != json!("delivering")
            {
                return run.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the launch target still launches");
    assert_eq!(run["state"], "delivered", "{run}");
    assert_eq!(run["session_origin"], "launch", "{run}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
