use super::*;
use dim_agent_runtime::testing::branch_events;
use dim_agent_types::{BranchSource, InputMode};

fn error_name(value: &Value) -> &str {
    value["error"]["data"]["appError"].as_str().unwrap()
}

fn create_with_branch() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch"],
        "permissionPolicy": {"allowedTools": ["branch"]}
    })
}

async fn open_tool_line(
    client: &mut Client,
    session: &dim_agent_runtime::Session,
    id: &str,
) -> String {
    let text = format!(
        "tool:branch {}",
        json!({"action": "create", "prompt": "watch the tests"})
    );
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":text}),
        )
        .await;
    assert_ok(&submitted);
    session.idle("main").await.unwrap();
    let child = session
        .branches()
        .into_iter()
        .find(|info| info.parent.as_deref() == Some("main"))
        .expect("main opened a line")
        .id;
    session.idle(&child).await.unwrap();
    child
}

fn end(session: &str, branch: &str) -> Value {
    json!({"sessionId": session, "branchId": branch})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_ends_a_line_and_a_v3_client_reads_both_sides() {
    user_end_history(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_ends_a_native_line_and_a_v3_client_reads_both_sides() {
    user_end_history(true).await;
}

async fn user_end_history(native: bool) {
    use super::history_window::{BatchProbe, BatchProbePlugin};

    let directory = std::env::temp_dir().join(format!("dim-end-{}", uuid::Uuid::new_v4()));
    let batch = Arc::new(BatchProbe::default());
    let probe = batch.clone();
    let (server, agent) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            database_path: native.then(|| directory.join("dimcode.sqlite")),
            ..Default::default()
        },
        |builder| {
            builder.require_feature(
                Feature::new("end-history-batch").branch("end-history-batch", move |names| {
                    Ok(Box::new(BatchProbePlugin::new(names, probe.clone())?))
                }),
            );
        },
    )
    .await;
    let mut client = Client::new_version(&server, native, 3).await;
    let created = client.rpc("session.create", create_with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let child = open_tool_line(&mut client, &session, &id).await;
    let timeline = batch.0.lock().unwrap().clone().unwrap();
    let appended = timeline
        .append_batch_registered(
            ["first in batch", "second in batch"]
                .into_iter()
                .map(|text| {
                    EventKind::InputMessage(dim_agent_types::InputMessageData::text(
                        text,
                        Some(InputMode::Append),
                    ))
                    .into()
                })
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let idle = client
        .rpc("session.idle", json!({"sessionId":id,"branchId":child}))
        .await;
    assert_ok(&idle);
    let pin = appended[1].sequence;
    let open = history_pages(&mut client, &id, &child, pin).await;
    let mut windows = Vec::new();
    if native {
        for fields in [
            json!({"latestTurn":true,"limit":1}),
            json!({"latestTurn":true,"limit":4096}),
            json!({"before":pin,"limit":1}),
            json!({"before":appended[0].sequence,"limit":1}),
        ] {
            let mut params = json!({"sessionId":id,"branchId":child,"through":pin});
            params
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let reply = client.rpc("session.history", params.clone()).await;
            assert_ok(&reply);
            if fields["latestTurn"] == true && fields["limit"] == 1 {
                assert_eq!(reply["result"]["events"].as_array().unwrap().len(), 2);
                assert_eq!(reply["result"]["nextBefore"], appended[0].sequence);
            }
            if fields["before"].is_number() {
                assert!(
                    reply["result"]["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|event| {
                            event["sequence"].as_u64().unwrap() < appended[0].sequence
                        })
                );
            }
            windows.push((params, reply["result"].clone()));
        }
    }
    let ended = client.rpc("session.branch_end", end(&id, &child)).await;
    assert_ok(&ended);
    assert_eq!(ended["result"], json!({}));
    assert_eq!(history_pages(&mut client, &id, &child, pin).await, open);
    for (params, expected) in windows {
        let reply = client.rpc("session.history", params).await;
        assert_ok(&reply);
        assert_eq!(
            reply["result"], expected,
            "ending preserves the pinned window"
        );
    }
    if native {
        let invalid = client
            .rpc(
                "session.history",
                json!({
                    "sessionId":id,"branchId":child,"latestTurn":true,
                    "through":appended[0].sequence
                }),
            )
            .await;
        assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    }
    history_pages(&mut client, &id, &child, session.sequence(&child).unwrap()).await;
    for after in [0, appended[0].sequence] {
        let invalid = client
            .rpc(
                "session.history",
                json!({"sessionId":id,"branchId":child,"after":after,"through":appended[0].sequence}),
            )
            .await;
        assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    }
    let invalid = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":child,"after":appended[0].sequence,"through":pin}),
        )
        .await;
    assert_eq!(invalid["error"]["code"], -32602, "{invalid}");
    let whole = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":child,"after":appended[0].sequence-1,"through":pin,"limit":1}),
        )
        .await;
    assert_ok(&whole);
    assert_eq!(whole["result"]["events"].as_array().unwrap().len(), 2);
    assert_eq!(whole["result"]["nextAfter"], pin);
    assert_eq!(whole["result"]["done"], true);
    let main = branch_events(&session, "main", ..).await.unwrap();
    let result = main
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::BranchEnded(data) if data.branch == child => Some(data.clone()),
            _ => None,
        })
        .expect("main recorded the end");
    assert!(matches!(result.source, BranchSource::Host { .. }));
    assert!(result.ancestor.is_none());
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
            .any(|event| event["type"] == "branch"
                && event["name"] == "ended"
                && event["data"]["branch"] == child)
    );
    let line = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":child,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&line);
    assert_eq!(line["result"]["done"], true);
    assert!(
        line["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "branch"
                && event["name"] == "end"
                && event["data"]["source"]["kind"] == "host")
    );
    let notice = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = branch_events(&session, "main", ..).await.unwrap();
            if let Some(text) = events.iter().find_map(|event| match &event.kind {
                EventKind::InputMessage(data) if data.effective_mode() == InputMode::Steer => {
                    data.blocks.iter().find_map(|block| match block {
                        dim_agent_types::InputBlock::Raw { text, .. }
                            if text
                                .starts_with(&format!("<dim-branch-ended branch=\"{child}\">")) =>
                        {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                }
                _ => None,
            }) {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("main heard the end");
    assert!(notice.contains("It no longer receives messages; read it if you need what it did."));
    let main_end = client.rpc("session.branch_end", end(&id, "main")).await;
    assert_eq!(error_name(&main_end), "session_invalid_input");
    let again = client.rpc("session.branch_end", end(&id, &child)).await;
    assert_eq!(error_name(&again), "branch_ended");
    let late = client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":child,"text":"late"}),
        )
        .await;
    assert_eq!(error_name(&late), "branch_ended");
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.branch_end"))
    );
    server.shutdown();
    server.join().await.unwrap();
    if native {
        std::fs::remove_dir_all(directory).unwrap();
    }
}

async fn history_pages(client: &mut Client, id: &str, branch: &str, through: u64) -> Vec<Value> {
    let full = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":branch,"through":through,"limit":256}),
        )
        .await;
    assert_ok(&full);
    assert_eq!(full["result"]["done"], true);
    let mut after = 0;
    let mut events = Vec::new();
    while after < through {
        let page = client
            .rpc(
                "session.history",
                json!({"sessionId":id,"branchId":branch,"through":through,"after":after,"limit":1}),
            )
            .await;
        assert_ok(&page);
        let result = &page["result"];
        assert_eq!(result["through"], through);
        let next = result["nextAfter"].as_u64().unwrap();
        assert!(next > after && next <= through, "{page}");
        assert_eq!(result["done"], next == through);
        events.extend(result["events"].as_array().unwrap().iter().cloned());
        after = next;
    }
    assert_eq!(events, *full["result"]["events"].as_array().unwrap());
    let empty = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":branch,"through":through,"after":through,"limit":1}),
        )
        .await;
    assert_ok(&empty);
    assert_eq!(empty["result"]["events"], json!([]));
    assert_eq!(empty["result"]["nextAfter"], through);
    assert_eq!(empty["result"]["done"], true);
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v2_client_never_sees_the_new_kinds() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 2).await;
    let created = client.rpc("session.create", create_with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    let attached = client.rpc("session.attach", json!({"sessionId":id})).await;
    assert_ok(&attached);
    let child = open_tool_line(&mut client, &session, &id).await;
    session.idle(&child).await.unwrap();
    let unknown = client.rpc("session.branch_end", end(&id, &child)).await;
    assert_eq!(error_name(&unknown), "unknown_method");
    session
        .end_branch(
            &child,
            BranchSource::Host {
                operation_id: "test-end".into(),
            },
        )
        .await
        .unwrap();
    let idle = client
        .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
        .await;
    assert_ok(&idle);
    let mut seen = std::mem::take(&mut client.notices);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !seen.iter().any(|value| {
            let event = &value["params"]["event"];
            event["type"] == "input"
                && event["data"]["blocks"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.starts_with("<dim-branch-ended"))
                    })
                })
        }) {
            seen.push(client.notice("session.event").await);
        }
    })
    .await
    .expect("v2 client received the end notice");
    assert!(
        seen.iter().all(|value| {
            let event = &value["params"]["event"];
            !(event["type"] == "branch" && event["name"] == "ended")
                && !(event["type"] == "branch" && event["name"] == "end")
        }),
        "{seen:?}"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_a_running_line_reports_it_stopped() {
    let mut stale = Vec::new();
    for round in 0..12 {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let created = client
            .rpc(
                "session.create",
                json!({
                    "cwd": std::env::temp_dir(),
                    "tools": ["branch", "wait"],
                    "permissionPolicy": {"allowedTools": ["branch", "wait"]}
                }),
            )
            .await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let session = agent.get_session(&id).unwrap();
        assert_ok(&client.rpc("session.attach", json!({"sessionId": id})).await);
        let child = open_tool_line(&mut client, &session, &id).await;
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":id,"branchId":child,"text":"tool:wait {\"millis\":20000}"}),
                )
                .await,
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while !session.state(&child).unwrap().running {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the line runs");
        client.notices.clear();
        assert_ok(&client.rpc("session.branch_end", end(&id, &child)).await);
        let stopped = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notice = client.notice("session.state").await;
                if notice["params"]["branchId"] == child.as_str()
                    && notice["params"]["running"] == false
                {
                    return;
                }
            }
        })
        .await;
        if stopped.is_err() {
            stale.push(round);
        }
        server.shutdown();
        server.join().await.unwrap();
    }
    assert!(stale.is_empty(), "rounds left the line running: {stale:?}");
}
