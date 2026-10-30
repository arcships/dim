//! `session.fork` through the wire (S5c §6, acceptance 3 to 8) on
//! disposable native stores.
use super::deletion::{Store, eventually, session, turn};
use super::*;
use dim_agent_runtime::Session;
use dim_agent_types::AgentEvent;

/// Kinds that would mean the fork ran work on its own.
const WORK: [&str; 6] = [
    "turn/start",
    "step/model-start",
    "model/call",
    "step/tool-call",
    "exec/started",
    "collaboration/request",
];

async fn branch(client: &mut Client, session: &str, at: u64, prompt: &str) -> String {
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":"main","at":at,"prompt":prompt}),
        )
        .await;
    assert_ok(&created);
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn fork(client: &mut Client, session: &str, at: u64) -> Value {
    client
        .rpc("session.fork", json!({"sessionId":session,"at":at}))
        .await
}
fn forked(reply: &Value) -> String {
    assert_ok(reply);
    reply["result"]["sessionId"].as_str().unwrap().to_owned()
}
async fn history(session: &Session, branch: &str) -> Vec<Arc<AgentEvent>> {
    session.history(branch, None).await.unwrap()
}
fn keys(events: &[Arc<AgentEvent>]) -> Vec<&str> {
    events.iter().map(|event| event.kind.key()).collect()
}
fn failed_creations(events: &[Arc<AgentEvent>]) -> Vec<String> {
    let mut branches: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreateFailed(data) => Some(match data {
                dim_agent_types::BranchCreateFailedData::Failed { branch, .. }
                | dim_agent_types::BranchCreateFailedData::Cancelled { branch, .. } => {
                    branch.clone()
                }
            }),
            _ => None,
        })
        .collect();
    branches.sort();
    branches
}
fn count(store: &Store, sql: &str, params: impl rusqlite::Params) -> i64 {
    store
        .connection()
        .query_row(sql, params, |row| row.get(0))
        .unwrap()
}
fn error(reply: &Value) -> (String, String) {
    (
        reply["error"]["data"]["appError"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        reply["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("expected an error reply: {reply}"))
            .to_owned(),
    )
}

/// Acceptance 3, 4 and 5: a fork at a rest point copies `main` through the
/// cut byte for byte with the content rows its events reference, copies the
/// branches `main` created by then as directories without events, and runs
/// nothing until a client sends it input, while the source's children go
/// on unaffected. A restart reloads it without a second failed creation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fork_copies_main_through_a_rest_point_and_starts_nothing() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.fork"))
    );
    let mut params = create(json!(["wait", "branch"]));
    params["interactive"] = json!(false);
    let sid = forked(&client.rpc("session.create", params).await);
    turn(&mut client, &sid, "first").await;
    let source = agent.get_session(&sid).unwrap();

    let head = source.sequence("main").unwrap();
    let done = branch(&mut client, &sid, head, "tool:wait {\"millis\":400}").await;
    let running = branch(&mut client, &sid, head, "tool:wait {\"millis\":60000}").await;
    let steered = branch(&mut client, &sid, head, "hello").await;
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":steered}))
            .await,
    );
    let mut calls = 0;
    while calls == 0 {
        calls = keys(&history(&source, &running).await)
            .into_iter()
            .filter(|key| *key == "step/tool-call")
            .count();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let at = source.sequence("main").unwrap();
    let after = branch(&mut client, &sid, at, "hello").await;
    assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":steered,"text":"steer after the cut","mode":"steer"})).await);

    let reply = fork(&mut client, &sid, at).await;
    let fid = forked(&reply);
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT preview FROM session_catalog WHERE session_id=?1",
                [&fid],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "first"
    );
    let fork = agent.get_session(&fid).unwrap();
    assert_eq!(
        reply["result"]["sequence"].as_u64().unwrap(),
        fork.sequence("main").unwrap()
    );

    let mut children = vec![done.clone(), running.clone(), steered.clone()];
    children.sort();
    let mut copied = vec![];
    for _ in 0..500 {
        copied = history(&fork, "main").await;
        if failed_creations(&copied) == children {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let prefix = source.history("main", Some(at)).await.unwrap();
    let encode = |events: &[Arc<AgentEvent>]| -> Vec<Value> {
        events
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect()
    };
    assert_eq!(encode(&copied[..at as usize]), encode(&prefix));
    let tail = &copied[at as usize..];
    assert_eq!(tail[0].kind.key(), "session/forked");
    assert_eq!(tail[1].kind.key(), "branch/resume");
    assert_eq!(failed_creations(tail), children, "{:?}", keys(tail));
    assert_eq!(fork.branches().len(), 1, "only main is active in the fork");

    let rows = |sql: &str| count(&store, sql, [&fid, &sid]);
    assert_eq!(
        rows(
            "SELECT COUNT(*) FROM timeline_events f JOIN timeline_events s ON s.session_id=?2 \
             AND s.branch_id='main' AND s.sequence=f.sequence WHERE f.session_id=?1 \
             AND f.body=s.body AND f.kind=s.kind AND f.last_sequence=s.last_sequence \
             AND f.event_id IS s.event_id"
        ),
        count(
            &store,
            "SELECT COUNT(*) FROM timeline_events WHERE session_id=?1 AND branch_id='main' AND sequence<=?2",
            rusqlite::params![sid, at as i64],
        )
    );
    let directories: Vec<(String, i64)> = store
        .connection()
        .prepare("SELECT branch_id, sequence FROM timeline_branches WHERE session_id=?1 ORDER BY branch_id")
        .unwrap()
        .query_map([&fid], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut expected: Vec<(String, i64)> = children.iter().map(|id| (id.clone(), 0)).collect();
    expected.push(("main".into(), fork.sequence("main").unwrap() as i64));
    expected.sort();
    assert_eq!(directories, expected);
    assert!(!directories.iter().any(|(id, _)| *id == after));
    assert_eq!(
        rows(
            "SELECT COUNT(*) FROM timeline_events WHERE session_id=?1 AND branch_id<>'main' AND ?2<>''"
        ),
        0
    );
    assert_eq!(
        rows(
            "SELECT COUNT(*) FROM timeline_content c WHERE c.session_id=?1 AND ?2<>'' AND NOT EXISTS \
             (SELECT 1 FROM timeline_events e WHERE e.session_id=?1 AND instr(e.body, c.hash) > 0) AND NOT EXISTS \
             (SELECT 1 FROM timeline_content n WHERE n.session_id=?1 AND n.kind='node' AND instr(n.body, c.hash) > 0)"
        ),
        0,
        "every copied content row is referenced by a copied event or is the parent of a copied node"
    );
    assert_eq!(
        rows(
            "SELECT COUNT(*) FROM timeline_content f WHERE f.session_id=?1 AND NOT EXISTS \
             (SELECT 1 FROM timeline_content s WHERE s.session_id=?2 AND s.hash=f.hash AND s.body=f.body)"
        ),
        0
    );
    assert!(
        rows("SELECT COUNT(*) FROM timeline_content WHERE session_id=?1 AND ?2<>''")
            < rows("SELECT COUNT(*) FROM timeline_content WHERE session_id=?2 AND ?1<>''"),
        "the children's requests are not copied"
    );
    let configs: Vec<String> = store
        .connection()
        .prepare("SELECT config FROM agent_sessions WHERE session_id IN (?1, ?2)")
        .unwrap()
        .query_map([&fid, &sid], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(configs.len(), 2);
    assert_eq!(configs[0], configs[1]);
    assert!(
        configs[0].contains("\"interactive\":false"),
        "{}",
        configs[0]
    );
    assert_eq!(
        rows(
            "SELECT COUNT(*) FROM session_owners f JOIN session_owners s ON s.session_id=?2 \
             AND s.agent_id=f.agent_id AND s.protocol_version=f.protocol_version WHERE f.session_id=?1"
        ),
        1
    );

    eventually("the finishing child ends", || {
        source.state(&done).is_ok_and(|state| !state.running)
    })
    .await;
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":steered}))
            .await,
    );
    assert!(history(&source, &steered).await.iter().any(|event| {
        matches!(&event.kind, EventKind::InputMessage(data)
            if data.user.as_ref().is_some_and(|user| matches!(&user.content[..],
                [dim_agent_types::ContentBlock::Text { text }] if text == "steer after the cut")))
    }));
    assert!(source.state(&running).unwrap().running);
    let tail = history(&fork, "main").await.split_off(at as usize);
    assert!(
        !keys(&tail).iter().any(|key| WORK.contains(key)),
        "{:?}",
        keys(&tail)
    );
    for child in &children {
        assert!(
            fork.state(child).is_err(),
            "{child} is not part of the fork"
        );
    }

    drop((client, fork, source, agent));
    server.shutdown();
    server.join().await.unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":fid})).await);
    let fork = agent.get_session(&fid).unwrap();
    let reloaded = history(&fork, "main").await;
    assert_eq!(failed_creations(&reloaded), children);
    assert!(
        !keys(&reloaded[at as usize..])
            .iter()
            .any(|key| WORK.contains(key))
    );
    turn(&mut client, &fid, "in the fork").await;
    assert!(keys(&history(&fork, "main").await).contains(&"turn/start"));
    drop((client, fork, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// Acceptance 6: cuts that are beyond the head, inside a batch, outside a
/// context boundary or not a rest point, unknown sources and other
/// protocols are refused and create nothing. The inputs D1 lets rest at a
/// cut, a withdrawn Queue input and a Steer input stopped before it was
/// consumed, do not refuse it, and the fork runs neither. `at` 0 forks a
/// Session with the source's configuration and no event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fork_is_refused_unless_the_cut_is_a_rest_point() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = session(&mut client).await;
    turn(&mut client, &sid, "tool:wait {\"millis\":1}").await;
    let source = agent.get_session(&sid).unwrap();
    let events = history(&source, "main").await;
    let head = source.sequence("main").unwrap();
    let sessions = || count(&store, "SELECT COUNT(*) FROM agent_sessions", []);
    let before = sessions();
    let position = |key: &str| {
        events
            .iter()
            .find(|event| event.kind.key() == key)
            .unwrap()
            .sequence
    };
    for (at, cause) in [
        (head + 1, "future"),
        (position("step/tool-call"), "without results"),
        (
            position("input/message"),
            "an input is waiting to be consumed",
        ),
        (position("turn/start"), "a turn is open"),
    ] {
        let (code, message) = error(&fork(&mut client, &sid, at).await);
        assert_eq!(code, "session_invalid_input", "{at}: {message}");
        assert!(message.contains(cause), "{at}: {message}");
    }
    assert_eq!(
        error(&fork(&mut client, &uuid::Uuid::new_v4().to_string(), 0).await).0,
        "session_closed"
    );
    for params in [
        json!({"sessionId":sid}),
        json!({"sessionId":sid,"at":-1}),
        json!({"sessionId":sid,"at":0,"branchId":"main"}),
    ] {
        assert_eq!(
            client.rpc("session.fork", params).await["error"]["code"],
            -32602
        );
    }
    let mut v2 = Client::new_version(&server, false, 2).await;
    assert_eq!(
        error(
            &v2.rpc("session.fork", json!({"sessionId":sid,"at":0}))
                .await
        )
        .0,
        "unknown_method"
    );
    assert_eq!(sessions(), before, "a refused fork creates nothing");

    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":"tool:wait {\"millis\":2000}"}),
            )
            .await,
    );
    let held = client
        .rpc(
            "session.submit",
            json!({"sessionId":sid,"branchId":"main","text":"held","mode":"queue"}),
        )
        .await;
    let held = held["result"]["event"]["id"].as_str().unwrap().to_owned();
    let withdrawn = client
        .rpc(
            "session.input.withdraw",
            json!({"sessionId":sid,"branchId":"main","inputId":held}),
        )
        .await;
    assert_eq!(withdrawn["result"]["status"], "deleted", "{withdrawn}");
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    let withdrawn_at = source.sequence("main").unwrap();
    assert!(keys(&history(&source, "main").await).contains(&"input/deleted"));
    let rested = forked(&fork(&mut client, &sid, withdrawn_at).await);
    let rested = agent.get_session(&rested).unwrap();

    // A Steer input applied before a stop and not consumed may stay at the
    // cut (D1): a reload does not run it, and it joins the next request a
    // client's input starts in the fork.
    let calls = |events: &[Arc<AgentEvent>]| {
        keys(events)
            .into_iter()
            .filter(|key| *key == "step/tool-call")
            .count()
    };
    let before = calls(&history(&source, "main").await);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":"tool:wait {\"millis\":60000}"}),
            )
            .await,
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while calls(&history(&source, "main").await) == before {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn runs its tool batch");
    let steer = client
        .rpc(
            "session.submit",
            json!({"sessionId":sid,"branchId":"main","text":"steered","mode":"steer"}),
        )
        .await;
    let steer = steer["result"]["event"]["id"].as_str().unwrap().to_owned();
    let input = |events: &[Arc<AgentEvent>], applied: bool| {
        events.iter().any(|event| match &event.kind {
            EventKind::InputApplied(data) if applied => data.input_id == steer,
            EventKind::InputConsumed(data) if !applied => data.input_id == steer,
            _ => false,
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !input(&history(&source, "main").await, true) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the steer is applied during the tool batch");
    let main = json!({"sessionId":sid,"branchId":"main"});
    assert_ok(&client.rpc("session.stop", main.clone()).await);
    assert_ok(&client.rpc("session.idle", main).await);
    assert!(
        !input(&history(&source, "main").await, false),
        "the stop left the steer unconsumed"
    );
    let at = source.sequence("main").unwrap();
    let fid = forked(&fork(&mut client, &sid, at).await);
    let stopped = agent.get_session(&fid).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    for (fork, at) in [(&rested, withdrawn_at), (&stopped, at)] {
        let tail = history(fork, "main").await.split_off(at as usize);
        assert!(
            !keys(&tail).iter().any(|key| WORK.contains(key)),
            "{:?}",
            keys(&tail)
        );
    }
    turn(&mut client, &fid, "in the fork").await;
    assert!(
        input(&history(&stopped, "main").await, false),
        "the steer joins the fork's next request"
    );

    let fid = forked(&fork(&mut client, &sid, 0).await);
    let empty = history(&agent.get_session(&fid).unwrap(), "main").await;
    assert_eq!(empty[0].kind.key(), "branch/start");
    assert!(!keys(&empty).iter().any(|key| WORK.contains(key)));
    drop((client, v2, source, rested, stopped, agent));
    server.shutdown();
    server.join().await.unwrap();

    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let features = client.features.as_array().unwrap();
    assert!(features.contains(&json!("session.delete")));
    assert!(!features.contains(&json!("session.fork")));
    let sid = session(&mut client).await;
    assert_eq!(error(&fork(&mut client, &sid, 0).await).0, "unknown_method");
    assert_ok(&client.rpc("session.delete", json!({"sessionId":sid})).await);
    assert!(server.agent().unwrap().get_session(&sid).is_none());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Acceptance 7 (#127): a fork of a leader whose request to a member is
/// open at the cut records a failed result and starts no turn; the source's
/// member keeps working and its result reaches the source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fork_of_a_leader_does_not_stop_the_source_member() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": ["echo", "wait", "agent"],
                "permissionPolicy": {"allowedTools": ["echo", "wait", "agent"]}}),
        )
        .await;
    let lead = forked(&created);
    let agent_id = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let request =
        json!({"action":"request","agentId":agent_id,"prompt":"tool:wait {\"millis\":1500}"});
    turn(&mut client, &lead, &format!("tool:agent {request}")).await;
    let leader = agent.get_session(&lead).unwrap();
    let events = history(&leader, "main").await;
    let member = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationDelivered(data) => match &data.delivery {
                dim_agent_types::CollaborationDelivery::Ok { session_id, .. } => {
                    Some(session_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .expect("the request was delivered");
    assert!(
        !keys(&events).contains(&"collaboration/result"),
        "the request is open at the cut"
    );
    let member_session = agent.get_session(&member).unwrap();
    assert!(member_session.state("main").unwrap().running);

    let at = leader.sequence("main").unwrap();
    let fid = forked(&fork(&mut client, &lead, at).await);
    let fork = agent.get_session(&fid).unwrap();
    let mut tail = vec![];
    for _ in 0..500 {
        tail = history(&fork, "main").await.split_off(at as usize);
        if keys(&tail).contains(&"collaboration/result") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let result = tail
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::CollaborationResult(data) => Some(data.clone()),
            _ => None,
        })
        .expect("the fork settles the copied request");
    assert_eq!(result.session_id, member);
    assert!(matches!(
        result.outcome,
        dim_agent_types::CollaborationOutcome::Failed { ref reason } if reason == "capability reloading"
    ));
    assert!(member_session.state("main").unwrap().running);
    assert!(
        !keys(&history(&member_session, "main").await).contains(&"stop/interrupt"),
        "the fork did not stop the source's member"
    );
    let mut reported = false;
    for _ in 0..500 {
        let events = history(&leader, "main").await;
        reported = events.iter().any(|event| {
            matches!(&event.kind, EventKind::CollaborationResult(data)
                if data.session_id == member
                    && data.outcome == dim_agent_types::CollaborationOutcome::Completed)
        });
        if reported {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reported, "the member's result reaches the source");
    let tail = history(&fork, "main").await.split_off(at as usize);
    assert!(
        !keys(&tail).iter().any(|key| WORK.contains(key)),
        "{:?}",
        keys(&tail)
    );
    assert!(
        !tail
            .iter()
            .any(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.mode == Some(dim_agent_types::InputMode::Steer)))
    );
    drop((client, fork, leader, member_session, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// Acceptance 8: a fork owns its source's Session files, keeps them after
/// the source is deleted, and its own delete releases them. A delete and a
/// fork of the same source either leave the fork with the files or refuse
/// the fork; never a fork without them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fork_shares_session_files_and_outlives_its_source() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = session(&mut client).await;
    turn(&mut client, &sid, "first").await;
    let file = store.own_file(&sid);
    let at = agent.get_session(&sid).unwrap().sequence("main").unwrap();
    let fid = forked(&fork(&mut client, &sid, at).await);
    let owners = |file: &str| {
        let mut owners: Vec<String> = store
            .connection()
            .prepare(
                "SELECT session_id FROM session_file_owners WHERE file_id=?1 ORDER BY session_id",
            )
            .unwrap()
            .query_map([file], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        owners.sort();
        owners
    };
    let mut both = vec![sid.clone(), fid.clone()];
    both.sort();
    assert_eq!(owners(&file), both);
    assert_ok(&client.rpc("session.delete", json!({"sessionId":sid})).await);
    assert_eq!(owners(&file), std::slice::from_ref(&fid));
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":fid})).await);
    assert_eq!(
        std::fs::read(store.file(&file).join("paste.txt")).unwrap(),
        b"pasted"
    );
    assert_ok(&client.rpc("session.delete", json!({"sessionId":fid})).await);
    eventually("the last owner's file is swept", || {
        store.file_rows(&file) == 0 && !store.file(&file).exists()
    })
    .await;

    let sid = session(&mut client).await;
    let at = agent.get_session(&sid).unwrap().sequence("main").unwrap();
    assert_ok(&client.rpc("session.delete", json!({"sessionId":sid})).await);
    assert_eq!(
        error(&fork(&mut client, &sid, at).await).0,
        "session_closed"
    );
    for _ in 0..8 {
        let sid = session(&mut client).await;
        turn(&mut client, &sid, "raced").await;
        let file = store.own_file(&sid);
        let at = agent.get_session(&sid).unwrap().sequence("main").unwrap();
        let mut deleter = Client::new_version(&server, false, 3).await;
        let (forked, deleted) = tokio::join!(
            fork(&mut client, &sid, at),
            deleter.rpc("session.delete", json!({"sessionId":sid}))
        );
        assert_ok(&deleted);
        assert!(store.rows(&sid).is_empty());
        match forked["result"]["sessionId"].as_str() {
            Some(fid) => {
                assert_eq!(owners(&file), [fid.to_owned()]);
                assert_eq!(store.file_rows(&file), 1);
            }
            None => {
                assert_eq!(error(&forked).0, "session_closed", "{forked}");
                eventually("the source's file is swept", || store.file_rows(&file) == 0).await;
            }
        }
    }
    drop((client, agent));
    server.shutdown();
    server.join().await.unwrap();
}

/// A configured-model daemon with the local execution tools, the mock model
/// and its store in `store`'s directory: the daemon gives WorkspaceTools
/// `command-output/` there, as the product does.
pub(super) async fn launch_configured(
    store: &Store,
    work: &std::path::Path,
) -> (RunningServer, dim_agent_runtime::Agent) {
    use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    let models = Arc::new(
        AimuxModels::from_config(
            serde_json::from_value::<ModelConfigFile>(json!({
                "defaultModel":{"providerId":"local","modelId":"alpha"},
                "providers":[{"id":"local","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1",
                    "models":[{"id":"alpha","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":64}},{"id":"beta","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":64}}]}]
            }))
            .unwrap(),
        )
        .unwrap(),
    );
    let mut builder = AgentBuilder::new(AgentConfig {
        model: models.default_model().clone(),
        ..Default::default()
    });
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: store
                .0
                .join(dim_app_server::storage::COMMAND_OUTPUT_DIRECTORY),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            target_home: Some(store.0.clone()),
            model_catalog: Some(models.clone()),
            unix_path: Some({
                use std::os::unix::fs::DirBuilderExt;
                let directory = std::path::PathBuf::from("/tmp")
                    .join(format!("ds5c-{}", &uuid::Uuid::new_v4().to_string()[..8]));
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
                directory.join("server.sock")
            }),
            file_roots: vec![work.to_path_buf()],
            session_defaults: dim_agent_runtime::SessionConfig {
                cwd: work.to_str().unwrap().into(),
                model: dim_agent_types::ModelSelection {
                    provider_id: "local".into(),
                    model_id: "alpha".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                },
                tools: Some(vec!["exec".into()]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    (server, agent)
}

/// Acceptance 4, 5 and 6 with the real BackgroundExec: cuts with a task
/// running, its wake not posted or its wake not consumed are refused; a
/// child's running task does not keep `main` from resting, and the fork gets
/// that child as a directory without events; at a rest point the fork keeps
/// the source's permission preset and model and starts no command. Deleting
/// the source removes its real command output.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fork_waits_for_background_work_and_keeps_preset_and_model() {
    let store = Store::new();
    let work = store.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let (server, _) = launch_configured(&store, &work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":work,"tools":["exec","branch"],"permissionPolicy":{"preset":"workspace-write","allowedTools":["exec"]}}),
        )
        .await;
    let sid = forked(&created);
    drop(client);
    let socket = server.unix_path.clone().unwrap();
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    let (server, agent) = launch_configured(&store, &work).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    let source = agent.get_session(&sid).unwrap();
    assert!(history(&source, "main").await.iter().any(|event| matches!(
        &event.kind,
        EventKind::StopInterrupt(data) if data.scope == dim_agent_types::InterruptScope::Branch
    )));
    let main = json!({"sessionId":sid,"branchId":"main"});
    assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":"tool:exec {\"command\":\"sleep 1; printf finished\",\"runInBackground\":true,\"timeoutMs\":-1}"})).await);
    let mut events = vec![];
    for _ in 0..1000 {
        events = history(&source, "main").await;
        let keys = keys(&events);
        let ended = keys.iter().position(|key| *key == "exec/ended");
        if ended.is_some_and(|ended| keys[ended..].contains(&"turn/end")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_ok(&client.rpc("session.idle", main.clone()).await);
    assert!(store.output(&sid).exists(), "the command wrote its output");
    let position = |key: &str| {
        events
            .iter()
            .find(|event| event.kind.key() == key)
            .unwrap_or_else(|| panic!("{key} in {:?}", keys(&events)))
            .sequence
    };
    let wake = events
        .iter()
        .find(|event| {
            event
                .tags
                .iter()
                .flatten()
                .any(|tag| tag.starts_with("exec-wake-result:"))
        })
        .unwrap()
        .sequence;
    let first_turn_end = position("turn/end");
    assert!(first_turn_end < position("exec/ended"));
    for (at, cause) in [
        (first_turn_end, "a background task is running"),
        (
            position("exec/ended"),
            "a background task has not woken the branch",
        ),
        (wake, "an input is waiting to be consumed"),
    ] {
        let (code, message) = error(&fork(&mut client, &sid, at).await);
        assert_eq!(code, "session_invalid_input", "{at}: {message}");
        assert!(message.contains(cause), "{at}: {message}");
    }

    let head = source.sequence("main").unwrap();
    let child = branch(
        &mut client,
        &sid,
        head,
        "tool:exec {\"command\":\"sleep 60\",\"runInBackground\":true,\"timeoutMs\":-1}",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !keys(&source.local(&child, ..).await.unwrap()).contains(&"exec/started") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the child starts its background task");

    let policy = client.rpc("session.permission.get", main.clone()).await;
    assert_ok(&client.rpc("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":policy["result"]["version"],"policy":{"preset":"read-only","allowedTools":["exec"],"disallowedTools":[]}})).await);
    let updated = client
        .rpc(
            "session.model.update",
            json!({"sessionId":sid,"branchId":"main","providerId":"local","modelId":"beta"}),
        )
        .await;
    assert_ok(&updated);
    assert_ok(&client.rpc("session.idle", main.clone()).await);
    let policy = client.rpc("session.permission.get", main.clone()).await;
    assert_eq!(policy["result"]["policy"]["preset"], "read-only");
    let inside: i64 = store
        .connection()
        .query_row(
            "SELECT sequence FROM timeline_events WHERE session_id=?1 AND branch_id='main' \
             AND json_extract(body, '$.record.batch_end') > sequence LIMIT 1",
            [&sid],
            |row| row.get(0),
        )
        .unwrap();
    let (code, message) = error(&fork(&mut client, &sid, inside as u64).await);
    assert_eq!(code, "session_invalid_input");
    assert!(message.contains("inside an atomic batch"), "{message}");

    let at = source.sequence("main").unwrap();
    let fid = forked(&fork(&mut client, &sid, at).await);
    let fork = agent.get_session(&fid).unwrap();
    let forked_main = json!({"sessionId":fid,"branchId":"main"});
    assert_eq!(
        client
            .rpc("session.permission.get", forked_main.clone())
            .await["result"],
        policy["result"]
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while failed_creations(&history(&fork, "main").await[at as usize..]).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the fork records the child's creation as failed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tail = history(&fork, "main").await.split_off(at as usize);
    assert!(
        !keys(&tail).iter().any(|key| WORK.contains(key)),
        "{:?}",
        keys(&tail)
    );
    assert!(!store.output(&fid).exists(), "the fork ran no command");
    assert_eq!(failed_creations(&tail), std::slice::from_ref(&child));
    assert!(fork.state(&child).is_err());
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM timeline_events WHERE session_id=?1 AND branch_id=?2",
            [&fid, &child]
        ),
        0
    );
    assert!(
        !keys(&source.local(&child, ..).await.unwrap()).contains(&"exec/ended"),
        "the source's task still runs"
    );
    turn(&mut client, &fid, "in the fork").await;
    let calls: Vec<String> = history(&fork, "main")
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.model_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.last().map(String::as_str), Some("beta"), "{calls:?}");

    assert_ok(&client.rpc("session.delete", json!({"sessionId":sid})).await);
    assert!(!store.output(&sid).exists());
    assert!(store.rows(&sid).is_empty());
    assert!(!store.rows(&fid).is_empty());
    drop((client, fork, source, agent));
    let socket = server.unix_path.clone().unwrap();
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}
