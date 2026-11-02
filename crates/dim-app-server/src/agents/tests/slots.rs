//! Admission slots (#345): Agents keeps a Session's slot while something
//! holds it, while the Session is loaded, and while it records the close of
//! a Session whose rows remain (S5c D5); every other slot goes. Clients do
//! not see slots, so these flows drive Sessions through Rpc and count the
//! index's map here instead of through a test API. The grace period is
//! longer than any test: each idle unload pass runs when the test calls it,
//! so every count is exact.
use super::{AgentIndex, builder, config};
use crate::{RunningServer, ServerConfig, agents::SessionSlot};
use dim_agent_runtime::{AgentBuilder, Feature, Names, SessionIdentity};
use dim_agent_types::EventKind;
use futures_util::{SinkExt, StreamExt};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio::{net::TcpStream, sync::Notify};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

const DEADLINE: Duration = Duration::from_secs(20);
/// The idle unload pass's marks, kept by the test between passes.
type Marks = HashMap<String, (Arc<SessionSlot>, u64)>;

/// A disposable native store.
struct Store(PathBuf);
impl Store {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("dim-slots-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
    async fn serve(&self, builder: AgentBuilder) -> RunningServer {
        crate::serve(
            builder,
            ServerConfig {
                idle_unload_after: Duration::from_secs(3600),
                ..config(&self.0)
            },
        )
        .await
        .unwrap()
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A protocol v3 client over the WebSocket transport.
struct Client(WebSocketStream<MaybeTlsStream<TcpStream>>, u64);
impl Client {
    async fn new(server: &RunningServer) -> Self {
        let address = server.websocket_address.unwrap();
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        let mut client = Self(socket, 0);
        let hello = client
            .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await;
        assert_eq!(hello["result"]["protocolVersion"], 3, "{hello}");
        client
    }
    /// The reply to one call; notices in between are skipped.
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.1 += 1;
        let request = json!({"jsonrpc":"2.0","id":self.1,"method":method,"params":params});
        self.0
            .send(Message::Text(request.to_string().into()))
            .await
            .unwrap();
        loop {
            let message = tokio::time::timeout(DEADLINE, self.0.next())
                .await
                .unwrap_or_else(|_| panic!("deadline: reply to {method}"))
                .unwrap()
                .unwrap();
            if let Message::Text(text) = message {
                let reply: Value = serde_json::from_str(&text).unwrap();
                if reply["id"] == self.1 {
                    return reply;
                }
            }
        }
    }
    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let reply = self.rpc(method, params).await;
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        reply["result"].clone()
    }
    async fn create(&mut self, store: &Store) -> String {
        let created = self
            .ok("session.create", json!({"cwd":store.0,"tools":[]}))
            .await;
        created["sessionId"].as_str().unwrap().to_owned()
    }
    /// The ids `method` (`session.list` or `session.summaries`) lists.
    async fn listed(&mut self, method: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .ok(method, json!({}))
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|info| info["sessionId"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    }
    async fn app_error(&mut self, method: &str, id: &str) -> Value {
        self.rpc(method, json!({"sessionId":id})).await["error"]["data"]["appError"].clone()
    }
}

/// The slots the index keeps, and how many of them are closed.
fn slots(index: &AgentIndex) -> (usize, usize) {
    let inner = index.directory.inner.lock().unwrap();
    let closed = inner
        .sessions
        .values()
        .filter(|slot| slot.closed.load(Ordering::Acquire))
        .count();
    (inner.sessions.len(), closed)
}
/// The leases on Session `id`, as the pass reads them from its slot.
fn uses(index: &AgentIndex, id: &str) -> Option<usize> {
    let inner = index.directory.inner.lock().unwrap();
    inner
        .sessions
        .get(id)
        .map(|slot| slot.uses.load(Ordering::Acquire))
}
/// Idle unload passes until `loaded` Sessions remain loaded. A Session that
/// was just loaded can refuse once while its resume is dispatched.
async fn settle(index: &AgentIndex, marks: &mut Marks, loaded: usize) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            index.unload_idle(marks).await;
            if index.session_count() == loaded {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("deadline: {loaded} Sessions loaded"));
}

/// The acceptance flow of #345: many Sessions created, listed, unloaded,
/// closed and deleted. At every step the slots are those of the loaded
/// Sessions, of the closed Sessions whose rows remain, and of what a caller
/// still holds; listing creates none. The closes outlive every pass, and a
/// Session whose slot went loads again on its next use.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slots_stay_bounded_by_loaded_sessions_and_kept_closes() {
    const N: usize = 12;
    let store = Store::new();
    let server = store.serve(builder()).await;
    let index = server.root.get::<AgentIndex>().unwrap();
    let mut client = Client::new(&server).await;
    let mut marks = Marks::new();
    let mut ids = vec![];
    for _ in 0..N {
        ids.push(client.create(&store).await);
    }
    ids.sort();
    assert_eq!((index.session_count(), slots(&index)), (N, (N, 0)));
    assert_eq!(client.listed("session.list").await, ids);

    settle(&index, &mut marks, 0).await;
    assert_eq!(slots(&index), (0, 0));
    assert_eq!(client.listed("session.list").await, ids);
    assert_eq!(client.listed("session.summaries").await, ids);
    assert_eq!(slots(&index), (0, 0));

    let (closed, rest) = ids.split_at(N / 3);
    let (deleted, unloaded) = rest.split_at(N / 3);
    for id in closed {
        client.ok("session.close", json!({"sessionId":id})).await;
    }
    assert_eq!((index.session_count(), slots(&index)), (0, (N / 3, N / 3)));

    let lease = index.acquire(&deleted[0]).await.unwrap();
    for id in deleted {
        client.ok("session.delete", json!({"sessionId":id})).await;
    }
    assert_eq!(slots(&index), (N / 3 + 1, N / 3 + 1));
    assert_eq!(
        client.app_error("session.attach", &deleted[0]).await,
        "session_closed"
    );
    drop(lease);
    index.unload_idle(&mut marks).await;
    assert_eq!(slots(&index), (N / 3, N / 3));

    assert_eq!(client.listed("session.list").await, unloaded);
    for id in closed {
        assert_eq!(
            client.app_error("session.attach", id).await,
            "session_closed"
        );
    }
    assert_eq!(index.session_count(), 0);

    let attach = json!({"sessionId":unloaded[0]});
    client.ok("session.attach", attach.clone()).await;
    assert_eq!(uses(&index, &unloaded[0]), Some(1));
    index.unload_idle(&mut marks).await;
    index.unload_idle(&mut marks).await;
    assert_eq!(
        (index.session_count(), slots(&index)),
        (1, (N / 3 + 1, N / 3))
    );
    client.ok("session.detach", attach).await;
    settle(&index, &mut marks, 0).await;
    assert_eq!(slots(&index), (N / 3, N / 3));

    for id in closed {
        client.ok("session.delete", json!({"sessionId":id})).await;
    }
    assert_eq!(slots(&index), (0, 0));
    assert_eq!(client.listed("session.list").await, unloaded);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Idle unload §6.1 proves "no lease since the mark" by an unchanged acquire
/// count, which only holds while the marked slot stays the same: a slot
/// made anew restarts its count. A Session released outside the pass after
/// its mark, as a fault or another caller of `unload_if_idle` releases it,
/// is not loaded, yet its mark keeps the slot through a prune. Restored
/// within the grace, it is not unloaded by the next pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mark_keeps_the_slot_of_a_session_released_outside_the_pass() {
    let store = Store::new();
    let server = store.serve(builder()).await;
    let index = server.root.get::<AgentIndex>().unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new(&server).await;
    let id = client.create(&store).await;
    let mut marks = Marks::new();
    drop(index.acquire(&id).await.unwrap());
    index.unload_idle(&mut marks).await;
    assert!(
        agent
            .get_session(&id)
            .unwrap()
            .unload_if_idle()
            .await
            .unwrap()
    );
    assert!(agent.get_session(&id).is_none());

    index.directory.inner.lock().unwrap().sweep();
    assert_eq!(slots(&index), (1, 0));
    drop(index.acquire(&id).await.unwrap());
    index.unload_idle(&mut marks).await;
    assert!(
        agent.get_session(&id).is_some(),
        "a Session restored since its mark is not unloaded"
    );
    settle(&index, &mut marks, 0).await;
    assert_eq!(slots(&index), (0, 0));
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A pass while a restore holds a Session's gate: the Session is not loaded
/// yet, and only the restoring `acquire` holds its slot. The pass keeps the
/// slot, so the lease the restore ends with is on the slot the pass reads,
/// and it holds the Session until the client detaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pass_during_a_restore_keeps_the_slot_the_restore_holds() {
    let store = Store::new();
    let held = HeldRestore::default();
    let mut builder = builder();
    held.install(&mut builder);
    let server = store.serve(builder).await;
    let index = server.root.get::<AgentIndex>().unwrap();
    let mut client = Client::new(&server).await;
    let id = client.create(&store).await;
    let mut marks = Marks::new();
    settle(&index, &mut marks, 0).await;
    assert_eq!(slots(&index), (0, 0));

    held.hold(&id);
    let mut attaching = Client::new(&server).await;
    let attach = json!({"sessionId":id});
    let attached = tokio::spawn({
        let attach = attach.clone();
        async move {
            attaching.ok("session.attach", attach).await;
            attaching
        }
    });
    held.entered().await;
    assert!(server.agent().unwrap().get_session(&id).is_none());
    index.unload_idle(&mut marks).await;
    assert_eq!(slots(&index), (1, 0));
    held.release();
    let mut attaching = attached.await.unwrap();
    assert_eq!(uses(&index, &id), Some(1));
    index.unload_idle(&mut marks).await;
    index.unload_idle(&mut marks).await;
    assert_eq!(index.session_count(), 1);
    attaching.ok("session.detach", attach).await;
    settle(&index, &mut marks, 0).await;
    assert_eq!(slots(&index), (0, 0));
    drop((client, attaching));
    server.shutdown();
    server.join().await.unwrap();
}

/// S5c §3.2 step 1 and idle unload §6.4: a delete that refuses a Session
/// leaves no slot behind. The leader is unloaded when the delete names it,
/// so the slot that call would create is one nothing keeps: the refusal
/// sweeps it like any other unused slot. Its member is loaded, which an
/// attached client uses, and keeps its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_delete_leaves_no_slot_for_a_session_it_did_not_load() {
    let store = Store::new();
    let mut builder = builder();
    crate::collaboration::install(&mut builder);
    let server = store.serve(builder).await;
    let index = server.root.get::<AgentIndex>().unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new(&server).await;
    let lead = client
        .ok(
            "session.create",
            json!({"cwd":store.0,"tools":["echo","wait","agent"],
                "permissionPolicy":{"allowedTools":["echo","wait","agent"]}}),
        )
        .await["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let holder = client.ok("agent.list", json!({})).await[0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = json!({"action":"request","agentId":holder,"prompt":"hello"});
    client
        .ok(
            "session.submit",
            json!({"sessionId":lead,"branchId":"main",
                "text":format!("tool:agent {request}"),"mode":"queue"}),
        )
        .await;
    let member = poll_member(&agent, &lead).await;
    client
        .ok("session.idle", json!({"sessionId":lead,"branchId":"main"}))
        .await;
    let mut marks = Marks::new();
    settle(&index, &mut marks, 0).await;
    assert_eq!(slots(&index), (0, 0), "both Sessions are unloaded");

    let mut watcher = Client::new(&server).await;
    watcher
        .ok("session.attach", json!({"sessionId":member}))
        .await;
    assert_eq!(slots(&index), (1, 0), "only the loaded member keeps a slot");

    let refused = client
        .rpc(
            "session.delete",
            json!({"sessionId":lead,"requireInactive":true}),
        )
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "session_active",
        "{refused}"
    );
    assert_eq!(
        uses(&index, &lead),
        None,
        "the refused delete kept no slot for the leader"
    );
    assert_eq!(slots(&index), (1, 0));
    drop((client, watcher));
    server.shutdown();
    server.join().await.unwrap();
}

/// The member of the leader's delivered request, once the reply reached the
/// leader.
async fn poll_member(agent: &dim_agent_runtime::Agent, lead: &str) -> String {
    let session = agent.get_session(lead).unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            let report_delivered =
                session
                    .history("main", None)
                    .await
                    .unwrap()
                    .iter()
                    .any(|event| {
                        matches!(&event.kind, EventKind::InputMessage(data)
                    if data.blocks.first().is_some_and(|block| matches!(block,
                        dim_agent_types::InputBlock::Raw { text, .. }
                            if text.starts_with("<dim-agent-report "))))
                    });
            if report_delivered {
                let member = session
                    .history("main", None)
                    .await
                    .unwrap()
                    .iter()
                    .find_map(|event| {
                        let EventKind::ToolResult(data) = &event.kind else {
                            return None;
                        };
                        data.results.iter().find_map(|result| {
                            let dim_agent_types::ToolContent::Text(text) = &result.result.content
                            else {
                                return None;
                            };
                            let value: Value = serde_json::from_str(text).ok()?;
                            (value["status"] == "running")
                                .then(|| value["sessionId"].as_str().unwrap().to_owned())
                        })
                    })
                    .expect("the request names its member");
                return member;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the member reported to the leader")
}

/// Holds the next restore of one Session in its Session part's apply,
/// while `acquire` holds the Session's gate and slot.
#[derive(Clone, Default)]
struct HeldRestore(Arc<HeldState>);
#[derive(Default)]
struct HeldState {
    held: Mutex<Option<String>>,
    entered: Notify,
    release: Notify,
}
impl HeldRestore {
    fn install(&self, builder: &mut AgentBuilder) {
        let held = self.clone();
        builder.require_feature(
            Feature::new("test.held-restore").session("test.held-restore", move |names| {
                Ok(Box::new(HeldRestorePart::new(names, held.clone())?))
            }),
        );
    }
    fn hold(&self, id: &str) {
        *self.0.held.lock().unwrap() = Some(id.to_owned());
    }
    async fn entered(&self) {
        tokio::time::timeout(DEADLINE, self.0.entered.notified())
            .await
            .expect("the held restore started");
    }
    fn release(&self) {
        self.0.release.notify_one();
    }
}
struct HeldRestorePart {
    held: HeldRestore,
    identity: Key<SessionIdentity>,
}
impl HeldRestorePart {
    fn new(names: &Names<'_>, held: HeldRestore) -> rutis::Result<Self> {
        Ok(Self {
            held,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for HeldRestorePart {
    fn name(&self) -> &str {
        "test.held-restore"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let state = &self.held.0;
            let held = identity.restored
                && state
                    .held
                    .lock()
                    .unwrap()
                    .take_if(|id| *id == identity.session_id)
                    .is_some();
            if held {
                state.entered.notify_one();
                state.release.notified().await;
            }
            Ok(Effect::Done)
        })
    }
}
