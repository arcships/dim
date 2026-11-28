use super::*;
use dim_agent_capabilities::session_broadcast::{
    SessionBroadcast, SessionBroadcastEvent, session_channel,
};
use dim_agent_runtime::SessionLines;
use dim_agent_runtime::SessionScope;
use rutis::host::CtxHost;
use std::collections::HashMap;
use tokio::sync::Notify;

/// Deliberately delays the bus, not Dispatcher execution. This models the
/// asynchronous gap that a delivery fence must cross before replying. A later
/// record of one name reaches its listeners only once every listener of the
/// earlier record has started, so the gate holds the listener's synchronous
/// part; an awaiting listener would hold nothing.
#[derive(Default)]
struct Gate {
    entered: Notify,
    open: std::sync::Mutex<bool>,
    opened: std::sync::Condvar,
    blocked: std::sync::atomic::AtomicBool,
}
impl Gate {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}
struct Delay(Arc<Gate>);
impl Delay {
    /// The listener's synchronous part.
    fn hold(&self, record: &SessionBroadcastEvent) {
        let block = matches!(record, SessionBroadcastEvent::Event { event, .. }
            if matches!(event.kind, dim_agent_types::EventKind::Text(_)))
            && !self.0.blocked.swap(true, Ordering::SeqCst);
        if block {
            self.0.entered.notify_one();
            tokio::task::block_in_place(|| {
                let open = self.0.open.lock().unwrap();
                drop(self.0.opened.wait_while(open, |open| !*open).unwrap());
            });
        }
    }
}
struct DelayPlugin {
    gate: Arc<Gate>,

    identity: rutis::host::Key<SessionScope>,
}
impl DelayPlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, gate: Arc<Gate>) -> rutis::Result<Self> {
        Ok(Self {
            gate,
            identity: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for DelayPlugin {
    fn name(&self) -> &str {
        "test.delay-session-broadcast"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let delay = Delay(self.gate.clone());
            let listener = ctx
                .listen::<SessionBroadcast>()
                .name(session_channel(&identity.session_id()))
                .on(move |event, _| {
                    delay.hold(&event.record);
                    async { Ok(None) }
                })?;
            ctx.effect(move || async move { Ok(listener.dispose().await?) })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

async fn setup(ws: bool) -> (RunningServer, Client, String, Arc<Gate>) {
    let gate = Arc::new(Gate::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    let plugin_gate = gate.clone();
    builder.require_feature(
        Feature::new("test.delay-session-broadcast")
            .session("test.delay-session-broadcast", move |names| {
                Ok(Box::new(DelayPlugin::new(names, plugin_gate.clone())?))
            }),
    );
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-barrier-{}/server.sock", uuid::Uuid::new_v4())),
            ),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, ws, 2).await;
    let created = client
        .rpc("session.create", json!({"cwd":std::env::temp_dir()}))
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({
                    "sessionId":sid,"branchId":"main","text":"first output"
                }),
            )
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    server
        .agent()
        .unwrap()
        .get_session(&sid)
        .unwrap()
        .idle("main")
        .await
        .unwrap();
    (server, client, sid, gate)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_does_not_reply_ahead_of_delayed_broadcasts() {
    for ws in [false, true] {
        for method in ["session.idle", "session.stop"] {
            let (server, mut client, sid, gate) = setup(ws).await;
            let request = client
                .send(method, json!({"sessionId":sid,"branchId":"main"}))
                .await;
            let early =
                tokio::time::timeout(Duration::from_millis(100), client.response(request)).await;
            gate.release();
            assert!(
                early.is_err(),
                "execution reply overtook the blocked text: {early:?}"
            );
            let reply = client.response(request).await;
            assert_ok(&reply);
            let cutoff = reply["result"]["sequence"].as_u64().unwrap();
            assert!(
                client.notices.iter().any(|n| {
                    n["method"] == "session.event"
                        && n["params"]["event"]["name"] == "text"
                        && n["params"]["event"]["sequence"].as_u64().unwrap() <= cutoff
                }),
                "text must precede the reply on the wire"
            );
            server.shutdown();
            server.join().await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_a_subscription_invalidates_its_pending_delivery() {
    for ws in [false, true] {
        for replacement in ["session.detach", "session.attach"] {
            let (server, mut client, sid, gate) = setup(ws).await;
            let request = client
                .send("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                .await;
            let early =
                tokio::time::timeout(Duration::from_millis(100), client.response(request)).await;
            if early.is_ok() {
                gate.release();
            }
            assert!(early.is_err(), "idle must still await delivery");
            let replace = client.send(replacement, json!({"sessionId":sid})).await;
            let mut replies = HashMap::new();
            while replies.len() < 2 {
                let value = client.read("idle and owner replacement responses").await;
                if let Some(id) = value["id"].as_u64() {
                    replies.insert(id, value);
                }
            }
            gate.release();
            assert_ok(&replies[&replace]);
            assert_eq!(
                replies[&request]["error"]["data"]["appError"],
                "subscription_changed"
            );
            server.shutdown();
            server.join().await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_reload_releases_delivery_waits_without_releasing_the_broadcast_gate() {
    for method in ["session.idle", "session.stop"] {
        let (server, mut client, sid, gate) = setup(false).await;
        let request = client
            .send(method, json!({"sessionId":sid,"branchId":"main"}))
            .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.response(request))
                .await
                .is_err()
        );
        let reload = tokio::time::timeout(Duration::from_secs(5), server.restart("Rpc")).await;
        gate.release();
        reload
            .expect("delivery wait must not hold RPC unload")
            .unwrap();
        let reply = client.response(request).await;
        assert_eq!(reply["error"]["data"]["appError"], "service_reloading");
        assert_ok(&client.rpc("daemon.ping", json!({})).await);
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_cutoff_stays_fixed_while_new_input_advances_the_timeline() {
    let (server, mut client, sid, gate) = setup(false).await;
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let first = session.sequence("main").unwrap();
    let request = client
        .send("session.idle", json!({"sessionId":sid,"branchId":"main"}))
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), client.response(request))
            .await
            .is_err()
    );
    session
        .submit(
            "second output".into(),
            dim_agent_types::InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let second = session.sequence("main").unwrap();
    assert!(second > first);
    gate.release();
    let reply = client.response(request).await;
    assert_ok(&reply);
    assert_eq!(reply["result"]["sequence"], first);
    server.shutdown();
    server.join().await.unwrap();
}

/// Fence allocation and emission share the `issued` lock, so concurrent fences reach the listener
/// in token order and a single monotonic `reached` watch suffices.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivery_is_subscription_local_and_concurrent_waiters_complete() {
    let (server, mut client, sid, gate) = setup(false).await;
    let mut requests = Vec::new();
    for _ in 0..3 {
        requests.push(
            client
                .send("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                .await,
        );
    }
    let mut observer = Client::new_version(&server, false, 2).await;
    assert_ok(
        &observer
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), client.response(requests[0]))
            .await
            .is_err()
    );
    gate.release();
    let cutoff = server
        .agent()
        .unwrap()
        .get_session(&sid)
        .unwrap()
        .sequence("main")
        .unwrap();
    for request in requests {
        let reply = client.response(request).await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["sequence"], cutoff);
    }
    server.shutdown();
    server.join().await.unwrap();
}
