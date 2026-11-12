use super::*;

struct ProcessOwner {
    activity: rutis::host::Key<dim_app_server::ProcessActivity>,
    captured: Arc<std::sync::OnceLock<Arc<dim_app_server::ProcessActivity>>>,
}

impl rutis::host::DynPlugin for ProcessOwner {
    fn name(&self) -> &str {
        "ProcessOwner"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.activity.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let activity = ctx.require_at(&self.activity)?;
            let _ = self.captured.set(activity);
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_process_work_refuses_idle_shutdown_before_any_session_exists() {
    let captured = Arc::new(std::sync::OnceLock::new());
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), {
        let captured = captured.clone();
        move |builder| {
            builder.plugin(move |names| {
                Ok(Box::new(ProcessOwner {
                    activity: names.key()?,
                    captured: captured.clone(),
                }) as Box<dyn rutis::host::DynPlugin>)
            });
        }
    })
    .await;
    let activity = captured.get().unwrap();
    let first = activity.hold().unwrap();
    let second = activity.take_hold().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        client.rpc("daemon.ping", json!({})).await["result"]["sessions"],
        0
    );
    for release_first in [false, true] {
        if release_first {
            assert!(activity.release(first));
        }
        let refused = client.rpc("daemon.shutdown", json!({"ifIdle":true})).await;
        assert_eq!(
            refused["error"]["data"]["appError"], "daemon_busy",
            "{refused}"
        );
        assert_ok(&client.rpc("daemon.ping", json!({})).await);
    }
    drop(second);
    assert_ok(&client.rpc("session.create", create(json!([]))).await);
    assert_ok(&client.rpc("daemon.shutdown", json!({"ifIdle":true})).await);
    assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
    assert!(
        activity.take_hold().is_none(),
        "completed shutdown stays closed"
    );
}

/// The daemon an idle shutdown would disturb is counted and kept: another
/// connection, a Session another connection attached, and a Session the
/// requesting connection attached itself all refuse it, with the counts that
/// refused it, and the daemon keeps serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_shutdown_is_refused_while_another_client_uses_the_daemon() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let path = server.unix_path.clone().unwrap();
    let mut requester = Client::new_version(&server, false, 3).await;
    assert!(
        requester
            .features
            .as_array()
            .unwrap()
            .contains(&json!("daemon.shutdown.ifIdle")),
        "{}",
        requester.features
    );
    let mut other = Client::new_version(&server, false, 3).await;
    let refused = requester
        .rpc("daemon.shutdown", json!({"ifIdle":true}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "daemon_busy",
        "{refused}"
    );
    assert_eq!(refused["error"]["data"]["connections"], 1, "{refused}");
    assert_eq!(refused["error"]["data"]["attachedSessions"], 0, "{refused}");
    assert_eq!(refused["error"]["data"]["running"], 0, "{refused}");
    assert_ok(&other.rpc("daemon.ping", json!({})).await);

    let created = other.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&other.rpc("session.attach", json!({"sessionId":sid})).await);
    let refused = requester
        .rpc("daemon.shutdown", json!({"ifIdle":true}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "daemon_busy",
        "{refused}"
    );
    assert_eq!(refused["error"]["data"]["attachedSessions"], 1, "{refused}");

    assert_ok(&other.rpc("session.detach", json!({"sessionId":sid})).await);
    drop(other);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ping = requester.rpc("daemon.ping", json!({})).await;
        if ping["result"]["connections"] == json!(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the other connection never left: {ping}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_ok(
        &requester
            .rpc("session.attach", json!({"sessionId":sid}))
            .await,
    );
    let refused = requester
        .rpc("daemon.shutdown", json!({"ifIdle":true}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "daemon_busy",
        "{refused}"
    );
    assert_eq!(refused["error"]["data"]["connections"], 0, "{refused}");
    assert_eq!(refused["error"]["data"]["attachedSessions"], 1, "{refused}");

    assert_ok(
        &requester
            .rpc("session.detach", json!({"sessionId":sid}))
            .await,
    );
    assert_ok(&requester.rpc("daemon.shutdown", json!({})).await);
    assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A turn running in a Session whose client left still counts as use: the
/// daemon is kept, and the running turn is not interrupted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_shutdown_is_refused_while_a_turn_runs() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut owner = Client::new_version(&server, false, 3).await;
    let created = owner.rpc("session.create", create(json!(["wait"]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&owner.rpc("session.attach", json!({"sessionId":sid})).await);
    assert_ok(
        &owner
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":"tool:wait {\"millis\":3000,\"text\":\"done\"}"}),
            )
            .await,
    );
    drop(owner);

    let mut requester = Client::new_version(&server, false, 3).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed = requester.rpc("session.summaries", json!({})).await;
        if listed["result"][0]["running"] == json!(true) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the turn never started: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let refused = requester
        .rpc("daemon.shutdown", json!({"ifIdle":true}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "daemon_busy",
        "{refused}"
    );
    assert_eq!(refused["error"]["data"]["running"], 1, "{refused}");
    assert_eq!(refused["error"]["data"]["connections"], 0, "{refused}");
    assert_eq!(refused["error"]["data"]["attachedSessions"], 0, "{refused}");

    server.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(10), server.join())
        .await
        .expect("a refused idle shutdown does not prevent cleanup");
}

/// With nothing else attached or running, the idle variant stops the daemon
/// exactly as the plain call does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_shutdown_stops_an_idle_daemon() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let path = server.unix_path.clone().unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.create", create(json!([]))).await);
    assert_ok(&client.rpc("daemon.shutdown", json!({"ifIdle":true})).await);
    assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Without `ifIdle`, a shutdown request still proceeds whatever other
/// clients are connected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_shutdown_ignores_other_connections() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut requester = Client::new_version(&server, false, 3).await;
    let mut other = Client::new_version(&server, false, 3).await;
    assert_ok(&other.rpc("daemon.ping", json!({})).await);
    assert_ok(&requester.rpc("daemon.shutdown", json!({})).await);
    assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
}
