//! Idle Session unload (idle unload design §13) on disposable native stores
//! with a grace period of a few hundred milliseconds. Every wait ends on an
//! observed transition: the release of a loaded instance (`Session::closed`),
//! the loaded count in `agent.list`, or a call's reply.
use super::super::deletion::eventually;
use super::*;
use dim_agent_capabilities::file_storage::{FileStorage, testing};
use dim_agent_runtime::SessionLines;
use dim_agent_runtime::testing::branch_events;
use dim_agent_runtime::{SessionScope, TimelineTreeControl};
use dim_app_server::mcp::config::McpSettings;
use dim_app_server::settings::{SettingsScope, SettingsService};
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
    rutis::host::Key<SessionScope>,
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
                identity.session_id(),
                identity.describe().expect("the Session is live").restored,
                Arc::downgrade(&tree),
            ));
            ctx.effect(move || async move {
                let held = identity
                    .describe()
                    .map(|description| description.restored)
                    .unwrap_or(false)
                    && state
                        .held
                        .lock()
                        .unwrap()
                        .take_if(|id| *id == identity.session_id())
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
            .input(text_input(&id, &format!("test:{run}"), "hello"))
            .await;
        let through = sessions.idle(&at(&id)).await;
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
        .input(text_input(first, "test:again", "again"))
        .await;
    assert_eq!(loads.of(first), (2, true));
    let idle = sessions.idle(&at(first)).await;
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
            .skip_while(|event| event["id"] != receipt["id"])
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
    sessions.input(text_input(&id, "test:run", "hello")).await;
    gap().await;
    let through = sessions.idle(&at(&id)).await;
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
    sessions.input(text_input(&id, "test:run", "hello")).await;
    sessions.idle(&at(&id)).await;
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
    sessions.input(text_input(&id, "test:run", "hello")).await;
    sessions.idle(&at(&id)).await;
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
    sessions.input(text_input(&id, "test:read", "hello")).await;
    let through = sessions.idle(&at(&id)).await;
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
    sessions.stop(&at(session.id())).await;
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
        .input(text_input(&id, "test:stream", "a streamed reply"))
        .await;
    tokio::time::timeout(DEADLINE, entered.notified())
        .await
        .expect("the reply started streaming");
    passes(&sessions, &agent).await;
    passes(&sessions, &agent).await;
    assert!(!session.is_unloading());
    release.notify_one();
    let through = sessions.idle(&at(&id)).await;
    let events = branch_events(&session, "main", ..=through).await.unwrap();
    let turn: Vec<_> = events
        .iter()
        .skip_while(|event| event.id != receipt["id"])
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
        let idle = sessions.idle(&request).await;
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
        let idle = sessions.idle(&at.clone()).await;
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
        sessions.input(text_input(&id, "test:run", &text)).await;
        let head = sessions.idle(&at(&id)).await;
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

/// One catalog row's `sequence`, as a client reads it.
async fn catalog_sequence(client: &mut Client, id: &str) -> u64 {
    let listed = client.rpc("session.catalog", json!({})).await;
    assert_ok(&listed);
    listed["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .unwrap_or_else(|| panic!("{id} is listed: {listed}"))["sequence"]
        .as_u64()
        .unwrap()
}

/// §14: the catalog row's `sequence` is the root's in both projections, so an
/// idle unload with the default branch on a longer line changes no sequence;
/// only the running state and the activity follow the default branch (design
/// §14, §15).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_catalog_sequence_stays_the_roots_across_an_idle_unload() {
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
    super::super::deletion::turn(&mut client, &id, "root turn").await;
    let cut = session.sequence("main").unwrap();
    let opened = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,"parentBranchId":"main","at":cut,"prompt":"child"}),
        )
        .await;
    assert_ok(&opened);
    let child = opened["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    // The line runs two turns, so its head passes the root's.
    sessions
        .idle(&json!({"sessionId":id,"branchId":"main"}))
        .await;
    for (submission, text) in [("test:one", "one"), ("test:two", "two")] {
        sessions
            .write_published(json!({"sessionId":id,"branchId":child,"mode":"queue",
                    "submissionId":submission,"rawBlocks":[],
                    "content":[{"type":"text","text":text}]}))
            .await;
        sessions
            .idle(&json!({"sessionId":id,"branchId":child}))
            .await;
    }
    assert_ok(
        &client
            .rpc(
                "session.branch_set_default",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    let heads = stored_heads(&store, &id);
    assert!(
        heads[&child] > heads["main"],
        "the line has the longer head: {heads:?}"
    );
    let loaded = catalog_sequence(&mut client, &id).await;
    assert_eq!(
        loaded, heads["main"],
        "the loaded row reports the root's sequence"
    );
    unloaded(session).await;
    loads.released(&id).await;
    let cold = catalog_sequence(&mut client, &id).await;
    assert_eq!(
        cold, heads["main"],
        "the cold row reports the root's sequence"
    );
    assert_eq!(cold, loaded, "loading or unloading changed no sequence");
    assert_eq!(
        loads.of(&id),
        (1, false),
        "the catalog read restored nothing"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A server with Settings and its store in `store`'s directory, which
/// authorizes a Session's project, and a configured model so a client can
/// create Sessions.
async fn serve_with_project(store: &Store, grace: Duration) -> Served {
    use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
    let models = Arc::new(
        AimuxModels::from_config(
            serde_json::from_value::<ModelConfigFile>(json!({
                "defaultModel":{"providerId":"local","modelId":"alpha"},
                "providers":[{"id":"local","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1",
                    "models":[{"id":"alpha","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":64}}]}]
            }))
            .unwrap(),
        )
        .unwrap(),
    );
    serve_with(Setup {
        target_home: Some(store.0.clone()),
        models: Some(models),
        grace: Some(grace),
        ..Default::default()
    })
    .await
}

/// A Session that stops using a project gives up its root descriptor; the
/// authorization keeps the index, and the next operation opens the directory
/// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unloaded_session_releases_its_project_root() {
    let store = Store::new();
    let Served {
        server, mut client, ..
    } = serve_with_project(&store, GRACE).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": project.clone(), "tools": ["confirm"], "permissionPolicy": host_policy()}),
        )
        .await;
    assert_ok(&created);
    let root = settings
        .project_root(&project)
        .expect("the creation authorized the project");
    assert!(
        testing::root_held(&files, root),
        "the Session holds the project's directory"
    );
    let released = testing::root_releases(&files, root);
    eventually("the Session unloaded", || {
        testing::root_releases(&files, root) > released
    })
    .await;
    assert_eq!(
        settings.project_root(&project),
        Some(root),
        "the unload kept the authorization"
    );
    let cancel = CancellationToken::new();
    assert_eq!(
        settings.authorize_project(project, &cancel).await.unwrap(),
        root
    );
    assert!(
        testing::root_held(&files, root),
        "the next registration opened the directory again"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A close ends the Session's use of its project the same way an unload does,
/// and keeps the authorization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_session_releases_its_project_root() {
    let store = Store::new();
    let Served {
        server, mut client, ..
    } = serve_with_project(&store, Duration::from_secs(30)).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": project.clone(), "tools": ["confirm"], "permissionPolicy": host_policy()}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let root = settings
        .project_root(&project)
        .expect("the creation authorized the project");
    assert!(testing::root_held(&files, root));
    let released = testing::root_releases(&files, root);
    assert_ok(&client.rpc("session.close", json!({"sessionId": id})).await);
    assert!(
        testing::root_releases(&files, root) == released + 1,
        "the close released the project's descriptor"
    );
    assert_eq!(
        settings.project_root(&project),
        Some(root),
        "the close kept the authorization"
    );
    let cancel = CancellationToken::new();
    assert_eq!(
        settings.authorize_project(project, &cancel).await.unwrap(),
        root
    );
    assert!(testing::root_held(&files, root));
    server.shutdown();
    server.join().await.unwrap();
}

/// A Session only the runtime closes, as the fault supervisor does, ends the
/// ownership its load registered and releases the project's root, without an
/// App Server close, delete or unload settling it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_close_releases_the_project_root_a_load_registers() {
    let store = Store::new();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let id = {
        let Served {
            server, mut client, ..
        } = serve_with_project(&store, Duration::from_secs(30)).await;
        let created = client
            .rpc(
                "session.create",
                json!({"cwd": project.clone(), "tools": ["confirm"], "permissionPolicy": host_policy()}),
            )
            .await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        server.shutdown();
        server.join().await.unwrap();
        id
    };
    let Served {
        server,
        agent,
        mut client,
        ..
    } = serve_with_project(&store, Duration::from_secs(30)).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    // A stored Session loads through the Agents index on its next use, and that
    // load is what registers the ownership this test never holds itself.
    assert_ok(&client.rpc("session.attach", json!({"sessionId": id})).await);
    let session = agent.get_session(&id).expect("the use loaded the Session");
    let root = settings
        .project_root(&project)
        .expect("the load authorized the project");
    assert!(
        testing::root_held(&files, root),
        "the loaded Session holds the project's directory"
    );
    let released = testing::root_releases(&files, root);
    session.close().await.unwrap();
    eventually("the runtime close released the project root", || {
        testing::root_releases(&files, root) > released
    })
    .await;
    assert_eq!(
        settings.project_root(&project),
        Some(root),
        "the runtime close kept the authorization"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// A delete ends a loaded Session without going through `session.close`, so it
/// releases the project's descriptor itself once no Session uses it, and keeps
/// the authorization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_session_releases_its_project_root() {
    let store = Store::new();
    let Served {
        server, mut client, ..
    } = serve_with_project(&store, Duration::from_secs(30)).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": project.clone(), "tools": ["confirm"], "permissionPolicy": host_policy()}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let root = settings
        .project_root(&project)
        .expect("the creation authorized the project");
    assert!(testing::root_held(&files, root));
    let released = testing::root_releases(&files, root);
    assert_ok(&client.rpc("session.delete", json!({"sessionId": id})).await);
    assert!(
        testing::root_releases(&files, root) == released + 1,
        "the delete released the project's descriptor"
    );
    assert_eq!(
        settings.project_root(&project),
        Some(root),
        "the delete kept the authorization"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// `agent.close` ends every Session of its Agent at once; each project those
/// Sessions used loses its descriptor, and the authorization stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_close_releases_its_sessions_project_roots() {
    let store = Store::new();
    let Served {
        server, mut client, ..
    } = serve_with_project(&store, Duration::from_secs(30)).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let agent = client.rpc("agent.create", json!({})).await;
    assert_ok(&agent);
    let agent_id = agent["result"]["agentId"].as_str().unwrap().to_owned();
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": project.clone(),
                "tools": ["confirm"],
                "permissionPolicy": host_policy(),
                "agentId": agent_id,
            }),
        )
        .await;
    assert_ok(&created);
    let root = settings
        .project_root(&project)
        .expect("the creation authorized the project");
    assert!(testing::root_held(&files, root));
    let released = testing::root_releases(&files, root);
    assert_ok(
        &client
            .rpc("agent.close", json!({"agentId": agent_id}))
            .await,
    );
    assert!(
        testing::root_releases(&files, root) == released + 1,
        "the Agent close released its Session's project"
    );
    assert_eq!(
        settings.project_root(&project),
        Some(root),
        "the Agent close kept the authorization"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Two Sessions share one project. Ending one leaves the project in use: its
/// descriptor stays, and the other Session reads and writes the project's
/// settings through the same root. The last close releases the descriptor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_of_two_sessions_in_a_project_keeps_its_root_in_use() {
    let store = Store::new();
    let Served {
        server, mut client, ..
    } = serve_with_project(&store, Duration::from_secs(30)).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let created = client
            .rpc(
                "session.create",
                json!({"cwd": project.clone(), "tools": ["confirm"], "permissionPolicy": host_policy()}),
            )
            .await;
        assert_ok(&created);
        sessions.push(created["result"]["sessionId"].as_str().unwrap().to_owned());
    }
    let root = settings
        .project_root(&project)
        .expect("the creations authorized the project");
    assert!(testing::root_held(&files, root));
    let cancel = CancellationToken::new();
    let scope = SettingsScope::Project(root);
    let missing = settings.read::<McpSettings>(scope, &cancel).await.unwrap();
    assert_eq!(missing.revision, "missing");
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": sessions[0]}))
            .await,
    );
    assert!(
        testing::root_held(&files, root),
        "the other Session still uses the project"
    );
    let written = settings
        .update(
            scope,
            &missing.revision,
            McpSettings::from_value(json!({"mcpServers": {}})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let read = settings.read::<McpSettings>(scope, &cancel).await.unwrap();
    assert!(
        read.document == written.document,
        "the second Session read back the document it wrote"
    );
    assert_ne!(read.revision, "missing");
    let released = testing::root_releases(&files, root);
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId": sessions[1]}))
            .await,
    );
    assert!(
        testing::root_releases(&files, root) == released + 1,
        "the last close released the project's descriptor"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// The project's directory is replaced while a Session is loaded: the next
/// creation registers the path again and serves the new directory, so the
/// settings read and write reach it instead of the directory the path no
/// longer names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_project_directory_is_served_after_a_session_is_created() {
    let store = Store::new();
    let Served {
        server, mut client, ..
    } = serve_with_project(&store, Duration::from_secs(30)).await;
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = store.0.canonicalize().unwrap().join("project");
    std::fs::create_dir(&project).unwrap();
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": project.clone(), "tools": ["confirm"], "permissionPolicy": host_policy()}),
        )
        .await;
    assert_ok(&created);
    let root = settings
        .project_root(&project)
        .expect("the creation authorized the project");
    assert!(testing::root_held(&files, root));
    let cancel = CancellationToken::new();
    let scope = SettingsScope::Project(root);
    let missing = settings.read::<McpSettings>(scope, &cancel).await.unwrap();
    settings
        .update(
            scope,
            &missing.revision,
            McpSettings::from_value(json!({"mcpServers": {}})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_ne!(
        settings
            .read::<McpSettings>(scope, &cancel)
            .await
            .unwrap()
            .revision,
        "missing",
        "the first directory holds the written document"
    );

    std::fs::remove_dir_all(&project).unwrap();
    std::fs::create_dir(&project).unwrap();
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": project, "tools": ["confirm"], "permissionPolicy": host_policy()}),
        )
        .await;
    assert_ok(&created);
    assert_eq!(
        settings.project_root(&project),
        Some(root),
        "the registration kept the authorization and its index"
    );
    let replaced = settings.read::<McpSettings>(scope, &cancel).await.unwrap();
    assert_eq!(
        replaced.revision, "missing",
        "the root serves the replacement directory, which holds no document"
    );
    settings
        .update(
            scope,
            &replaced.revision,
            McpSettings::from_value(json!({"mcpServers": {}})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_ne!(
        settings
            .read::<McpSettings>(scope, &cancel)
            .await
            .unwrap()
            .revision,
        "missing",
        "the replacement directory holds the document written through the root"
    );
    server.shutdown();
    server.join().await.unwrap();
}
