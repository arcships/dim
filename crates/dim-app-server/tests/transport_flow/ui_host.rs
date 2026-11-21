//! The UI registry (Tui design §3.6): interfaces register the connection
//! they already have, any number per Session; a shutdown is a notification
//! on that connection; registrations leave with their connection's
//! attachment or the connection itself; the methods are local-only.
use super::*;
use rutis::host::CtxHost;

async fn attached_session(client: &mut Client) -> String {
    let created = client
        .rpc("session.create", json!({"cwd": std::env::temp_dir()}))
        .await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    session_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interfaces_register_any_number_per_session_and_leave_on_request() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut first = Client::new_version(&server, false, 3).await;
    for method in [
        "ui.open",
        "ui.close",
        "ui.attach",
        "ui.status",
        "ui.shutdown",
        "ui.replace",
    ] {
        assert!(
            first.features.as_array().unwrap().contains(&json!(method)),
            "{method}"
        );
    }
    assert!(
        first
            .features
            .as_array()
            .unwrap()
            .contains(&json!("ui.leaf.2"))
    );
    let session_id = attached_session(&mut first).await;
    let missing = first
        .rpc(
            "ui.open",
            json!({"sessionId": session_id, "kind": "tui", "peer": "ui-missing", "address": "unix:/nonexistent/leaf.sock"}),
        )
        .await;
    assert_eq!(
        missing["error"]["data"]["appError"], "ui_leaf_failed",
        "{missing}"
    );

    let unattached = first
        .rpc("ui.attach", json!({"sessionId":"missing","kind":"tui"}))
        .await;
    assert_eq!(
        unattached["error"]["data"]["appError"],
        "session_not_attached"
    );
    let attached = first
        .rpc("ui.attach", json!({"sessionId": session_id, "kind": "tui"}))
        .await;
    assert_ok(&attached);
    let ui_id = attached["result"]["uiId"].as_str().unwrap().to_owned();
    let again = first
        .rpc("ui.attach", json!({"sessionId": session_id, "kind": "tui"}))
        .await;
    assert_eq!(again["result"]["uiId"], ui_id);

    let mut second = Client::new_version(&server, false, 3).await;
    assert_ok(
        &second
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    let other = second
        .rpc(
            "ui.attach",
            json!({"sessionId": session_id, "kind": "electron"}),
        )
        .await;
    assert_ok(&other);
    let status = second.rpc("ui.status", json!({})).await;
    let instances = status["result"]["instances"].as_array().unwrap();
    assert_eq!(instances.len(), 2);
    assert!(
        instances
            .iter()
            .all(|i| i["sessionId"] == session_id && i["sinceMs"].is_u64())
    );
    assert_eq!(status["result"]["webConnections"], 0);

    assert_ok(
        &second
            .rpc(
                "ui.shutdown",
                json!({"uiId": ui_id, "reason": "another terminal took over"}),
            )
            .await,
    );
    let notice = first.notice("ui.shutdown").await;
    assert_eq!(notice["params"]["uiId"], ui_id);
    assert_eq!(notice["params"]["reason"], "another terminal took over");
    drop(first);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = second.rpc("ui.status", json!({})).await;
            if status["result"]["instances"].as_array().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the registration leaves with its connection");
    assert_eq!(
        second.rpc("ui.shutdown", json!({"uiId": ui_id})).await["error"]["data"]["appError"],
        "ui_unknown"
    );

    let mut third = Client::new_version(&server, false, 3).await;
    assert_ok(
        &third
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    let old = other["result"]["uiId"].as_str().unwrap();
    let replaced = third
        .rpc(
            "ui.replace",
            json!({"uiId": old, "sessionId": session_id, "kind": "tui"}),
        )
        .await;
    assert_ok(&replaced);
    assert_eq!(
        second.notice("ui.shutdown").await["params"]["reason"],
        "replaced"
    );
    let gone = third
        .rpc(
            "ui.replace",
            json!({"uiId": "no-such-ui", "sessionId": session_id, "kind": "tui"}),
        )
        .await;
    assert_eq!(gone["error"]["data"]["appError"], "ui_unknown");
    let status = third.rpc("ui.status", json!({})).await;
    assert!(
        status["result"]["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["uiId"] == replaced["result"]["uiId"])
    );

    let mut bare = Client::new_version(&server, true, 3).await;
    assert_eq!(
        bare.rpc("ui.status", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registration_leaves_when_its_session_is_detached_or_closed() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let first = attached_session(&mut client).await;
    let second = attached_session(&mut client).await;
    for session_id in [&first, &second] {
        assert_ok(
            &client
                .rpc("ui.attach", json!({"sessionId": session_id, "kind": "tui"}))
                .await,
        );
    }
    let registered = |status: &Value| -> Vec<String> {
        status["result"]["instances"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["sessionId"].as_str().unwrap().to_owned())
            .collect()
    };

    assert_ok(
        &client
            .rpc("session.detach", json!({"sessionId": first}))
            .await,
    );
    let status = client.rpc("ui.status", json!({})).await;
    assert_eq!(registered(&status), std::slice::from_ref(&second));

    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": second}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = client.rpc("ui.status", json!({})).await;
            if registered(&status).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the registration leaves with the closed Session");
}

/// A leaf as the interactive `dimcode` is one (Tui design §3.6): a rutis
/// kernel on a private socket that greets with its id and offers no plugin
/// catalog. Its `Tui` is a protocol v3 connection attaching with the token
/// `ui.open` replied.
struct FakeLeaf {
    peer: String,
    address: String,
    root: rutis::Ctx,
    session: tokio::sync::watch::Receiver<Option<rutis_bridge::SessionHandle>>,
    ended: tokio::sync::watch::Receiver<bool>,
    directory: std::path::PathBuf,
    tui: Option<Client>,
}

impl FakeLeaf {
    async fn start() -> Self {
        Self::greeting_as(None).await
    }

    /// A leaf whose handshake names `id` instead of its own id.
    async fn greeting_as(id: Option<&str>) -> Self {
        use rutis::host::FiberState;
        use rutis_bridge::{LocalId, PeerId, Session};
        let own = uuid::Uuid::new_v4().simple().to_string();
        let peer = format!("ui-{own}");
        let directory = std::env::temp_dir().join(format!("dim-leaf-{}", &own[..12]));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("leaf.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let root = rutis::Ctx::root().unwrap();
        root.provide(LocalId(PeerId::new(id.unwrap_or(&peer)).unwrap()))
            .unwrap();
        let (found, session) = tokio::sync::watch::channel(None);
        let (end, ended) = tokio::sync::watch::channel(false);
        let ctx = root.clone();
        tokio::spawn(async move {
            let stream = listener.accept().await.unwrap().0.into_std().unwrap();
            stream.set_nonblocking(false).unwrap();
            let reader = stream.try_clone().unwrap();
            let closer = Arc::new(UnixCloser(stream.try_clone().unwrap()));
            let channel = rutis_bridge::pipe::framed(reader, stream, closer);
            if let Ok(greeted) = Session::handshake(&ctx, channel).await {
                found.send_replace(Some(greeted.handle()));
                let mut state = ctx.plugin(greeted).watch();
                let _ = state
                    .wait_for(|s| matches!(s.state, FiberState::Disposed | FiberState::Failed))
                    .await;
            }
            end.send_replace(true);
        });
        Self {
            peer,
            address: format!("unix:{}", path.display()),
            root,
            session,
            ended,
            directory,
            tui: None,
        }
    }

    /// Its `Tui` attaches as `ui.open` replied.
    async fn attach(&mut self, server: &RunningServer, session_id: &str, opened: &Value) {
        let mut tui = Client::new_version(server, false, 3).await;
        assert_ok(
            &tui.rpc("session.attach", json!({"sessionId": session_id}))
                .await,
        );
        let attached = tui
            .rpc(
                "ui.attach",
                json!({"sessionId": session_id, "kind": "tui",
                    "uiId": opened["result"]["uiId"], "token": opened["result"]["token"]}),
            )
            .await;
        assert_ok(&attached);
        assert_eq!(attached["result"]["uiId"], self.peer.as_str());
        self.tui = Some(tui);
    }

    fn ended(&self) -> bool {
        *self.ended.borrow()
    }

    /// Waits until its session ended: the daemon closed it.
    async fn closed(&mut self, within: Duration) {
        tokio::time::timeout(within, self.ended.wait_for(|ended| *ended))
            .await
            .expect("the daemon closed the leaf")
            .unwrap();
    }

    /// The process ends: its session ends with it.
    async fn exit(self) {
        let _ = self.root.shutdown().await;
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct UnixCloser(std::os::unix::net::UnixStream);

impl rutis_bridge::channel::Closer for UnixCloser {
    fn close(&self, _: rutis_bridge::Close) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

async fn states(client: &mut Client) -> Vec<(String, String)> {
    let status = client.rpc("ui.status", json!({})).await;
    let mut states: Vec<(String, String)> = status["result"]["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["uiId"].as_str().unwrap().to_owned(),
                i["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    states.sort();
    states
}

fn state(leaf: &FakeLeaf, state: &str) -> (String, String) {
    (leaf.peer.clone(), state.to_owned())
}

async fn eventually(client: &mut Client, check: impl Fn(&[(String, String)]) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if check(&states(client).await) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the registry reached the expected state");
}

fn opening(session_id: &str, leaf: &FakeLeaf, extra: Value) -> Value {
    let mut params = json!({"sessionId": session_id, "kind": "tui",
        "peer": leaf.peer, "address": leaf.address});
    for (key, value) in extra.as_object().into_iter().flatten() {
        params[key] = value.clone();
    }
    params
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_opens_replaces_and_closes_leaves() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let session_id = attached_session(&mut client).await;
    let open = |leaf: &FakeLeaf, extra: Value| opening(&session_id, leaf, extra);

    let mut first = FakeLeaf::start().await;
    let opened = client.rpc("ui.open", open(&first, json!({}))).await;
    assert_ok(&opened);
    assert_eq!(opened["result"]["uiId"], first.peer.as_str());
    assert!(
        opened["result"]["token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "{opened}"
    );
    assert_eq!(states(&mut client).await, [state(&first, "starting")]);

    let mut other = Client::new_version(&server, false, 3).await;
    assert_ok(
        &other
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    let token = other
        .rpc(
            "ui.attach",
            json!({"sessionId": session_id, "kind": "tui", "uiId": first.peer, "token": "wrong"}),
        )
        .await;
    assert_eq!(token["error"]["data"]["appError"], "ui_token", "{token}");
    first.attach(&server, &session_id, &opened).await;
    assert_eq!(states(&mut client).await, [state(&first, "online")]);

    let handle = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let handle = first.session.borrow().clone().unwrap();
            if let Ok(row) = handle.run("UiHost", json!({})) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the daemon serves rows to the leaf");
    let refused = handle.loaded().await.unwrap_err().to_string();
    assert!(
        refused.contains("does not contain plugin UiHost"),
        "{refused}"
    );

    let generation_alone = client
        .rpc("ui.open", open(&first, json!({"expectedGeneration": 1})))
        .await;
    assert_eq!(
        generation_alone["error"]["code"], -32602,
        "{generation_alone}"
    );

    let generation = opened["result"]["generation"].clone();
    let (a, b) = (FakeLeaf::start().await, FakeLeaf::start().await);
    let mut client_b = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client_b
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    let replace = json!({"replace": first.peer, "expectedGeneration": generation});
    let (ra, rb) = tokio::join!(
        other.rpc("ui.open", open(&a, replace.clone())),
        client_b.rpc("ui.open", open(&b, replace.clone())),
    );
    let (mut winner, winning, loser, losing) = if ra.get("error").is_none() {
        (a, ra, b, rb)
    } else {
        (b, rb, a, ra)
    };
    assert_eq!(
        losing["error"]["data"]["appError"], "ui_generation",
        "{losing}"
    );
    assert_ok(&winning);
    let mut expected = vec![state(&first, "online"), state(&winner, "starting")];
    expected.sort();
    assert_eq!(states(&mut client).await, expected);
    assert!(
        !first.ended(),
        "the old interface closed before the new one attached"
    );
    winner.attach(&server, &session_id, &winning).await;
    first.closed(Duration::from_secs(10)).await;
    assert_eq!(
        first.tui.as_mut().unwrap().notice("ui.shutdown").await["params"]["reason"],
        "replaced"
    );
    assert_eq!(states(&mut client).await, [state(&winner, "online")]);

    let failed = client
        .rpc(
            "ui.open",
            json!({"sessionId": session_id, "kind": "tui", "peer": "ui-gone",
                "address": "unix:/nonexistent/leaf.sock", "replace": winner.peer}),
        )
        .await;
    assert_eq!(
        failed["error"]["data"]["appError"], "ui_leaf_failed",
        "{failed}"
    );
    let ending = FakeLeaf::start().await;
    assert_ok(
        &client
            .rpc("ui.open", open(&ending, json!({"replace": winner.peer})))
            .await,
    );
    ending.exit().await;
    eventually(&mut client, |s| s == [state(&winner, "online")]).await;
    assert!(!winner.ended());

    let mut third = FakeLeaf::start().await;
    let replaced = client
        .rpc("ui.open", open(&third, json!({"replace": winner.peer})))
        .await;
    assert_ok(&replaced);
    third.attach(&server, &session_id, &replaced).await;
    winner.closed(Duration::from_secs(10)).await;
    assert_eq!(states(&mut client).await, [state(&third, "online")]);
    third.exit().await;
    eventually(&mut client, |s| s.is_empty()).await;

    let mut fourth = FakeLeaf::start().await;
    let opened = client.rpc("ui.open", open(&fourth, json!({}))).await;
    assert_ok(&opened);
    fourth.attach(&server, &session_id, &opened).await;
    assert_ok(&client.rpc("ui.close", json!({"uiId": fourth.peer})).await);
    fourth.closed(Duration::from_secs(10)).await;
    assert!(states(&mut client).await.is_empty());

    let impostor = FakeLeaf::greeting_as(Some("ui-someone-else")).await;
    let refused = client.rpc("ui.open", open(&impostor, json!({}))).await;
    assert_eq!(
        refused["error"]["data"]["appError"], "ui_leaf_failed",
        "{refused}"
    );
    assert!(states(&mut client).await.is_empty());

    for leaf in [first, winner, loser, fourth, impostor] {
        leaf.exit().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leaf_that_never_attaches_is_closed_after_its_start_bound() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let session_id = attached_session(&mut client).await;
    let open = |leaf: &FakeLeaf, extra: Value| opening(&session_id, leaf, extra);

    let mut old = FakeLeaf::start().await;
    let opened = client.rpc("ui.open", open(&old, json!({}))).await;
    assert_ok(&opened);
    old.attach(&server, &session_id, &opened).await;
    let generation = opened["result"]["generation"].clone();

    let mut silent = FakeLeaf::start().await;
    let replace = json!({"replace": old.peer, "expectedGeneration": generation});
    assert_ok(&client.rpc("ui.open", open(&silent, replace.clone())).await);
    let mut expected = vec![state(&old, "online"), state(&silent, "starting")];
    expected.sort();
    assert_eq!(states(&mut client).await, expected);
    silent.closed(Duration::from_secs(30)).await;
    assert_eq!(states(&mut client).await, [state(&old, "online")]);
    assert!(!old.ended());

    let mut next = FakeLeaf::start().await;
    let replaced = client.rpc("ui.open", open(&next, replace)).await;
    assert_ok(&replaced);
    next.attach(&server, &session_id, &replaced).await;
    old.closed(Duration::from_secs(10)).await;

    for leaf in [old, silent, next] {
        leaf.exit().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_ui_operations_use_the_connected_client_and_extension_catalog() {
    let mut parts = super::session_typescript::fixture();
    parts.rows = vec![json!({
        "id":"ui-owner", "name":parts.anchor.parent().unwrap().join("fixtures/ui-owner.ts"),
        "provides":{}
    })];
    let home = std::env::temp_dir().join(format!("dm-ui-{}", uuid::Uuid::new_v4()));
    let (server, _, _) = super::session_typescript::start(ServerConfig {
        target_home: Some(home.join("rust")),
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let source = home.join("ui-fixture");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("dim-plugin.json"), r#"{"name":"ui-fixture"}"#).unwrap();
    std::fs::write(source.join("ui/panel.js"), "export default 1").unwrap();
    let installed = client
        .rpc("plugin.install", json!({"source":{"path":source}}))
        .await;
    assert_ok(&installed);
    assert_eq!(installed["result"]["id"], "ui-fixture");
    let session = attached_session(&mut client).await;
    for action in ["detach", "release"] {
        assert_ok(
            &client
                .rpc("ui.attach", json!({"sessionId":session,"kind":"tui"}))
                .await,
        );
        let result = super::process_typescript::files_available(
            &mut client,
            &format!("uifixture.{action}"),
            json!({"sessionId":session}),
        )
        .await;
        assert_eq!(result, true);
        let status = client.rpc("ui.status", json!({})).await;
        assert_eq!(status["result"]["instances"], json!([]));
        let catalog = client.rpc("ui.extensions.list", json!({})).await;
        assert_ok(&catalog);
        let refused = catalog["result"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error["pluginName"] == "ui-fixture");
        assert_eq!(refused, action == "detach");
    }
    let mut leaf = FakeLeaf::start().await;
    let opened = client
        .rpc("ui.open", opening(&session, &leaf, json!({})))
        .await;
    assert_ok(&opened);
    leaf.attach(&server, &session, &opened).await;
    assert_ok(&client.rpc("ui.close", json!({"uiId": leaf.peer})).await);
    leaf.closed(Duration::from_secs(10)).await;
    let finished = leaf
        .tui
        .as_mut()
        .unwrap()
        .rpc("uifixture.finished", json!({"sessionId": session}))
        .await;
    assert_ok(&finished);
    assert_eq!(finished["result"], true);
    leaf.exit().await;
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}
