//! `session.model.get`: one line's adopted configuration at that line's own
//! cutoff.
use super::*;
use dim_agent_capabilities::prompt_state::PromptSnapshot;
use dim_agent_runtime::{ConfigureUpdate, Dispatcher, Timeline, TimelineIdentity};
use dim_agent_types::{
    ContextConfiguredData, EventKind, ModelConfiguration, PromptChange, PromptChangeReason,
    PromptChangedData, PromptDelivery, PromptEntry, PromptScope,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};

fn create() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch"],
        "permissionPolicy": {"allowedTools": ["branch"]}
    })
}

fn configured(model: &str) -> ContextConfiguredData {
    ContextConfiguredData {
        timeline: "main".into(),
        revision: model.into(),
        previous: None,
        prompt: None,
        settings: Box::new(ModelConfiguration {
            provider_id: "mock".into(),
            model_id: model.into(),
            capability_snapshot: None,
            tools: None,
            tool_sources: vec![],
            config: None,
            metadata: None,
        }),
        system: None,
        system_source: None,
    }
}

/// The line's last published rest point, as `session.history` reads it.
async fn head(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["through"].as_u64().unwrap()
}

/// Opens a child of `parent` at its last rest point.
async fn open_line(client: &mut Client, session: &str, parent: &str, prompt: &str) -> String {
    let at = head(client, session, parent).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":parent,"at":at,"prompt":prompt}),
        )
        .await;
    assert_ok(&created);
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn directory(client: &mut Client, session: &str) -> Vec<Value> {
    let reply = client
        .rpc("session.branches", json!({"sessionId":session}))
        .await;
    assert_ok(&reply);
    reply["result"]["branches"].as_array().unwrap().clone()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["branchId"] == id)
        .unwrap_or_else(|| panic!("{id} is missing: {rows:?}"))
}

async fn adopt(session: &dim_agent_runtime::Session, branch: &str, model: &str) {
    session
        .configure(
            ConfigureUpdate {
                model: Some(dim_agent_types::ModelSelection {
                    provider_id: "mock".into(),
                    model_id: model.into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
                ..Default::default()
            },
            branch,
        )
        .await
        .unwrap();
}

/// `session.model.get` at an open line's current head, or at a cutoff.
async fn read(client: &mut Client, session: &str, branch: &str, through: Option<u64>) -> Value {
    let mut params = json!({"sessionId":session,"branchId":branch});
    if let Some(through) = through {
        params["through"] = json!(through);
    }
    client.rpc("session.model.get", params).await
}

fn model_of(reply: &Value) -> &str {
    assert_ok(reply);
    reply["result"]["configuration"]["model"]["modelId"]
        .as_str()
        .unwrap_or_else(|| panic!("a configuration: {reply}"))
}

fn through_of(reply: &Value) -> u64 {
    assert_ok(reply);
    reply["result"]["through"].as_u64().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_answers_its_own_configuration_and_its_own_cutoff() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.model.get")),
        "the feature is advertised: {:?}",
        client.features
    );
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    // Grow the root well past the child's own sequence range, so a cutoff
    // borrowed from the parent would visibly swallow the child's facts.
    for turn in 0..4 {
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":id,"branchId":"main","text":format!("turn {turn}")}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
                .await,
        );
    }
    adopt(&session, "main", "alpha").await;

    let root = read(&mut client, &id, "main", None).await;
    assert_eq!(model_of(&root), "alpha");
    assert_eq!(root["result"]["branchId"], "main");
    let root_through = through_of(&root);

    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;
    let rows = directory(&mut client, &id).await;
    let cut = row(&rows, &child)["branchPoint"].as_u64().unwrap();

    let line = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&line), "beta");
    assert_eq!(line["result"]["branchId"], child);
    let line_through = through_of(&line);
    assert!(
        line_through < cut,
        "the line's cutoff {line_through} stays below the parent's cut {cut}"
    );
    assert!(
        line_through < root_through,
        "the line's cutoff {line_through} is not the root's {root_through}"
    );

    let root = read(&mut client, &id, "main", None).await;
    assert_eq!(model_of(&root), "alpha");
    assert!(
        through_of(&root) >= root_through,
        "the root's head only moves forward"
    );

    // A client that applies only this line's `Configuration` events above the
    // returned cutoff still takes the next change.
    adopt(&session, &child, "gamma").await;
    let changed = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&changed), "gamma");
    assert!(
        through_of(&changed) > line_through,
        "the later adoption is above the earlier cutoff"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ended_line_reads_at_its_fixed_head_and_an_unknown_line_is_an_error() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;

    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    let rows = directory(&mut client, &id).await;
    let ended = row(&rows, &child);
    assert_eq!(ended["state"], "ended", "{rows:?}");
    let fixed = ended["head"].as_u64().unwrap();

    let line = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&line), "beta");
    assert_eq!(through_of(&line), fixed);

    let earlier = read(&mut client, &id, &child, Some(1)).await;
    assert_eq!(model_of(&earlier), "alpha");
    assert_eq!(through_of(&earlier), 1);

    let after = directory(&mut client, &id).await;
    assert_eq!(
        row(&after, &child)["state"],
        "ended",
        "the read reopens nothing"
    );
    assert_eq!(row(&after, &child)["head"].as_u64().unwrap(), fixed);

    for through in [None, Some(1)] {
        let unknown = read(&mut client, &id, "no-such-line", through).await;
        assert_eq!(
            unknown["error"]["data"]["appError"], "session_invalid_input",
            "{unknown}"
        );
        assert!(
            unknown["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown branch"),
            "{unknown}"
        );
    }

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A test branch part that hands each branch's Timeline to the test, so it
/// can commit facts without waiting for a producer.
struct TimelineProbePart {
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
    probe: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Arc<Timeline>>>>,
}
impl DynPlugin for TimelineProbePart {
    fn name(&self) -> &str {
        "TimelineProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let branch = ctx.require_at(&self.identity)?.branch_id.clone();
            self.probe.lock().unwrap().insert(branch, timeline);
            Ok(Effect::Done)
        })
    }
}

fn timeline_probe(
    probe: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Arc<Timeline>>>>,
) -> impl FnOnce(&mut AgentBuilder) + Send + Sync + 'static {
    move |builder| {
        builder.require_feature(Feature::new("TimelineProbe").branch(
            "TimelineProbe",
            move |names| {
                Ok(Box::new(TimelineProbePart {
                    timeline: names.key()?,
                    identity: names.key()?,
                    probe: probe.clone(),
                }))
            },
        ));
    }
}

/// A server whose store is SQLite, so `storage::timeline_testing` can hold a
/// publication; the returned directory is the test's to remove.
async fn launch_with_sqlite(
    probe: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Arc<Timeline>>>>,
    extra: impl FnOnce(&mut AgentBuilder),
) -> (RunningServer, std::path::PathBuf) {
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::path::PathBuf::from("/tmp")
        .join(format!("dim-model-get-sqlite-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    timeline_probe(probe)(&mut builder);
    extra(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            unix_path: Some(directory.join("server.sock")),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (server, directory)
}

/// A published prompt change the owner has not replayed, so a read that
/// replays sources writes a `prompt-state/recovered` checkpoint.
fn pending_prompt_change(timeline: &Timeline, branch: &str) -> Arc<dim_agent_types::AgentEvent> {
    timeline
        .append(
            EventKind::PromptChanged(PromptChangedData {
                timeline: branch.into(),
                revision: "pending".into(),
                previous: None,
                reason: PromptChangeReason::Refresh,
                changes: vec![PromptChange::Set {
                    entry: PromptEntry {
                        source: "pending".into(),
                        version: "p1".into(),
                        body: "pending state".into(),
                        scope: PromptScope::Branch {
                            timeline: branch.into(),
                        },
                        delivery: PromptDelivery::Context,
                        order: 0,
                    },
                }],
                inputs: Vec::new(),
            })
            .into(),
        )
        .unwrap()
}

/// Stops the Dispatcher before a committed `context/configured` reaches the
/// owner, so a read can be taken while the fact is committed and not yet
/// dispatched.
struct DispatchGate {
    armed: AtomicBool,
    arrived: tokio::sync::mpsc::UnboundedSender<()>,
    release: tokio::sync::watch::Receiver<bool>,
}

struct HeldDispatch {
    gate: Arc<DispatchGate>,
    arrived: tokio::sync::mpsc::UnboundedReceiver<()>,
    released: tokio::sync::watch::Sender<bool>,
}
impl HeldDispatch {
    fn new() -> Self {
        let (arrived, arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (released, release) = tokio::sync::watch::channel(false);
        Self {
            gate: Arc::new(DispatchGate {
                armed: AtomicBool::new(false),
                arrived,
                release,
            }),
            arrived: arrivals,
            released,
        }
    }
    /// Stops the Dispatcher at the next committed `context/configured` or
    /// `prompt/changed`.
    fn arm(&self) {
        self.gate.armed.store(true, Ordering::SeqCst);
    }
    /// Resolves when the Dispatcher is stopped.
    async fn arrived(&mut self) {
        let _ = self.arrived.recv().await;
    }
    fn release(&self) {
        self.released.send_replace(true);
    }
}

struct HoldDispatchPart {
    dispatcher: Key<Dispatcher>,
    gate: Arc<DispatchGate>,
}
impl DynPlugin for HoldDispatchPart {
    fn name(&self) -> &str {
        "HoldDispatch"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let gate = self.gate.clone();
            dispatcher.subscribe(
                ctx,
                "HoldDispatch",
                &["context/configured", "prompt/changed"],
                move |_| {
                    let gate = gate.clone();
                    Box::pin(async move {
                        if gate.armed.swap(false, Ordering::SeqCst) {
                            let _ = gate.arrived.send(());
                            let mut release = gate.release.clone();
                            while !*release.borrow_and_update() {
                                if release.changed().await.is_err() {
                                    break;
                                }
                            }
                        }
                        Ok(())
                    })
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

fn hold_dispatch(
    gate: Arc<DispatchGate>,
) -> impl FnOnce(&mut AgentBuilder) + Send + Sync + 'static {
    move |builder| {
        builder.require_feature(Feature::new("HoldDispatch").branch(
            "HoldDispatch",
            move |names| {
                Ok(Box::new(HoldDispatchPart {
                    dispatcher: names.key()?,
                    gate: gate.clone(),
                }))
            },
        ));
    }
}

/// The owner can apply a later adoption while a read is in flight. The read
/// must then answer that adoption at that adoption's own cutoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_read_answers_the_adoption_it_reports_as_its_cutoff() {
    let probe = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let (server, agent) = launch_with(
        Arc::new(MockTools),
        ServerConfig::default(),
        timeline_probe(probe.clone()),
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;

    // The read stops after it has captured its cutoff and before it takes the
    // owner's answer, so a later adoption lands in that window.
    let mut held = dim_agent_capabilities::prompt::testing::hold_read(&id, "main");
    let mut reading = Client::new_version(&server, false, 3).await;
    let reading_id = id.clone();
    let reading = tokio::spawn(async move {
        let reply = read(&mut reading, &reading_id, "main", None).await;
        (reading, reply)
    });
    held.arrived().await;
    let timeline = probe
        .lock()
        .unwrap()
        .get("main")
        .cloned()
        .expect("the root's Timeline");
    let adopted = timeline
        .append_batch_registered(
            vec![EventKind::ContextConfigured(configured("beta")).into()],
            |_| Ok(()),
        )
        .unwrap();
    let cutoff = adopted.last().unwrap().sequence;
    // Let the owner apply the adoption, so the read meets it whatever it does
    // next instead of racing the Dispatcher.
    let mut snapshot = session.on::<PromptSnapshot>("main").unwrap().unwrap();
    while snapshot
        .borrow()
        .configuration
        .as_ref()
        .is_none_or(|data| data.model_id != "beta")
    {
        snapshot.changed().await.unwrap();
    }
    held.release();

    let (_reading, reply) = reading.await.unwrap();
    assert_eq!(model_of(&reply), "beta");
    assert_eq!(
        through_of(&reply),
        cutoff,
        "the cutoff is the one the answer is read at"
    );
    let pinned = read(&mut client, &id, "main", Some(cutoff)).await;
    assert_eq!(model_of(&pinned), "beta");

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// An adoption can be committed before the Dispatcher reaches it. The read
/// must wait for it and answer it at its own cutoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_read_waits_for_a_committed_adoption_the_dispatcher_has_not_reached() {
    let probe = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let watched = probe.clone();
    let mut dispatch = HeldDispatch::new();
    let gate = dispatch.gate.clone();
    let (server, agent) = launch_with(
        Arc::new(MockTools),
        ServerConfig::default(),
        move |builder| {
            timeline_probe(watched.clone())(builder);
            hold_dispatch(gate.clone())(builder);
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;

    let timeline = probe
        .lock()
        .unwrap()
        .get("main")
        .cloned()
        .expect("the root's Timeline");
    let mut held = dim_agent_capabilities::prompt::testing::hold_read(&id, "main");
    dispatch.arm();
    let committed = timeline
        .append_batch_registered(
            vec![
                EventKind::ContextConfigured(configured("hot-1")).into(),
                EventKind::ContextConfigured(configured("hot-2")).into(),
            ],
            |_| Ok(()),
        )
        .unwrap();
    let cutoff = committed.last().unwrap().sequence;
    dispatch.arrived().await;

    let mut reading = Client::new_version(&server, false, 3).await;
    let reading_id = id.clone();
    let mut reading = tokio::spawn(async move {
        let reply = read(&mut reading, &reading_id, "main", None).await;
        (reading, reply)
    });
    held.arrived().await;
    held.release();
    let reply = tokio::select! {
        biased;
        replied = &mut reading => replied.unwrap().1,
        _ = held.waiting() => {
            dispatch.release();
            reading.await.unwrap().1
        }
    };
    dispatch.release();

    assert_eq!(
        model_of(&reply),
        "hot-2",
        "the committed adoption is read even though the Dispatcher has not reached it"
    );
    let through = through_of(&reply);
    assert!(
        through >= cutoff,
        "the committed adoption is inside the reported cutoff: {through} >= {cutoff}"
    );
    let pinned = read(&mut client, &id, "main", Some(cutoff)).await;
    assert_eq!(model_of(&pinned), "hot-2");
    let pinned = read(&mut client, &id, "main", Some(through)).await;
    assert_eq!(
        model_of(&pinned),
        "hot-2",
        "reading at the reported cutoff answers the reported configuration"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A line's scope closes before its parent records the end. The read of that
/// closing line must answer its own facts instead of a reload error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_line_answers_its_adopted_configuration() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;

    let mut held = dim_agent_capabilities::timeline_tree::testing::hold_end_record(&id, &child);
    let mut ending = Client::new_version(&server, false, 3).await;
    let ending_id = id.clone();
    let ending_child = child.clone();
    let ending = tokio::spawn(async move {
        let reply = ending
            .rpc(
                "session.branch_end",
                json!({"sessionId":ending_id,"branchId":ending_child}),
            )
            .await;
        (ending, reply)
    });
    held.arrived().await;

    let line = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&line), "beta");
    let through = through_of(&line);
    let pinned = read(&mut client, &id, &child, Some(through)).await;
    assert_eq!(model_of(&pinned), "beta");
    assert_eq!(through_of(&pinned), through);

    held.release();
    let (_ending, ended) = ending.await.unwrap();
    assert_ok(&ended);
    let after = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&after), "beta");

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A closing line commits its interrupt before the store publishes it. The read
/// of that line must answer at a published cutoff instead of passing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_line_answers_at_its_published_cutoff() {
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::path::PathBuf::from("/tmp")
        .join(format!("dim-model-get-published-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-model-get-published-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;

    // The store commits the close's interrupt, then this holds its publication:
    // the line is closing, its Prompt is still mounted, and the interrupt is a
    // sequence no read may pass yet.
    let mut held =
        dim_app_server::storage::timeline_testing::hold_publication(&id, &child, "stop/interrupt");
    let mut ending = Client::new_version(&server, false, 3).await;
    let ending_id = id.clone();
    let ending_child = child.clone();
    let ending = tokio::spawn(async move {
        let reply = ending
            .rpc(
                "session.branch_end",
                json!({"sessionId":ending_id,"branchId":ending_child}),
            )
            .await;
        (ending, reply)
    });
    held.arrived().await;

    let line = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&line), "beta");
    let through = through_of(&line);
    let pinned = read(&mut client, &id, &child, Some(through)).await;
    assert_eq!(model_of(&pinned), "beta");
    assert_eq!(through_of(&pinned), through);

    held.release();
    let (_ending, ended) = ending.await.unwrap();
    assert_ok(&ended);
    let after = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&after), "beta");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&directory);
}

/// A configuration read that has passed its phase check must not become a
/// writer when its line starts closing before it advances: it answers the
/// line's published facts and commits nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_whose_line_starts_closing_before_it_advances_commits_nothing() {
    let probe = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let mut dispatch = HeldDispatch::new();
    let gate = dispatch.gate.clone();
    let (server, directory) =
        launch_with_sqlite(probe.clone(), move |builder| hold_dispatch(gate)(builder)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;
    let timeline = probe
        .lock()
        .unwrap()
        .get(&child)
        .cloned()
        .expect("the line's Timeline");

    // A change is committed and published while the Dispatcher is stopped
    // before it, so the owner's projection still ends below it and a replay
    // would have to write a checkpoint.
    dispatch.arm();
    pending_prompt_change(&timeline, &child);
    tokio::time::timeout(std::time::Duration::from_secs(10), dispatch.arrived())
        .await
        .expect("the Dispatcher stopped at the change");

    // The read stops after its phase check passed and before it advances.
    let mut held = dim_agent_capabilities::prompt::testing::hold_read_after_check(&id, &child);
    let mut reading = Client::new_version(&server, false, 3).await;
    let reading_id = id.clone();
    let reading_child = child.clone();
    let reading = tokio::spawn(async move {
        let reply = read(&mut reading, &reading_id, &reading_child, None).await;
        (reading, reply)
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), held.arrived())
        .await
        .expect("the read stopped after its phase check");

    // The close runs until its own interrupt is committed and its publication
    // is held: the line's phase is closing and its Timeline has not closed.
    let mut held_interrupt =
        dim_app_server::storage::timeline_testing::hold_publication(&id, &child, "stop/interrupt");
    let closing_session = session.clone();
    let closing = tokio::spawn(async move { closing_session.close().await });
    tokio::time::timeout(std::time::Duration::from_secs(10), held_interrupt.arrived())
        .await
        .expect("the close committed its interrupt");
    let before = timeline.committed();

    held.release();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(10), reading)
        .await
        .expect("the read answered its closing line")
        .unwrap()
        .1;
    assert_eq!(model_of(&reply), "beta");
    let through = through_of(&reply);
    let pinned = read(&mut client, &id, &child, Some(through)).await;
    assert_eq!(model_of(&pinned), "beta");
    assert_eq!(through_of(&pinned), through);
    assert_eq!(
        timeline.committed(),
        before,
        "the read committed nothing after its line started closing"
    );

    dispatch.release();
    held_interrupt.release();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), closing)
        .await
        .expect("the session closed")
        .unwrap();
    assert!(closed.is_ok(), "{closed:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&directory);
}

/// A closing line answers from its published facts without waiting for the
/// recovery checkpoint the owner writes when it consumes a change. The close
/// is stopped after its own end is committed and before its Timeline closes;
/// the change lands above that end, so the owner applies it and writes the
/// checkpoint the hold stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closing_line_answers_without_waiting_for_its_recovery_checkpoint() {
    let probe = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let (server, directory) = launch_with_sqlite(probe.clone(), |_| {}).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;
    let timeline = probe
        .lock()
        .unwrap()
        .get(&child)
        .cloned()
        .expect("the line's Timeline");

    let mut held_recovered = dim_app_server::storage::timeline_testing::hold_publication(
        &id,
        &child,
        "prompt-state/recovered",
    );
    let mut held_close = dim_agent_runtime::testing::hold_close(&id, &child);
    let ending_session = session.clone();
    let ending_child = child.clone();
    let ending = tokio::spawn(async move {
        ending_session
            .end_branch(
                &ending_child,
                dim_agent_types::BranchSource::Host {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                },
            )
            .await
    });

    // The close is stopped after the line's own end is committed and before
    // its Timeline closes: the line is closing, its head is live, and it still
    // accepts appends. The change lands above that end, so the owner applies
    // it and writes the checkpoint the hold stops.
    tokio::time::timeout(std::time::Duration::from_secs(10), held_close.arrived())
        .await
        .expect("the close stopped after the line's end was committed");
    pending_prompt_change(&timeline, &child);
    tokio::time::timeout(std::time::Duration::from_secs(10), held_recovered.arrived())
        .await
        .expect("the owner wrote its recovery checkpoint while the line was closing");

    let line = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read(&mut client, &id, &child, None),
    )
    .await
    .expect("the closing read answered without waiting for the held checkpoint");
    assert_eq!(model_of(&line), "beta");
    let through = through_of(&line);
    let pinned = read(&mut client, &id, &child, Some(through)).await;
    assert_eq!(model_of(&pinned), "beta");
    assert_eq!(through_of(&pinned), through);

    held_recovered.release();
    held_close.release();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(10), ending)
        .await
        .expect("the line ended")
        .unwrap();
    assert!(ended.is_ok(), "{ended:?}");
    let after = read(&mut client, &id, &child, None).await;
    assert_eq!(model_of(&after), "beta");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&directory);
}

/// A live read that waits for a recovery result stops waiting when its line
/// starts closing, and answers from the tree instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_read_ends_when_its_line_starts_closing() {
    let probe = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let (server, directory) = launch_with_sqlite(probe.clone(), |_| {}).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;
    let child = open_line(&mut client, &id, "main", "worker work").await;
    session.idle(&child).await.unwrap();
    adopt(&session, &child, "beta").await;

    let timeline = probe
        .lock()
        .unwrap()
        .get(&child)
        .cloned()
        .expect("the line's Timeline");
    let mut held = dim_agent_capabilities::prompt::testing::hold_read(&id, &child);
    let mut reading = Client::new_version(&server, false, 3).await;
    let reading_id = id.clone();
    let reading_child = child.clone();
    let reading = tokio::spawn(async move {
        let reply = read(&mut reading, &reading_id, &reading_child, None).await;
        (reading, reply)
    });
    held.arrived().await;

    // The read has captured its cutoff. The change lands above it; the owner
    // applies the change and writes a checkpoint whose publication is held, so
    // the read has to wait for a write it did not capture.
    let mut held_recovered = dim_app_server::storage::timeline_testing::hold_publication(
        &id,
        &child,
        "prompt-state/recovered",
    );
    pending_prompt_change(&timeline, &child);
    held_recovered.arrived().await;

    held.release();
    held.publishing().await;

    let closing_session = session.clone();
    let mut closing = tokio::spawn(async move { closing_session.close().await });
    let (_reading, reply) = tokio::select! {
        replied = reading => replied.unwrap(),
        closed = &mut closing => {
            panic!("the session closed before the read answered: {closed:?}");
        }
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
            panic!("the live read did not end when its line started closing");
        }
    };
    assert_eq!(model_of(&reply), "beta");
    let through = through_of(&reply);
    let pinned = read(&mut client, &id, &child, Some(through)).await;
    assert_eq!(model_of(&pinned), "beta");
    assert_eq!(through_of(&pinned), through);

    held_recovered.release();
    let closed = closing.await.unwrap();
    assert!(closed.is_ok(), "{closed:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&directory);
}

/// A reload of a line's owner is not a closing line: the read keeps the reload
/// error instead of answering from the tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloading_active_line_keeps_its_error() {
    let tools = ToolReload::default();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(Arc::new(
        dim_agent_runtime::MemoryTimelineStore::default(),
    )));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    tools.install(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-model-get-reload-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    adopt(&session, "main", "alpha").await;

    // Hold the reloaded provider's start, so the line stays unloaded until a
    // read has observed it.
    let gate = tools.hold_next();
    let restart = tokio::spawn({
        let tools = tools.clone();
        async move { tools.restart().await }
    });
    let mut reloading = None;
    for _ in 0..64 {
        let reply = read(&mut client, &id, "main", None).await;
        if let Some(error) = reply.get("error") {
            reloading = Some(error.clone());
            break;
        }
    }
    let reloading = reloading.expect("a read observed the reload");
    assert_eq!(
        reloading["data"]["appError"], "service_reloading",
        "{reloading}"
    );
    gate.notify_one();
    restart.await.unwrap();

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}
