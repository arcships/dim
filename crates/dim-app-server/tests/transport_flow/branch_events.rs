use super::*;
use dim_agent_runtime::BranchOptions;
use dim_agent_types::{BranchCreateAt, BranchSource, EventKind, InputMode};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_transports_encode_the_v1_branch_create_boundary_live() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut live = Client::new(&server, ws).await;
        let created = live.rpc("session.create", create(json!(["echo"]))).await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(id).unwrap();
        session
            .submit("parent".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session.stop("main", "control cursor".into()).await.unwrap();
        let high = session.sequence("main").unwrap();
        let initial = live.rpc("session.attach", json!({"sessionId":id})).await;
        assert_eq!(initial["result"], json!({}));
        let child = session
            .branch(
                "main".into(),
                high,
                BranchOptions {
                    prompt: "child".into(),
                    source: BranchSource::Tool {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        call_id: "call".into(),
                    },
                },
            )
            .await
            .unwrap();
        session.idle(&child).await.unwrap();
        let events = session.local("main", ..).await.unwrap();
        let create = events
            .iter()
            .find(|event| matches!(&event.kind, EventKind::BranchCreate(data) if data.branch == child))
            .unwrap();
        assert!(
            matches!(&create.kind, EventKind::BranchCreate(data) if data.at == BranchCreateAt::Create {} && data.branch == child)
        );
        let through_create: Vec<_> = events
            .iter()
            .filter(|event| event.sequence <= create.sequence)
            .cloned()
            .collect();
        let notices = receive_events_through(&mut live, create.sequence).await;
        assert_eq!(notices, v1_events(&through_create, high));
        assert!(notices[0]["data"].get("at").is_none());
        assert_eq!(notices[0]["sequence"], create.sequence);
        let local = session.local(&child, ..).await.unwrap();
        assert!(
            matches!(&local[0].kind, EventKind::BranchStart(data) if data.create_event == Some(create.sequence))
        );
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restored_task_branches_are_discovered_locally_and_named_stops_preserve_the_leader() {
    async fn page(client: &mut Client, session: &str, branch: &str) -> Value {
        let reply = client
            .rpc(
                "session.history",
                json!({"sessionId":session,
            "branchId":branch,"after":0,"limit":256}),
            )
            .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["done"], true);
        reply["result"].clone()
    }

    let store = super::deletion::Store::new();
    let work = store.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let (server, _) = super::fork::launch_configured(&store, &work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":work,"tools":["exec","branch"],
        "permissionPolicy":{"preset":"full-access"}}),
        )
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let mut branches = Vec::new();
    let mut receipts = Vec::new();
    let mut parent = "main".to_owned();
    for label in ["child", "nested"] {
        let parent_page = page(&mut client, &session, &parent).await;
        let prompt = format!(
            "tool:exec {}",
            json!({"command":format!("printf {label}"),
            "runInBackground":true,"timeoutMs":-1})
        );
        let created = client
            .rpc(
                "session.branch_create",
                json!({"sessionId":session,
            "parentBranchId":parent,"at":parent_page["through"],"prompt":prompt}),
            )
            .await;
        assert_ok(&created);
        let branch = created["result"]["branch"]["branchId"]
            .as_str()
            .unwrap()
            .to_owned();
        let history = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let history = page(&mut client, &session, &branch).await;
                if history["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["type"] == "exec" && event["name"] == "ended")
                {
                    break history;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let events = history["events"].as_array().unwrap();
        let started = events
            .iter()
            .find(|event| event["type"] == "exec" && event["name"] == "started")
            .unwrap();
        let task = started["data"]["task_id"].clone();
        assert!(events.iter().any(|event| event["type"] == "exec"
            && event["name"] == "ended"
            && event["data"]["task_id"] == task
            && event["data"]["status"] == "completed"));
        receipts.push(task);
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":session,"branchId":branch}),
                )
                .await,
        );
        parent = branch.clone();
        branches.push(branch);
    }
    let main = page(&mut client, &session, "main").await;
    assert!(
        !main["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "input"
                && event["name"] == "message"
                && event["data"].get("user").is_some()),
        "main has no user conversation: {main}"
    );
    let socket = server.unix_path.clone().unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());

    let (server, _) = super::fork::launch_configured(&store, &work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":session}))
            .await,
    );
    let discovered = tokio::time::timeout(Duration::from_secs(10), async {
        let mut discovered = std::collections::HashSet::new();
        while discovered.len() != 3 {
            let state = client.notice("session.state").await;
            assert_eq!(state["params"]["sessionId"], session);
            discovered.insert(state["params"]["branchId"].as_str().unwrap().to_owned());
        }
        discovered
    })
    .await
    .unwrap();
    assert_eq!(
        discovered,
        ["main".to_owned(), branches[0].clone(), branches[1].clone()]
            .into_iter()
            .collect()
    );
    for (index, branch) in branches.iter().enumerate() {
        let history = page(&mut client, &session, branch).await;
        let commands: Vec<_> = history["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["type"] == "exec" && event["name"] == "started")
            .map(|event| event["data"]["task_id"].clone())
            .collect();
        assert_eq!(
            commands,
            [receipts[index].clone()],
            "local history excludes ancestor commands"
        );
        assert!(
            history["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["type"] == "exec"
                    && event["name"] == "ended"
                    && event["data"]["task_id"] == receipts[index]
                    && event["data"]["status"] == "completed")
        );
    }
    let mut active = Vec::new();
    for branch in &branches {
        let prompt = format!(
            "tool:exec {}",
            json!({"command":"printf running; sleep 60",
            "runInBackground":true,"timeoutMs":-1})
        );
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":session,"branchId":branch,"text":prompt}),
                )
                .await,
        );
        let task = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let history = page(&mut client, &session, branch).await;
                if let Some(event) = history["events"].as_array().unwrap().iter().find(|event| {
                    event["type"] == "exec"
                        && event["name"] == "started"
                        && !receipts.contains(&event["data"]["task_id"])
                }) {
                    break event["data"]["task_id"].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        active.push(task);
    }
    for (index, branch) in branches.iter().enumerate() {
        let stopped = client
            .rpc(
                "session.exec.stop",
                json!({"sessionId":session,
            "branchId":branch,"taskId":active[index]}),
            )
            .await;
        assert_ok(&stopped);
        assert_eq!(stopped["result"]["task_id"], active[index]);
        assert_eq!(stopped["result"]["status"], "cancelled");
        let history = page(&mut client, &session, branch).await;
        assert!(
            history["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["type"] == "exec"
                    && event["name"] == "ended"
                    && event["data"]["task_id"] == active[index]
                    && event["data"]["status"] == "cancelled")
        );
        if index == 0 {
            let untouched = page(&mut client, &session, &branches[1]).await;
            assert!(
                !untouched["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["type"] == "exec"
                        && event["name"] == "ended"
                        && event["data"]["task_id"] == active[1])
            );
        }
    }
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main",
        "text":"leader remains usable after named child stops"}),
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
    let main = page(&mut client, &session, "main").await;
    assert!(
        main["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["user"]["content"][0]["text"]
                    == "leader remains usable after named child stops")
    );
    assert!(
        !main["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "exec")
    );
    server.shutdown();
    server.join().await.unwrap();
}
