//! Idle Session unload (idle unload design §13) on disposable native stores
//! with a grace period of a few hundred milliseconds. Every wait ends on an
//! observed transition: the release of a loaded instance (`Session::closed`),
//! the loaded count in `agent.list`, or a call's reply.
use super::super::deletion::eventually;
use super::*;
use dim_agent_runtime::testing::branch_events;
use dim_agent_runtime::{SessionIdentity, TimelineTreeControl};
use rutis::host::CtxHost;
use std::sync::Weak;
use tokio::sync::Notify;

const GRACE: Duration = Duration::from_millis(200);
/// Deadline of one transition; a passing run stays far below it.
const DEADLINE: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checked_attach_rejects_a_different_cwd_before_restoring_the_session() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let id = launch(&sessions).await;
    loads.released(&id).await;
    let alias = store.0.join("legacy-cwd");
    std::os::unix::fs::symlink(std::env::temp_dir(), &alias).unwrap();
    rusqlite::Connection::open(store.database())
        .unwrap()
        .execute(
            "UPDATE agent_sessions SET config=json_set(config,'$.cwd',?2) WHERE session_id=?1",
            rusqlite::params![id, alias.to_str().unwrap()],
        )
        .unwrap();
    let before = loads.of(&id);
    let rejected = client
        .rpc(
            "session.attach",
            json!({
                "sessionId":id,"expectedCwd":std::env::current_dir().unwrap()
            }),
        )
        .await;
    assert!(rejected.get("error").is_some(), "{rejected}");
    assert_eq!(
        loads.of(&id),
        before,
        "a mismatched attach must not mount Session plugins"
    );
    let accepted = client
        .rpc(
            "session.attach",
            json!({
                "sessionId":id,"expectedCwd":std::env::temp_dir()
            }),
        )
        .await;
    assert_ok(&accepted);
    assert_eq!(loads.of(&id), (before.0 + 1, true));
    assert_ok(
        &client
            .rpc(
                "session.attach",
                json!({"sessionId":id,"expectedCwd":std::env::temp_dir()}),
            )
            .await,
    );
    assert_eq!(
        loads.of(&id),
        (before.0 + 1, true),
        "a checked attach to the loaded legacy cwd reuses its Session"
    );

    server.shutdown();
    server.join().await.unwrap();
}

/// Every load of a Session, creation or restore, with its Timeline tree held
/// weakly: once a released instance is no longer referenced anywhere, its
/// tree is gone. It can also hold one Session's next unload in its cleanup,
/// keeping that release under way until the test lets it finish.
#[derive(Clone, Default)]
pub(crate) struct Loads(Arc<LoadsState>);
type Load = (String, bool, Weak<dyn TimelineTreeControl>);
#[derive(Default)]
struct LoadsState {
    loads: std::sync::Mutex<Vec<Load>>,
    held: std::sync::Mutex<Option<String>>,
    entered: Notify,
    release: Notify,
}
impl Loads {
    pub(crate) fn install(&self, builder: &mut AgentBuilder) {
        let loads = self.clone();
        builder.require_feature(
            Feature::new("test.loads").session("test.loads", move |names| {
                Ok(Box::new(LoadsPart::new(names, loads.clone())?))
            }),
        );
    }
    /// How often `id` has loaded, and whether its last load restored it.
    pub(crate) fn of(&self, id: &str) -> (usize, bool) {
        let loads = self.0.loads.lock().unwrap();
        let mut of = loads.iter().filter(|(session, ..)| session == id);
        let count = of.clone().count();
        (
            count,
            of.next_back().is_some_and(|(_, restored, _)| *restored),
        )
    }
    /// Waits until no instance of `id` is referenced any more: nothing keeps
    /// a released Session alive.
    pub(crate) async fn released(&self, id: &str) {
        eventually(&format!("instances of {id} released"), || {
            self.0
                .loads
                .lock()
                .unwrap()
                .iter()
                .filter(|(session, ..)| session == id)
                .all(|(.., tree)| tree.upgrade().is_none())
        })
        .await;
    }
    /// Holds the next unload of `id` in its cleanup until [`Self::release`].
    fn hold(&self, id: &str) {
        *self.0.held.lock().unwrap() = Some(id.to_owned());
    }
    /// Waits until the held unload has started: the pass holds the
    /// Session's admission gate until it finishes.
    async fn entered(&self) {
        tokio::time::timeout(DEADLINE, self.0.entered.notified())
            .await
            .expect("the held unload started");
    }
    fn release(&self) {
        self.0.release.notify_one();
    }
}
struct LoadsPart(
    Loads,
    rutis::host::Key<SessionIdentity>,
    rutis::host::Key<dyn TimelineTreeControl>,
);
impl LoadsPart {
    fn new(names: &dim_agent_runtime::Names<'_>, a0: Loads) -> rutis::Result<Self> {
        Ok(Self(a0, names.key()?, names.key()?))
    }
}
impl rutis::host::DynPlugin for LoadsPart {
    fn name(&self) -> &str {
        "test.loads"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase(), self.2.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.1)?;
            let tree = ctx.require_at(&self.2)?;
            let state = self.0.0.clone();
            state.loads.lock().unwrap().push((
                identity.session_id.clone(),
                identity.restored,
                Arc::downgrade(&tree),
            ));
            ctx.effect(move || async move {
                let held = identity.unloading.is_cancelled()
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
                Ok(())
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

async fn serve_unloading(store: &Store, loads: &Loads, grace: Duration) -> Served {
    serve_with(Setup {
        database: Some(store.database()),
        grace: Some(grace),
        loads: Some(loads.clone()),
        ..Default::default()
    })
    .await
}

/// The default Agent's loaded Sessions, as `agent.list` reports them.
async fn loaded(client: &mut Client) -> u64 {
    let listed = client.rpc("agent.list", json!({})).await;
    listed["result"][0]["sessions"].as_u64().unwrap()
}
async fn launch(sessions: &Sessions) -> String {
    let reply = sessions
        .ok("launch", json!({"cwd":std::env::temp_dir()}))
        .await;
    reply["sessionId"].as_str().unwrap().to_owned()
}
fn at(id: &str) -> Value {
    json!({"sessionId":id,"branchId":"main"})
}
fn text_input(id: &str, submission: &str, text: &str) -> Value {
    json!({"sessionId":id,"branchId":"main","mode":"queue","submissionId":submission,
        "rawBlocks":[],"content":[{"type":"text","text":text}]})
}
/// `main` through `through`, in pages of `limit`.
async fn pages(sessions: &Sessions, id: &str, through: u64, limit: usize) -> Vec<Value> {
    let mut events = vec![];
    let mut after = 0;
    while after < through {
        let page = sessions
            .ok(
                "history",
                json!({"sessionId":id,"branchId":"main","after":after,"limit":limit,"through":through}),
            )
            .await;
        let page = page["events"].as_array().unwrap().clone();
        after = page.last().unwrap()["sequence"].as_u64().unwrap();
        events.extend(page);
    }
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_dropped_runs_unload_and_the_next_input_restores_one() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let mut runs = vec![];
    for run in 0..50 {
        let id = launch(&sessions).await;
        sessions
            .ok("input", text_input(&id, &format!("test:{run}"), "hello"))
            .await;
        let through = sessions.ok("idle", at(&id)).await["sequence"]
            .as_u64()
            .unwrap();
        let events = pages(&sessions, &id, through, 4).await;
        runs.push((id, through, events));
    }
    eventually("every Session unloaded", || agent.sessions().is_empty()).await;
    assert_eq!(loaded(&mut client).await, 0);
    for (id, ..) in &runs {
        loads.released(id).await;
        assert_eq!(loads.of(id), (1, false), "{id} was not restored");
    }
    let listed = client.rpc("session.summaries", json!({})).await;
    for (id, through, _) in &runs {
        let info = listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|info| info["sessionId"] == *id)
            .unwrap_or_else(|| panic!("{id} listed: {listed}"));
        assert_eq!(info["sequence"], *through, "{info}");
    }
    assert_eq!(loaded(&mut client).await, 0);

    let (first, through, _) = &runs[0];
    let receipt = sessions
        .ok("input", text_input(first, "test:again", "again"))
        .await;
    assert_eq!(loads.of(first), (2, true));
    let idle = sessions.ok("idle", at(first)).await["sequence"]
        .as_u64()
        .unwrap();
    let after = pages(&sessions, first, idle, 256).await;
    let after: Vec<_> = after
        .iter()
        .skip_while(|event| event["sequence"].as_u64().unwrap() <= *through)
        .collect();
    assert_eq!(after[0]["type"], "branch", "{after:?}");
    assert_eq!(after[0]["name"], "resume", "{after:?}");
    assert!(
        after
            .iter()
            .skip_while(|event| event["id"] != receipt["inputId"])
            .any(|event| event["type"] == "turn" && event["name"] == "end"),
        "the restored Session ran its turn: {after:?}"
    );
    for (id, through, events) in &runs {
        assert_eq!(&pages(&sessions, id, *through, 4).await, events, "{id}");
        assert!(loads.of(id).1, "expected Session {id} to be restored");
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// §13 item 2: a run's calls (`input`, `idle`, three `history` pages) come
/// closer together than the grace period, so the pass never releases the
/// Session between them: it loads once and `main` gets no `branch/resume`.
/// Once the run is over the Session unloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_is_not_restored_between_its_calls() {
    const GRACE: Duration = Duration::from_millis(800);
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let id = launch(&sessions).await;
    let session = agent.get_session(&id).unwrap();
    let gap = || tokio::time::sleep(GRACE / 4);
    gap().await;
    sessions
        .ok("input", text_input(&id, "test:run", "hello"))
        .await;
    gap().await;
    let through = sessions.ok("idle", at(&id)).await["sequence"]
        .as_u64()
        .unwrap();
    let mut events = vec![];
    let mut after = 0;
    while after < through {
        gap().await;
        let page = sessions
            .ok(
                "history",
                json!({"sessionId":id,"branchId":"main","after":after,"limit":3,"through":through}),
            )
            .await;
        let page = page["events"].as_array().unwrap().clone();
        after = page.last().unwrap()["sequence"].as_u64().unwrap();
        events.extend(page);
    }
    assert!(events.len() > 6, "three pages or more: {events:?}");
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "branch" && event["name"] == "resume"),
        "{events:?}"
    );
    assert_eq!(loads.of(&id), (1, false));
    unloaded(session).await;
    loads.released(&id).await;
    server.shutdown();
    server.join().await.unwrap();
}

/// §13 item 11: without a native store no pass runs, so nothing is
/// unloaded. Nothing happens to wait for, so the test waits several grace
/// periods.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_unloaded_without_a_native_store() {
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        grace: Some(GRACE),
        loads: Some(loads.clone()),
        ..Default::default()
    })
    .await;
    client_session(&mut client, &agent, host_policy()).await;
    let id = launch(&sessions).await;
    sessions
        .ok("input", text_input(&id, "test:run", "hello"))
        .await;
    sessions.ok("idle", at(&id)).await;
    tokio::time::sleep(GRACE * 5).await;
    assert_eq!(loaded(&mut client).await, 2);
    assert!(agent.get_session(&id).is_some());
    assert_eq!(loads.of(&id), (1, false));
    server.shutdown();
    server.join().await.unwrap();
}

/// §6.2: the grace period is positive. `serve` refuses zero, with which
/// the pass would run in a loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zero_grace_is_refused() {
    let refused = serve(
        AgentBuilder::new(AgentConfig::default()),
        ServerConfig {
            idle_unload_after: Duration::ZERO,
            ..Default::default()
        },
    )
    .await
    .err()
    .expect("a zero grace must fail");
    assert!(
        matches!(refused, dim_app_server::ServerError::Configuration(_)),
        "{refused}"
    );
}

/// A launched Session, idle after one turn: the loaded instance.
async fn idle_run(sessions: &Sessions, agent: &Agent) -> Session {
    let id = launch(sessions).await;
    sessions
        .ok("input", text_input(&id, "test:run", "hello"))
        .await;
    sessions.ok("idle", at(&id)).await;
    agent.get_session(&id).unwrap()
}

/// §13 item 3: a Session part holds the Session's unload in its cleanup, so
/// a release is under way when `sessions.history` and an Rpc
/// `session.history` arrive. Both wait for the admission gate the pass
/// holds, then read the restored Session; neither replies `session_closed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_during_an_unload_wait_and_read_the_restored_session() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let session = idle_run(&sessions, &agent).await;
    let id = session.id().to_owned();
    let through = session.sequence("main").unwrap();
    loads.hold(&id);
    loads.entered().await;
    let native = tokio::spawn({
        let sessions = sessions.clone();
        let request = json!({"sessionId":id,"branchId":"main","through":through});
        async move { sessions.call("history", request).await }
    });
    let rpc = client
        .send(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":1000}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!native.is_finished(), "the call waits for the release");
    loads.release();
    unloaded(session).await;
    let native = native.await.unwrap();
    assert_eq!(native["ok"], true, "{native}");
    assert_eq!(native["value"]["through"], through);
    let rpc = client.response(rpc).await;
    assert_ok(&rpc);
    assert!(
        rpc["result"]["through"].as_u64().unwrap() > through,
        "{rpc}"
    );
    assert_eq!(loads.of(&id), (2, true));
    server.shutdown();
    server.join().await.unwrap();
}

/// ADR 021 D3: a client `session.history` read of a Session the pass has
/// unloaded acquires it and pages the same stored facts; the protocol has no
/// branch that reads them without a Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_history_read_acquires_an_unloaded_session() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let id = launch(&sessions).await;
    sessions
        .ok("input", text_input(&id, "test:read", "hello"))
        .await;
    let through = sessions.ok("idle", at(&id)).await["sequence"]
        .as_u64()
        .unwrap();
    let history = json!({"sessionId":id,"branchId":"main","after":0,"limit":256,
        "through":through});
    let before = client.rpc("session.history", history.clone()).await;
    assert_ok(&before);
    unloaded(agent.get_session(&id).unwrap()).await;
    loads.released(&id).await;
    assert_eq!(loads.of(&id), (1, false));

    let page = client.rpc("session.history", history).await;
    assert_ok(&page);
    assert_eq!(loads.of(&id), (2, true), "the read acquired the Session");
    assert_eq!(
        page["result"]["events"], before["result"]["events"],
        "the restored Session pages the same facts"
    );
    assert!(
        page["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "input"
                && event["data"]["user"]["content"][0]["text"] == "hello"),
        "{page}"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// §13 item 9: an Agents index reload while a release is held under way, as
/// in item 3. The reload waits for it; no Session is lost; the next
/// generation unloads the lease-free Session the old pass never reached; and
/// a call after the reload restores the released one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_index_reload_waits_for_a_release_and_the_next_generation_unloads() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let first = idle_run(&sessions, &agent).await;
    let first_id = first.id().to_owned();
    loads.hold(&first_id);
    loads.entered().await;
    let second = idle_run(&sessions, &agent).await;
    let second_id = second.id().to_owned();
    {
        let restart = server.restart("Agents");
        tokio::pin!(restart);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut restart)
                .await
                .is_err(),
            "the old generation ends after its release"
        );
        loads.release();
        restart.await.unwrap();
    }
    unloaded(first).await;
    unloaded(second).await;
    let listed = client.rpc("session.summaries", json!({})).await;
    for id in [&first_id, &second_id] {
        assert!(
            listed["result"]
                .as_array()
                .unwrap()
                .iter()
                .any(|info| info["sessionId"] == **id),
            "{id}: {listed}"
        );
    }
    assert_eq!(loads.of(&second_id), (1, false));
    let read = sessions.ok("history", at(&first_id)).await;
    assert!(read["through"].as_u64().unwrap() > 0, "{read}");
    assert_eq!(loads.of(&first_id), (2, true));
    server.shutdown();
    server.join().await.unwrap();
}

/// Waits until a lease-free Session created now has been unloaded: at least
/// two passes have run since, each over every loaded Session.
async fn passes(sessions: &Sessions, agent: &Agent) {
    let id = launch(sessions).await;
    unloaded(agent.get_session(&id).unwrap()).await;
}

/// §13 item 5: an attached client holds its Session. It stays loaded while
/// lease-free Sessions beside it unload; after a detach it unloads, a
/// re-attach restores it and its events stream again, and closing the
/// connection releases it as a detach does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attached_client_holds_its_session_until_it_detaches_or_disconnects() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    let session = client_session(&mut client, &agent, host_policy()).await;
    let id = session.id().to_owned();
    let attach = json!({"sessionId":id});
    assert_ok(&client.rpc("session.attach", attach.clone()).await);
    passes(&sessions, &agent).await;
    passes(&sessions, &agent).await;
    assert!(!session.is_unloading());
    assert_ok(&client.rpc("session.detach", attach.clone()).await);
    unloaded(session).await;

    assert_ok(&client.rpc("session.attach", attach.clone()).await);
    assert_eq!(loads.of(&id), (2, true));
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"streamed"}),
            )
            .await,
    );
    loop {
        let notice = client.notice("session.event").await;
        let event = &notice["params"]["event"];
        if notice["params"]["sessionId"] == *id && event["type"] == "turn" && event["name"] == "end"
        {
            break;
        }
    }
    assert_ok(&client.rpc("session.detach", attach.clone()).await);

    let session = agent.get_session(&id).unwrap();
    let mut other = Client::new_version(&server, false, 4).await;
    assert_ok(&other.rpc("session.attach", attach.clone()).await);
    passes(&sessions, &agent).await;
    assert!(!session.is_unloading());
    drop(other);
    unloaded(session).await;
    loads.released(&id).await;
    server.shutdown();
    server.join().await.unwrap();
}

/// §13 item 7: a launched non-interactive Session whose turn waits on an
/// approval nobody answers holds no lease, yet stays loaded while lease-free
/// Sessions beside it unload. `stop` ends the turn, and the Session unloads
/// after the grace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_approval_keeps_a_launched_session_until_stop() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let session = awaiting_approval(&sessions, &agent).await;
    passes(&sessions, &agent).await;
    passes(&sessions, &agent).await;
    assert!(!session.is_unloading());
    sessions.ok("stop", at(session.id())).await;
    let id = session.id().to_owned();
    unloaded(session).await;
    assert_eq!(loads.of(&id), (1, false));
    server.shutdown();
    server.join().await.unwrap();
}

/// Echo model whose first reply streams its first half, then waits for the
/// test before the rest.
struct HeldStream {
    held: std::sync::atomic::AtomicBool,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl dim_agent_capabilities::host::ModelDriver for HeldStream {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> dim_agent_capabilities::host::HostFuture<'_, dim_agent_capabilities::host::ModelStream>
    {
        use dim_agent_capabilities::host::ModelChunk;
        Box::pin(async move {
            let stream = dim_agent_capabilities::host::ModelDriver::invoke(
                &dim_agent_capabilities::adapters::EchoModel,
                request,
                connection,
                cancel,
            )
            .await?;
            if !self.held.swap(false, Ordering::SeqCst) {
                return Ok(stream);
            }
            let mut head = vec![];
            let mut tail = vec![];
            for chunk in stream.collect::<Vec<_>>().await {
                match chunk? {
                    ModelChunk::Text(text) => {
                        let (first, second) = text.split_at(text.len() / 2);
                        head.push(Ok(ModelChunk::Text(first.into())));
                        tail.push(Ok(ModelChunk::Text(second.into())));
                    }
                    chunk if tail.is_empty() => head.push(Ok(chunk)),
                    chunk => tail.push(Ok(chunk)),
                }
            }
            let (entered, release) = (self.entered.clone(), self.release.clone());
            let wait = futures_util::stream::once(async move {
                entered.notify_one();
                release.notified().await;
            })
            .filter_map(|()| async { None });
            Ok(Box::pin(
                futures_util::stream::iter(head)
                    .chain(wait)
                    .chain(futures_util::stream::iter(tail)),
            ) as dim_agent_capabilities::host::ModelStream)
        })
    }
}

/// A refused unload never disturbs the turn it found: a launched Session
/// whose model response is half streamed holds no lease, and every pass
/// refuses to release it. Once the stream resumes the turn ends with the
/// whole response, uninterrupted, and the Session unloads after the grace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_unload_leaves_a_streaming_response_intact() {
    let store = Store::new();
    let loads = Loads::default();
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        database: Some(store.database()),
        grace: Some(GRACE),
        loads: Some(loads.clone()),
        model: Some(Arc::new(HeldStream {
            held: true.into(),
            entered: entered.clone(),
            release: release.clone(),
        })),
        ..Default::default()
    })
    .await;
    client_session(&mut client, &agent, host_policy()).await;
    let id = launch(&sessions).await;
    let session = agent.get_session(&id).unwrap();
    let receipt = sessions
        .ok("input", text_input(&id, "test:stream", "a streamed reply"))
        .await;
    tokio::time::timeout(DEADLINE, entered.notified())
        .await
        .expect("the reply started streaming");
    passes(&sessions, &agent).await;
    passes(&sessions, &agent).await;
    assert!(!session.is_unloading());
    release.notify_one();
    let through = sessions.ok("idle", at(&id)).await["sequence"]
        .as_u64()
        .unwrap();
    let events = branch_events(&session, "main", ..=through).await.unwrap();
    let turn: Vec<_> = events
        .iter()
        .skip_while(|event| event.id != receipt["inputId"])
        .collect();
    let text: String = turn
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Text(data) => Some(data.delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Echo: a streamed reply");
    assert!(
        !turn
            .iter()
            .any(|event| matches!(event.kind, EventKind::StopInterrupt(_))),
        "{turn:?}"
    );
    assert!(turn.iter().any(|event| matches!(&event.kind,
        EventKind::TurnEnd(data)
            if matches!(data.outcome, dim_agent_types::TurnOutcome::Completed { .. }))));
    unloaded(session).await;
    assert_eq!(loads.of(&id), (1, false));
    server.shutdown();
    server.join().await.unwrap();
}

/// The stored head of each branch of `id`.
fn stored_heads(store: &Store, id: &str) -> std::collections::BTreeMap<String, u64> {
    let connection = rusqlite::Connection::open(store.database()).unwrap();
    let mut query = connection
        .prepare("SELECT branch_id, sequence FROM timeline_branches WHERE session_id=?1")
        .unwrap();
    query
        .query_map([id], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// §13 item 4, the Session's own part: an unload is not a close. It leaves
/// the stored head of every branch where it was, writing no `stop/interrupt`
/// or anything else; the Session stays in `session.summaries`; and the restore
/// adds only a `branch/resume` to each branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unload_writes_nothing_and_the_restore_only_records_housekeeping() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    let mut create = create(json!(["branch"]));
    create["permissionPolicy"]["allowedTools"] = json!(["branch"]);
    let created = client.rpc("session.create", create).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    super::super::deletion::turn(&mut client, &id, "hello").await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,"parentBranchId":"main","at":session.sequence("main").unwrap(),
                "prompt":"child"}),
        )
        .await;
    assert_ok(&created);
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut heads = std::collections::BTreeMap::new();
    for branch in ["main", child.as_str()] {
        let request = json!({"sessionId":id,"branchId":branch});
        let idle = sessions.ok("idle", request).await["sequence"]
            .as_u64()
            .unwrap();
        heads.insert(branch.to_owned(), idle);
    }
    assert_eq!(stored_heads(&store, &id), heads);
    // #1099: the activity time is what the turn wrote. Unload writes
    // nothing, and the restore writes only `branch/resume`, which is
    // housekeeping, so the row must not move either time.
    let updated_at = |store: &Store, id: &str| -> i64 {
        rusqlite::Connection::open(store.database())
            .unwrap()
            .query_row(
                "SELECT updated_at_ms FROM session_catalog WHERE session_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let steady = updated_at(&store, &id);
    unloaded(session).await;
    assert_eq!(updated_at(&store, &id), steady);
    assert_eq!(stored_heads(&store, &id), heads);
    let listed = client.rpc("session.summaries", json!({})).await;
    assert!(
        listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|info| info["sessionId"] == *id && info["sequence"] == heads["main"]),
        "{listed}"
    );
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    assert_eq!(updated_at(&store, &id), steady);
    for (branch, head) in &heads {
        let at = json!({"sessionId":id,"branchId":branch});
        let idle = sessions.ok("idle", at.clone()).await["sequence"]
            .as_u64()
            .unwrap();
        let mut request = at;
        request["after"] = json!(head);
        request["through"] = json!(idle);
        let added = sessions.ok("history", request).await["events"].clone();
        let added: Vec<_> = added
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| {
                !((event["type"] == "input"
                    && ((event["name"] == "applied" && event["data"]["mode"] == "append")
                        || (event["name"] == "message" && event["data"]["user"].is_null())))
                    || (event["type"] == "input-queue"
                        && matches!(event["name"].as_str(), Some("node" | "recovered")))
                    || (event["type"] == "prompt-state" && event["name"] == "recovered"))
            })
            .map(|event| format!("{}/{}", event["type"], event["name"]).replace('"', ""))
            .collect();
        assert_eq!(added, ["branch/resume"], "{branch}");
    }
    assert_eq!(loads.of(&id), (2, true));
    server.shutdown();
    server.join().await.unwrap();
}

/// §13 item 8, the Agents part: `session.summaries`, called again and again
/// while Sessions unload under it, never fails and never omits a stored
/// Session or event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listing_while_sessions_unload_never_fails_or_omits_one() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let mut runs = std::collections::BTreeMap::new();
    let mut last = String::new();
    for run in 0..10 {
        let id = launch(&sessions).await;
        last.clone_from(&id);
        let text = format!("run {run}");
        sessions
            .ok("input", text_input(&id, "test:run", &text))
            .await;
        let head = sessions.ok("idle", at(&id)).await["sequence"].clone();
        runs.insert(id, (head, text));
    }
    let check = async |client: &mut Client| {
        let listed = client.rpc("session.summaries", json!({})).await;
        let summaries = client.rpc("session.summaries", json!({})).await;
        for reply in [&listed, &summaries] {
            assert_ok(reply);
            for (id, (head, _)) in &runs {
                let row = reply["result"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["sessionId"] == **id)
                    .unwrap_or_else(|| panic!("{id} omitted: {reply}"));
                assert_eq!(row["sequence"], *head, "{row}");
                if *reply == summaries {
                    assert_eq!(row["preview"], runs[id].1, "{row}");
                }
            }
        }
    };
    loads.hold(&last);
    loads.entered().await;
    check(&mut client).await;
    loads.release();
    tokio::time::timeout(DEADLINE, async {
        loop {
            let unloading = !agent.sessions().is_empty();
            check(&mut client).await;
            if !unloading {
                break;
            }
        }
    })
    .await
    .expect("every Session unloaded");
    server.shutdown();
    server.join().await.unwrap();
}

/// §13 item 10: `sessions.delete` of an unloaded Session removes its rows
/// without restoring it. A fork of an unloaded source restores the source,
/// and both unload after the grace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unloaded_session_is_deleted_as_stored_and_forked_after_a_restore() {
    let store = Store::new();
    let loads = Loads::default();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_unloading(&store, &loads, GRACE).await;
    client_session(&mut client, &agent, host_policy()).await;
    let doomed = idle_run(&sessions, &agent).await;
    let doomed_id = doomed.id().to_owned();
    unloaded(doomed).await;
    assert_eq!(
        sessions.ok("delete", json!({"sessionId":doomed_id})).await,
        json!({})
    );
    assert_eq!(
        loads.of(&doomed_id),
        (1, false),
        "deleted without a restore"
    );
    assert!(stored_heads(&store, &doomed_id).is_empty());
    let listed = client.rpc("session.summaries", json!({})).await;
    assert!(
        !listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|info| info["sessionId"] == *doomed_id),
        "{listed}"
    );

    let source = idle_run(&sessions, &agent).await;
    let source_id = source.id().to_owned();
    let cut = source.sequence("main").unwrap();
    unloaded(source).await;
    let forked = client
        .rpc("session.fork", json!({"sessionId":source_id,"at":cut}))
        .await;
    assert_ok(&forked);
    let fork_id = forked["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{forked}"))
        .to_owned();
    assert_eq!(loads.of(&source_id), (2, true));
    for id in [&source_id, &fork_id] {
        eventually(&format!("{id} unloaded"), || {
            agent.get_session(id).is_none()
        })
        .await;
        loads.released(id).await;
    }
    assert_eq!(loads.of(&fork_id), (1, false));
    let listed = client.rpc("session.summaries", json!({})).await;
    for id in [&source_id, &fork_id] {
        assert!(
            listed["result"]
                .as_array()
                .unwrap()
                .iter()
                .any(|info| info["sessionId"] == **id),
            "{id} unloaded, not closed: {listed}"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
}
