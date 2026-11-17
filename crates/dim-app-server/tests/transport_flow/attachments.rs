//! A re-attach replaces the subscription without a delivery gap, and a detach
//! settles the subscription it ends before its reply (#982, #1118).
use super::*;
use dim_agent_capabilities::session_broadcast::SessionNotice;
use std::collections::HashSet;

fn notice(data: &str) -> SessionNotice {
    SessionNotice {
        source: "test.attach".into(),
        kind: "changed".into(),
        data: json!(data),
    }
}

/// Creates a Session with one finished turn and leaves it idle.
async fn historical(client: &mut Client) -> String {
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let input = json!({"sessionId": sid, "branchId": "main", "text": "a historical turn"});
    assert_ok(&client.rpc("session.submit", input).await);
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": sid, "branchId": "main"}),
            )
            .await,
    );
    sid
}

async fn wait(signal: &tokio::sync::Notify, what: &str) {
    tokio::time::timeout(Duration::from_secs(20), signal.notified())
        .await
        .unwrap_or_else(|_| panic!("deadline waiting for {what}"));
}

/// The reply says the teardown happened: the `follow` task is held open, so
/// the detach cannot reply until the hold is released, and the Session's use
/// is released with it. `DIM_TEST_ATTACH_CYCLES` repeats the round for the
/// optional RSS measurement; the assertions hold for every round.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_detach_settles_its_subscription_before_the_reply() {
    let (server, _) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = historical(&mut client).await;
    let cycles = std::env::var("DIM_TEST_ATTACH_CYCLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    let baseline_uses = server.uses(&sid);
    for round in 0..cycles {
        let hold = server.hold_next_follow().expect("the ConnectionHub exists");
        let attached = client
            .rpc("session.attach", json!({"sessionId": sid}))
            .await;
        assert_ok(&attached);
        assert_eq!(server.uses(&sid), baseline_uses + 1, "round {round}");
        wait(&hold.waiting, "the follow task").await;
        let detach = client
            .send("session.detach", json!({"sessionId": sid}))
            .await;
        wait(&hold.cancelled, "the detach's retirement").await;
        let early = tokio::time::timeout(Duration::from_millis(100), client.response(detach)).await;
        assert!(
            early.is_err(),
            "round {round}: the detach reply must wait for the follow task",
        );
        hold.release.notify_one();
        let detached = client.response(detach).await;
        assert_ok(&detached);
        assert_eq!(server.uses(&sid), baseline_uses, "round {round}");
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// A re-attach that fails before it registers its listener keeps the previous
/// subscription: its use stays, and it still delivers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attach_failure_before_registration_keeps_the_previous_subscription() {
    let (server, _) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = historical(&mut client).await;
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": sid}))
            .await,
    );
    let attached_uses = server.uses(&sid);
    server.fail_next_attach_before_registration();
    let failed = client
        .rpc("session.attach", json!({"sessionId": sid}))
        .await;
    assert!(failed["error"].is_object(), "{failed}");
    assert_eq!(server.uses(&sid), attached_uses);
    server.notify(&sid, notice("after the failed re-attach"));
    assert_eq!(
        client.notice("session.notice").await["params"]["data"],
        "after the failed re-attach"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Re-attaching while a turn streams is continuous: the events committed
/// between the new pin and the new listener reach this connection. The
/// previous subscription is held open through its retirement, so the events
/// the turn commits when it is stopped fall in that window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_re_attach_while_a_turn_streams_loses_no_event() {
    let (server, _) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!(["wait"]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let hold = server.hold_next_follow().expect("the ConnectionHub exists");
    let attached = client
        .rpc("session.attach", json!({"sessionId": sid}))
        .await;
    assert_ok(&attached);
    wait(&hold.waiting, "the first follow task").await;
    let input = json!({
        "sessionId": sid, "branchId": "main", "text": "tool:wait {\"millis\":60000}"
    });
    assert_ok(&client.rpc("session.submit", input).await);
    let mut live = Vec::new();
    loop {
        let event = client.notice("session.event").await["params"]["event"].clone();
        let tool_started = event["type"] == "step" && event["name"] == "tool-use-start";
        live.push(event);
        if tool_started {
            break;
        }
    }
    let reattach = client
        .send("session.attach", json!({"sessionId": sid}))
        .await;
    wait(&hold.cancelled, "the re-attach's retirement").await;
    let stop = client
        .send(
            "session.stop",
            json!({"sessionId": sid, "branchId": "main"}),
        )
        .await;
    let stopped = client.response(stop).await;
    assert_ok(&stopped);
    let idle = client
        .rpc(
            "session.idle",
            json!({"sessionId": sid, "branchId": "main"}),
        )
        .await;
    assert_ok(&idle);
    let through = idle["result"]["sequence"].as_u64().unwrap();
    hold.release.notify_one();
    let reattached = client.response(reattach).await;
    assert_ok(&reattached);
    let new_pin = reattached["result"]["sequence"].as_u64().unwrap();
    assert!(
        new_pin < through,
        "the turn committed events after the new pin"
    );
    let history = client
        .rpc(
            "session.history",
            json!({
                "sessionId": sid, "branchId": "main", "after": new_pin, "limit": 4096,
                "through": through
            }),
        )
        .await;
    assert_ok(&history);
    let expected: Vec<u64> = history["result"]["events"]
        .as_array()
        .expect("a history page")
        .iter()
        .map(|event| event["sequence"].as_u64().unwrap())
        .collect();
    assert!(!expected.is_empty(), "{history}");
    let mut seen: HashSet<u64> = live
        .iter()
        .filter_map(|event| event["sequence"].as_u64())
        .filter(|sequence| *sequence > new_pin)
        .collect();
    while expected.iter().any(|sequence| !seen.contains(sequence)) {
        let event = client.notice("session.event").await["params"]["event"].clone();
        if let Some(sequence) = event["sequence"].as_u64() {
            seen.insert(sequence);
        }
    }
    server.shutdown();
    server.join().await.unwrap();
}
