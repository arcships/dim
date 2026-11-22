//! The default branch through the public RPC interface (design §9, §14, §15,
//! §20.10, §20.17, §20.21, §20.22, §20.23, §20.27).
use super::deletion::Store;
use super::*;

fn error_name(value: &Value) -> &str {
    value["error"]["data"]["appError"].as_str().unwrap()
}

/// A Session that can open lines and run a command.
fn with_branch() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch", "wait"],
        "permissionPolicy": {"preset": "full-access"}
    })
}

/// A Session whose `confirm` call needs the user's approval: only `branch` is
/// allowed, and the workspace-write preset asks for an unlisted execute.
fn with_branch_and_approvals() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch", "confirm"],
        "permissionPolicy": {"preset": "workspace-write", "allowedTools": ["branch"]}
    })
}

/// `MockTools` plus one tool whose risk needs an approval.
struct ApprovalTools;
impl ToolDriver for ApprovalTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = MockTools.definitions();
        definitions.push(ToolDefinition {
            risk: Some("execute_process".into()),
            ..dim_agent_capabilities::tool_registry::definition(
                "confirm",
                "An action the user must approve",
                json!({"type":"object"}),
            )
        });
        definitions
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        if call.name != "confirm" {
            return MockTools.execute(call, context, cancel);
        }
        Box::pin(async move { Ok(ToolResult::text("Approved")) })
    }
}

/// The last rest point of `branch`, as a client reads it.
async fn through(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["through"].as_u64().unwrap()
}

/// Opens a line the user owns, cut from `parent` at its last rest point.
async fn open_line(client: &mut Client, session: &str, parent: &str, prompt: &str) -> String {
    let at = through(client, session, parent).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":parent,"at":at,"prompt":prompt}),
        )
        .await;
    assert_ok(&created);
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `session.branches`, as a client reads it.
async fn directory(client: &mut Client, session: &str) -> Value {
    let reply = client
        .rpc("session.branches", json!({"sessionId":session}))
        .await;
    assert_ok(&reply);
    reply["result"].clone()
}

async fn switch(client: &mut Client, session: &str, branch: &str) -> Value {
    client
        .rpc(
            "session.branch_set_default",
            json!({"sessionId":session,"branchId":branch}),
        )
        .await
}

/// The status notes a line holds: one `branch:line` block per move that named
/// it, and the value it states (design §12.1, §20.21).
async fn notes(client: &mut Client, session: &str, branch: &str) -> Vec<String> {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| {
            event["data"]["blocks"]
                .as_array()?
                .iter()
                .filter_map(|block| block["text"].as_str())
                .find(|text| text.starts_with("<dim-branch-line ") && text.contains(" default=\""))
                .map(str::to_owned)
        })
        .collect()
}

/// Waits until `branch` reports a running turn.
async fn wait_running(session: &dim_agent_runtime::Session, branch: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !session.state(branch).is_ok_and(|state| state.running) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the line starts its turn");
}

/// Waits until `branch` holds `count` notes.
async fn wait_notes(client: &mut Client, session: &str, branch: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while notes(client, session, branch).await.len() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{branch} reaches {count} notes"));
}

/// Waits until `branch`'s history holds the input with `submission`.
async fn wait_submission(client: &mut Client, session: &str, branch: &str, submission: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let page = client
                .rpc(
                    "session.history",
                    json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
                )
                .await;
            assert_ok(&page);
            let held = page["result"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["data"]["submission_id"] == json!(submission));
            if held {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{branch} holds {submission}"));
}

async fn holds_input(client: &mut Client, session: &str, branch: &str, submission: &str) -> bool {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["data"]["submission_id"] == json!(submission))
}

/// §20.21: the move writes `branch/default` on the root and one note to every
/// participant that is not the root, and a refusal writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_branch_default_reports_the_new_line_and_writes_the_notes() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_eq!(directory(&mut client, &id).await["defaultBranchId"], "main");

    let moved = switch(&mut client, &id, &b).await;
    assert_ok(&moved);
    assert_eq!(moved["result"]["defaultBranchId"], json!(b));
    assert_eq!(
        session.default_branch().unwrap(),
        b,
        "the tree reads the line the fact names"
    );
    wait_notes(&mut client, &id, &b, 1).await;
    assert_eq!(
        notes(&mut client, &id, "main").await.len(),
        0,
        "the root receives no note"
    );
    let note = notes(&mut client, &id, &b).await;
    assert!(
        note[0].contains("default=\"true\""),
        "the new default branch is told it may open a line: {}",
        note[0]
    );
    let moved_directory = directory(&mut client, &id).await;
    assert_eq!(moved_directory["defaultBranchId"], json!(b));
    assert_eq!(
        moved_directory["branches"][0]["branchId"],
        json!("main"),
        "the root still comes first"
    );

    assert_ok(&switch(&mut client, &id, "main").await);
    wait_notes(&mut client, &id, &b, 2).await;
    let kept = notes(&mut client, &id, &b).await;
    assert_eq!(kept.len(), 2);
    assert!(
        kept[0].contains("default=\"true\""),
        "B keeps the note it received on the way in"
    );
    assert!(
        kept[1].contains("default=\"false\""),
        "and gains one on the way out"
    );
    assert_eq!(directory(&mut client, &id).await["defaultBranchId"], "main");

    let c = open_line(&mut client, &id, "main", "c").await;
    session.idle(&c).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    wait_notes(&mut client, &id, &b, 3).await;
    assert_ok(&switch(&mut client, &id, &c).await);
    wait_notes(&mut client, &id, &c, 1).await;
    wait_notes(&mut client, &id, &b, 3).await;
    assert_eq!(
        (
            notes(&mut client, &id, "main").await.len(),
            notes(&mut client, &id, &b).await.len(),
            notes(&mut client, &id, &c).await.len()
        ),
        (0, 4, 1),
        "main gets none, B one for each of the four moves it took part in, C one for the move to it"
    );
    assert_eq!(
        directory(&mut client, &id).await["defaultBranchId"],
        json!(c)
    );
}

/// §20.21's refusals: each answers its code and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_switch_writes_nothing() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    let c = open_line(&mut client, &id, "main", "c").await;
    session.idle(&c).await.unwrap();

    let already = switch(&mut client, &id, "main").await;
    assert_eq!(error_name(&already), "branch_already_default");
    let unknown = switch(&mut client, &id, "missing-line").await;
    assert_eq!(error_name(&unknown), "session_invalid_input");
    assert_ok(
        &client
            .rpc("session.branch_end", json!({"sessionId":id,"branchId":c}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows = directory(&mut client, &id).await;
            let ended = rows["branches"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["branchId"] == json!(c) && row["state"] == json!("ended"));
            if ended {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the end is recorded");
    let ended = switch(&mut client, &id, &c).await;
    assert_eq!(error_name(&ended), "branch_ended");
    assert_eq!(
        directory(&mut client, &id).await["defaultBranchId"],
        "main",
        "no refusal moved the default branch"
    );
    assert_eq!(
        (
            notes(&mut client, &id, "main").await.len(),
            notes(&mut client, &id, &b).await.len(),
            notes(&mut client, &id, &c).await.len()
        ),
        (0, 0, 0),
        "no refusal wrote a note"
    );
}

/// §14's compatibility rule: a v1 connection's event projection carries the
/// root alone, so a call that omits `branchId` on it stays on the root after a
/// newer connection moves the default branch (design §14, §15, §20.22). The
/// v1 stream then shows the turn's output, and the line the default names
/// holds none of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v1_submit_stays_on_the_root_after_a_switch() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut legacy = Client::new_version(&server, false, 1).await;
    let created = legacy.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    assert_eq!(session.default_branch().unwrap(), b);

    let after = through(&mut client, &id, "main").await;
    assert_ok(&legacy.rpc("session.attach", json!({"sessionId":id})).await);
    assert_ok(
        &legacy
            .rpc("session.submit", json!({"sessionId":id,"text":"from v1"}))
            .await,
    );
    session.idle("main").await.unwrap();
    let through = through(&mut client, &id, "main").await;
    let events = receive_events_through(&mut legacy, after, through).await;
    assert!(
        events
            .iter()
            .any(|event| event["type"] == json!("turn") && event["name"] == json!("end")),
        "the v1 stream shows the turn that ran on the root: {events:?}"
    );
    let root = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&root);
    assert!(
        root["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == json!("input")
                && event["data"]["user"]["content"][0]["text"] == json!("from v1")),
        "the root holds the v1 input: {root}"
    );
    let line = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":b,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&line);
    assert!(
        !line["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == json!("input")
                && event["data"]["user"]["content"][0]["text"] == json!("from v1")),
        "the line the default names holds no v1 input: {line}"
    );
}

/// §14: `session.attach` keeps `sequence` as the root's, the cutoff every
/// client built before the branch-qualified cursor reads its root transcript
/// from, and reports the line the conversation is on beside it. The two lines
/// have unequal heads in both directions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_keeps_the_root_sequence_beside_the_default_cursor() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();

    // The root reaches further than the idle line.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"root turn"}),
            )
            .await,
    );
    session.idle("main").await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    // The attach's own client-state input lands on the root after the reply's
    // cutoff: the reply reports the branch heads as the attach found them.
    let root_before = through(&mut client, &id, "main").await;
    let line_before = through(&mut client, &id, &b).await;
    let attached = client.rpc("session.attach", json!({"sessionId":id})).await;
    assert_ok(&attached);
    assert_eq!(
        attached["result"]["sequence"],
        json!(root_before),
        "the record's own sequence is the root's"
    );
    assert_eq!(attached["result"]["defaultBranch"]["branchId"], json!(b));
    assert_eq!(
        attached["result"]["defaultBranch"]["sequence"],
        json!(line_before),
        "the cursor names the line the conversation is on"
    );

    // The line reaches further than the root.
    for text in ["line turn one", "line turn two"] {
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":id,"branchId":b,"text":text}),
                )
                .await,
        );
        session.idle(&b).await.unwrap();
    }
    let line_head = through(&mut client, &id, &b).await;
    let root_head = through(&mut client, &id, "main").await;
    assert!(line_head > root_head, "the line is the longer one now");
    let attached = client.rpc("session.attach", json!({"sessionId":id})).await;
    assert_ok(&attached);
    assert_eq!(
        attached["result"]["sequence"],
        json!(root_head),
        "a longer line never becomes the root's cutoff"
    );
    assert_eq!(
        attached["result"]["defaultBranch"],
        json!({"branchId":b,"sequence":line_head})
    );
}

/// §14: `session.branches` takes the default and the rows from one tree
/// snapshot. The move away and the end of the former default complete while
/// the read waits before its snapshot, so the advertised default is open in
/// the directory the same answer carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branches_read_advertises_an_open_default() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);

    let hold = server
        .hold_next_branches()
        .expect("the ConnectionHub exists");
    let read = client
        .send("session.branches", json!({"sessionId":id}))
        .await;
    hold.entered.notified().await;
    // The competing calls land while the read waits before its snapshot.
    assert_ok(&switch(&mut client, &id, "main").await);
    assert_ok(
        &client
            .rpc("session.branch_end", json!({"sessionId":id,"branchId":b}))
            .await,
    );
    hold.release.notify_one();
    let reply = client.response(read).await;
    assert_ok(&reply);
    let default = reply["result"]["defaultBranchId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(default, "main", "the move is in the answer: {reply}");
    let row = reply["result"]["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["branchId"] == json!(default))
        .unwrap_or_else(|| panic!("the default is in the directory: {reply}"));
    assert_eq!(
        row["state"], "open",
        "the advertised default accepts input: {reply}"
    );
}

/// §20.22: a call that names no line reaches the line the fact names, and
/// `session.submit`, `session.stop` and `session.idle` are among them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_without_a_branch_targets_the_default_branch() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    let line_head = through(&mut client, &id, &b).await;
    let root_head = through(&mut client, &id, "main").await;
    assert_ne!(line_head, root_head, "the two lines have their own heads");

    let submission = uuid::Uuid::new_v4().to_string();
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"submissionId":submission,"text":"hello"}),
        )
        .await;
    assert_ok(&submitted);
    wait_submission(&mut client, &id, &b, &submission).await;
    assert!(
        !holds_input(&mut client, &id, "main", &submission).await,
        "the input is not written on the root"
    );
    assert_ok(&client.rpc("session.idle", json!({"sessionId":id})).await);

    let idle = client.rpc("session.idle", json!({"sessionId":id})).await;
    assert_ok(&idle);
    assert_eq!(
        idle["result"]["sequence"],
        json!(through(&mut client, &id, &b).await),
        "the idle cutoff is the default branch's"
    );
    let stop = client
        .rpc(
            "session.stop",
            json!({"sessionId":id,"reason":"check the line"}),
        )
        .await;
    assert_ok(&stop);
    assert_eq!(
        stop["result"]["sequence"],
        json!(through(&mut client, &id, &b).await),
        "the stop names the default branch"
    );
}

/// §20.23: a configuration method acts on the line it names, and on the
/// default branch when it names none, instead of refusing every line but the
/// root. `session.agent.update` is the one of the three this assembly serves:
/// `session.model.update` and `session.prompt.refresh` are execution Session
/// methods, advertised only with a protected profile, and their resolution is
/// the same one this test reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_change_acts_on_the_line_it_names() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);

    let profile = client
        .rpc(
            "agent.create",
            json!({"profile":{"name":"Reviewer","description":"","systemPrompt":"Review.","tools":[],"model":{"providerId":"mock","modelId":"deterministic"}}}),
        )
        .await;
    assert_ok(&profile);
    let reviewer = profile["result"]["agentId"].as_str().unwrap().to_owned();
    let unnamed = client
        .rpc(
            "session.agent.update",
            json!({"sessionId":id,"agentId":reviewer}),
        )
        .await;
    assert_ok(&unnamed);
    assert_eq!(unnamed["result"]["outcome"], "adopted");
    assert_eq!(
        adopted(&mut client, &id, &b).await.as_deref(),
        Some(reviewer.as_str())
    );
    assert_eq!(
        adopted(&mut client, &id, "main").await,
        None,
        "the update acted on the default branch, not the root"
    );
    assert_ok(
        &client
            .rpc(
                "session.agent.update",
                json!({"sessionId":id,"branchId":"main","agentId":reviewer}),
            )
            .await,
    );
    assert_eq!(
        adopted(&mut client, &id, "main").await.as_deref(),
        Some(reviewer.as_str()),
        "the named line is the one that changes"
    );
}

/// The profile `session.agent.get` answers for `branch`.
async fn adopted(client: &mut Client, session: &str, branch: &str) -> Option<String> {
    let reply = client
        .rpc(
            "session.agent.get",
            json!({"sessionId":session,"branchId":branch}),
        )
        .await;
    assert_ok(&reply);
    reply["result"]["agentId"].as_str().map(str::to_owned)
}

/// E1: the catalog's running state and recent activity follow the default
/// branch (design §14, §15; session-catalog §3.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_reads_running_and_activity_from_the_default_branch() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);

    // A wait keeps the line working while the root is idle.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"text":"tool:wait {\"millis\":5000}"}),
            )
            .await,
    );
    wait_running(&session, &b).await;
    let working = client.rpc("session.catalog", json!({})).await;
    assert_ok(&working);
    let row = working["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .expect("the Session is listed")
        .clone();
    assert_eq!(row["running"], true, "the default branch is working");

    assert_ok(&switch(&mut client, &id, "main").await);
    let rooted = client.rpc("session.catalog", json!({})).await;
    assert_ok(&rooted);
    let row = rooted["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .expect("the Session is listed")
        .clone();
    assert_eq!(
        row["running"], false,
        "the root is idle, so the Session is not working on it"
    );

    assert_ok(&switch(&mut client, &id, &b).await);
    assert_ok(&client.rpc("session.idle", json!({"sessionId":id})).await);
    let settled = client.rpc("session.catalog", json!({})).await;
    assert_ok(&settled);
    let row = settled["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .expect("the Session is listed")
        .clone();
    assert_eq!(row["running"], false);
    let line_activity = newest_activity(&mut client, &id, &b).await;
    let root_activity = newest_activity(&mut client, &id, "main").await;
    assert!(
        line_activity > root_activity,
        "the line has the newer activity: {line_activity} vs {root_activity}"
    );
    assert_eq!(
        row["updatedAt"],
        json!(line_activity),
        "the Session reads recent by its default branch"
    );
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    let summary = summaries["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .expect("the Session is summarised")
        .clone();
    assert_eq!(summary["updatedAt"], json!(line_activity));
}

/// The newest Session activity timestamp on `branch`, by the kinds the
/// catalog counts (issue #1099): a user message, a finished turn, or a
/// `branch/create`.
async fn newest_activity(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            let (kind, name) = (
                event["type"].as_str().unwrap_or_default(),
                event["name"].as_str().unwrap_or_default(),
            );
            (kind == "input" && name == "message" && event["data"]["user"].is_object())
                || (kind == "turn" && name == "end")
                || (kind == "branch" && name == "create")
        })
        .map(|event| event["create_at"].as_u64().unwrap())
        .max()
        .unwrap_or(0)
}

/// One catalog row, as a client reads it.
async fn catalog_row(client: &mut Client, session: &str) -> Value {
    let listed = client.rpc("session.catalog", json!({})).await;
    assert_ok(&listed);
    listed["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(session))
        .unwrap_or_else(|| panic!("{session} is listed: {listed}"))
        .clone()
}

/// §14: the writer republishes the new default's own activity in the switch's
/// transaction, and the switch itself is not activity, so two idle lines never
/// leave the catalog recent by the line the Session left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_switch_republishes_the_new_defaults_activity() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    // The line's turn runs while the root is the default, so the catalog
    // still reports the root's activity.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":b,"text":"line turn"}),
            )
            .await,
    );
    session.idle(&b).await.unwrap();
    let line_activity = newest_activity(&mut client, &id, &b).await;
    let root_activity = newest_activity(&mut client, &id, "main").await;
    assert!(
        line_activity > root_activity,
        "the line is the newer one: {line_activity} vs {root_activity}"
    );
    assert_eq!(
        catalog_row(&mut client, &id).await["updatedAt"],
        json!(root_activity),
        "the root is the default while the line works"
    );

    assert_ok(&switch(&mut client, &id, &b).await);
    assert_eq!(
        catalog_row(&mut client, &id).await["updatedAt"],
        json!(line_activity),
        "the switch publishes the line's own activity, not the switch's time"
    );

    // A root turn makes the root the newer line while the line is default.
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"root turn"}),
            )
            .await,
    );
    session.idle("main").await.unwrap();
    let root_activity = newest_activity(&mut client, &id, "main").await;
    assert!(
        root_activity > line_activity,
        "the root is the newer one now: {root_activity} vs {line_activity}"
    );
    assert_eq!(
        catalog_row(&mut client, &id).await["updatedAt"],
        json!(line_activity),
        "a non-default line's turn does not move the catalog"
    );
    assert_ok(&switch(&mut client, &id, "main").await);
    assert_eq!(
        catalog_row(&mut client, &id).await["updatedAt"],
        json!(root_activity),
        "the move back publishes the root's activity"
    );
}

/// §20.10: v3 clients are offered the method and read the new kind; v1 and v2
/// clients see neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v3_client_is_offered_the_switch_and_older_clients_are_not() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut older = Client::new_version(&server, false, 2).await;
    assert_eq!(older.features, Value::Null);
    let created = older.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.branch_set_default")),
        "{:?}",
        client.features
    );
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();

    // The v2 client attaches, then the move happens: its stream carries no new
    // kind, and its own write's barrier puts the move's notification ahead of
    // the reply the assertion below reads.
    assert_ok(&older.rpc("session.attach", json!({"sessionId":id})).await);
    let refused = older
        .rpc(
            "session.branch_set_default",
            json!({"sessionId":id,"branchId":b}),
        )
        .await;
    assert_eq!(error_name(&refused), "unknown_method");
    assert_ok(&switch(&mut client, &id, &b).await);
    assert_ok(
        &older
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"older client"}),
            )
            .await,
    );
    assert!(
        !older.notices.iter().any(|notice| {
            notice["params"]["event"]["type"] == json!("branch")
                && notice["params"]["event"]["name"] == json!("default")
        }),
        "a v2 client sees no new kind: {:?}",
        older.notices
    );
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&history);
    assert!(
        history["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == json!("branch") && event["name"] == json!("default")),
        "a v3 client reads the new kind"
    );
    assert_ok(
        &older
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
}

/// §20.22's approval half: a notice raised on a line is answered on that line,
/// before and after a move of the default branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_approval_answers_on_the_line_that_asked() {
    let (server, agent) = launch_with_agent(Arc::new(ApprovalTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", with_branch_and_approvals())
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);

    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"text":"tool:confirm {}"}),
            )
            .await,
    );
    let notice = client.notice("approval.request").await;
    assert_eq!(notice["params"]["branchId"], json!(b));
    let request_id = notice["params"]["requestId"].clone();
    assert_ok(&switch(&mut client, &id, "main").await);
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":id,"branchId":b,"requestId":request_id,"decision":"allow"}),
            )
            .await,
    );
    session.idle(&b).await.unwrap();
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":b,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    assert!(
        page["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == json!("step") && event["name"] == json!("tool-result")),
        "the approved call ran on the line that asked: {page}"
    );
    let root = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&root);
    assert!(
        !root["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == json!("step") && event["name"] == json!("tool-result")),
        "the root ran nothing"
    );
    assert_eq!(
        directory(&mut client, &id).await["defaultBranchId"],
        "main",
        "the move stands"
    );
}

/// §20.27, end to end: the user opens a line, works on it, continues on it,
/// restarts the daemon, and the same request still reaches it until the user
/// moves back to the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continuing_on_a_line_after_a_switch_survives_a_restart() {
    let store = Store::new();
    let config = store.config();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "work on the parser").await;
    session.idle(&b).await.unwrap();

    // The line uses a tool whose result it needs later.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":b,
                    "text":"tool:wait {\"millis\":1,\"text\":\"cached-result\"}"}),
            )
            .await,
    );
    session.idle(&b).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    let first = uuid::Uuid::new_v4().to_string();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"submissionId":first,"text":"use the result"}),
            )
            .await,
    );
    wait_submission(&mut client, &id, &b, &first).await;
    assert!(!holds_input(&mut client, &id, "main", &first).await);
    session.idle(&b).await.unwrap();

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let reply = directory(&mut client, &id).await;
    assert_eq!(
        reply["defaultBranchId"],
        json!(b),
        "the fact survives the restart"
    );
    let second = uuid::Uuid::new_v4().to_string();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"submissionId":second,"text":"still here"}),
            )
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    wait_submission(&mut client, &id, &b, &second).await;
    assert!(!holds_input(&mut client, &id, "main", &second).await);
    session.idle(&b).await.unwrap();

    assert_ok(&switch(&mut client, &id, "main").await);
    let third = uuid::Uuid::new_v4().to_string();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"submissionId":third,"text":"back on the root"}),
            )
            .await,
    );
    wait_submission(&mut client, &id, "main", &third).await;
    session.idle("main").await.unwrap();
}

/// E1: the catalog's running state and recent activity, with the default
/// branch on the root and on a line. Run with `--release --ignored
/// --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn measure_e1_catalog_with_the_default_on_the_root_and_on_a_line() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    for (label, target) in [("root", "main"), ("line", b.as_str())] {
        if target != "main" {
            assert_ok(&switch(&mut client, &id, target).await);
        }
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":id,"text":"tool:wait {\"millis\":2000}"}),
                )
                .await,
        );
        wait_running(&session, target).await;
        let started = std::time::Instant::now();
        let catalog = client.rpc("session.catalog", json!({})).await;
        let elapsed = started.elapsed();
        assert_ok(&catalog);
        let row = catalog["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sessionId"] == json!(id))
            .expect("the Session is listed")
            .clone();
        println!(
            "E1 catalog with the default on the {label}: {:.2} ms, running {}",
            elapsed.as_secs_f64() * 1e3,
            row["running"]
        );
        assert_ok(&client.rpc("session.idle", json!({"sessionId":id})).await);
        let settled = client.rpc("session.catalog", json!({})).await;
        assert_ok(&settled);
        let row = settled["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sessionId"] == json!(id))
            .expect("the Session is listed")
            .clone();
        let line = if target == "main" { "main" } else { &b };
        println!(
            "E1 catalog with the default on the {label}, idle: running {}, updatedAt {}",
            row["running"], row["updatedAt"]
        );
        assert_eq!(
            row["updatedAt"],
            json!(newest_activity(&mut client, &id, line).await)
        );
    }
    server.shutdown();
}

/// E4: a switch end to end, from the RPC call until both notes are committed.
/// Run with `--release --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn measure_e4_switch_until_both_notes_are_committed() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let b = open_line(&mut client, &id, "main", "b").await;
    session.idle(&b).await.unwrap();
    let c = open_line(&mut client, &id, "main", "c").await;
    session.idle(&c).await.unwrap();
    assert_ok(&switch(&mut client, &id, &b).await);
    wait_notes(&mut client, &id, &b, 1).await;
    let mut elapsed = Vec::new();
    for round in 0..5 {
        let (target, from, to) = if round % 2 == 0 {
            (c.clone(), b.clone(), c.clone())
        } else {
            (b.clone(), c.clone(), b.clone())
        };
        let before_from = notes(&mut client, &id, &from).await.len();
        let before_to = notes(&mut client, &id, &to).await.len();
        let started = std::time::Instant::now();
        assert_ok(&switch(&mut client, &id, &target).await);
        wait_notes(&mut client, &id, &to, before_to + 1).await;
        wait_notes(&mut client, &id, &from, before_from + 1).await;
        elapsed.push(started.elapsed());
    }
    elapsed.sort();
    println!(
        "E4 switch until both notes are committed: median {:.2} ms, max {:.2} ms",
        elapsed[elapsed.len() / 2].as_secs_f64() * 1e3,
        elapsed[elapsed.len() - 1].as_secs_f64() * 1e3
    );
    server.shutdown();
}
