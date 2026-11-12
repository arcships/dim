//! A Session recorded at another protocol version: a newer connection opens
//! it, deletes it and lists it, and an older one is refused (#1459).
use super::deletion::Store;
use super::*;

/// A Session created by a v3 connection, so its recorded version is 3 while
/// the server speaks 4.
async fn v3_session(client: &mut Client) -> String {
    let created = client.rpc("session.create", create(json!(["wait"]))).await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

/// The events of the page a client reads for `session` up to `through`, as
/// the wire carries them, so two connections' projections of one history can
/// be compared.
async fn page(client: &mut Client, session: &str, through: u64) -> Vec<Value> {
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":"main","after":0,"through":through,"limit":256}),
        )
        .await;
    assert_ok(&history);
    let mut events = history["result"]["events"].as_array().unwrap().clone();
    events.retain(|event| event["sequence"].as_u64().unwrap() <= through);
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_connection_opens_an_older_session_as_the_older_one_does() {
    let store = Store::new();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut v3 = Client::new_version(&server, false, 3).await;
    let session = v3_session(&mut v3).await;
    v3.rpc(
        "session.submit",
        json!({"sessionId":session,"branchId":"main","text":"from v3"}),
    )
    .await;
    let through = v3
        .rpc(
            "session.idle",
            json!({"sessionId":session,"branchId":"main"}),
        )
        .await["result"]["sequence"]
        .as_u64()
        .unwrap();
    // Both clients read the same window, cut before the v4 attach appends its
    // own client-state input, so only the version differs between the reads.
    let before = page(&mut v3, &session, through).await;
    let mut v4 = Client::new_version(&server, false, 4).await;
    let attached = v4.rpc("session.attach", json!({"sessionId":session})).await;
    assert_ok(&attached);
    assert_eq!(attached["result"]["sessionId"], json!(session));
    // The two projections of one v3 history are the same bytes: v4 only adds
    // an optional field, and a v3 Session has no parented events.
    let after_v4 = page(&mut v4, &session, through).await;
    assert_eq!(
        after_v4, before,
        "the v4 projection differs from the v3 one"
    );
    assert!(
        !after_v4.is_empty(),
        "the page must carry the Session's events"
    );
    drop((v3, v4));
    server.shutdown();
    server.join().await.unwrap();
}

/// A delete reads none of the Session's records, so it works in both
/// directions across the version bound, and a Session whose recorded version
/// is unreadable is removable too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_delete_removes_a_session_of_any_recorded_version() {
    let store = Store::new();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    // The v3 connection creates one Session; the v4 connection creates the
    // other, so deleting each from the other's connection crosses the bound.
    let mut v3 = Client::new_version(&server, false, 3).await;
    let older = v3_session(&mut v3).await;
    let mut v4 = Client::new_version(&server, false, 4).await;
    let newer = v3_session(&mut v4).await;
    for (client, session) in [(&mut v4, &older), (&mut v3, &newer)] {
        let deleted = client
            .rpc("session.delete", json!({"sessionId":session}))
            .await;
        assert_ok(&deleted);
        assert_eq!(deleted["result"], json!({"deleted": true}), "{deleted}");
        let again = client
            .rpc("session.delete", json!({"sessionId":session}))
            .await;
        assert_ok(&again);
        assert_eq!(again["result"], json!({"deleted": false}), "{again}");
        assert!(store.rows(session).is_empty(), "the rows are gone");
    }
    drop((v3, v4));
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_connection_lists_an_older_session_and_an_older_one_does_not() {
    let store = Store::new();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut v3 = Client::new_version(&server, false, 3).await;
    let older = v3_session(&mut v3).await;
    let mut v4 = Client::new_version(&server, false, 4).await;
    let current = v3_session(&mut v4).await;
    for (method, params) in [
        ("session.catalog", json!({"withAcpSessions":true})),
        ("session.summaries", json!({})),
    ] {
        let key = if method == "session.catalog" {
            "sessions"
        } else {
            ""
        };
        let listed = v4.rpc(method, params.clone()).await;
        assert_ok(&listed);
        let rows = match listed["result"].as_array() {
            Some(rows) => rows.clone(),
            None => listed["result"][key].as_array().unwrap().clone(),
        };
        let ids: Vec<&str> = rows
            .iter()
            .map(|row| row["sessionId"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&older.as_str()), "{method}: {listed}");
        assert!(ids.contains(&current.as_str()), "{method}: {listed}");
        // The older connection speaks exactly the older Session's version, so
        // it lists that one and not the newer Session.
        let listed = v3.rpc(method, params).await;
        assert!(listed.get("error").is_none(), "{method} from v3: {listed}");
        let rows = match listed["result"].as_array() {
            Some(rows) => rows.clone(),
            None => listed["result"][key].as_array().unwrap().clone(),
        };
        let ids: Vec<&str> = rows
            .iter()
            .map(|row| row["sessionId"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&older.as_str()), "{method}: {listed}");
        assert!(!ids.contains(&current.as_str()), "{method}: {listed}");
    }
    drop((v3, v4));
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_of_an_older_session_keeps_its_version() {
    let store = Store::new();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut v3 = Client::new_version(&server, false, 3).await;
    let source = v3_session(&mut v3).await;
    let mut v4 = Client::new_version(&server, false, 4).await;
    let forked = v4
        .rpc("session.fork", json!({"sessionId":source,"at":0}))
        .await;
    assert_ok(&forked);
    let fork = forked["result"]["sessionId"].as_str().unwrap().to_owned();
    // The fork records the source's version, so the v3 connection opens it and
    // a hypothetical v2 connection would not.
    let mut v3_again = Client::new_version(&server, false, 3).await;
    assert_ok(
        &v3_again
            .rpc("session.attach", json!({"sessionId":fork}))
            .await,
    );
    let mut v2 = Client::new_version(&server, false, 2).await;
    assert_eq!(
        v2.rpc("session.attach", json!({"sessionId":fork})).await["error"]["data"]["appError"],
        "session_protocol_version"
    );
    drop((v3, v4, v3_again, v2));
    server.shutdown();
    server.join().await.unwrap();
}

/// A Session recorded at a version the store still admits below v3 opens from
/// a v4 connection, including after the daemon reloads it from storage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_connection_opens_a_session_recorded_below_v3() {
    let store = Store::new();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut v2 = Client::new_version(&server, false, 2).await;
    let created = v2.rpc("session.create", create(json!(["wait"]))).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let submitted = v2
        .rpc(
            "session.submit",
            json!({"sessionId":session,"branchId":"main","text":"from v2"}),
        )
        .await;
    assert_ok(&submitted);
    let idle = v2
        .rpc(
            "session.idle",
            json!({"sessionId":session,"branchId":"main"}),
        )
        .await;
    assert_ok(&idle);
    let mut v4 = Client::new_version(&server, false, 4).await;
    let attached = v4.rpc("session.attach", json!({"sessionId":session})).await;
    assert_ok(&attached);
    assert_eq!(attached["result"]["sessionId"], json!(session));
    let page = v4
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":"main","after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    assert!(
        !page["result"]["events"].as_array().unwrap().is_empty(),
        "the page carries the v2 Session's events: {page}"
    );
    drop((v2, v4));
    // A restart makes the daemon reload the Session from the store, so the
    // restore path reads the recorded version too.
    server.shutdown();
    server.join().await.unwrap();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut v4 = Client::new_version(&server, false, 4).await;
    let restored = v4.rpc("session.attach", json!({"sessionId":session})).await;
    assert_ok(&restored);
    assert_eq!(restored["result"]["sessionId"], json!(session));
    drop(v4);
    server.shutdown();
    server.join().await.unwrap();
}
