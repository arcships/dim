use crate::local_support;

use dim_agent_capabilities::environment_clients::SessionClientsHost;
use dim_agent_capabilities::environment_clients::{BRANCH_PART, CLIENTS};
use dim_agent_capabilities::environment_clients::{
    ClientChannel, ClientPresence, ClientsChanged, SessionPresences, clients_changes,
};
use dim_agent_capabilities::execution_settings::prompt::{
    BRANCH_PART as SETTINGS_BRANCH_PART, SESSION_PART,
};
use dim_agent_capabilities::execution_settings::{
    ExecutionSettings, ExecutionSettingsHost, prompt::SNAPSHOT_SOURCE,
};
use dim_agent_capabilities::host_platform::HostPlatform;
use dim_agent_runtime::Session;
use dim_agent_runtime::SessionConfig;
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, MemoryTimelineStore, Names, PartRegistry, PublishCallback,
    QueryPage, QueryResult, QuerySpec, StoredBranch, StoredEvent, TimelineKey, TimelineReadScope,
    TimelineStore, TimelineStoreBinding, TimelineTrace,
};
use dim_agent_runtime::{BranchOptions, testing::branch_events};
use dim_agent_runtime::{QueryFilter, QueryOperand};
use dim_agent_types::{BranchSource, EventKind, InputBlock};
use local_support::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};
use rutis::host::{CtxHost, FiberState, InstanceId};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// A mutable presence source: the test changes the connections, as another
/// client would, and announces the change on the process bus.
struct Presence(Mutex<Vec<ClientPresence>>);
impl Presence {
    fn set(&self, presences: Vec<ClientPresence>) {
        *self.0.lock().unwrap() = presences;
    }
    fn presences(&self) -> Vec<ClientPresence> {
        self.0.lock().unwrap().clone()
    }
    fn host(self: &Arc<Self>) -> SessionPresences {
        let presence = self.clone();
        Arc::new(move |_| presence.presences())
    }
}

fn connection(channel: ClientChannel, label: Option<&str>) -> ClientPresence {
    ClientPresence {
        channel,
        label: label.map(str::to_owned),
    }
}

fn changed(env: &Env, session: &Session) {
    env.root
        .emit(ClientsChanged {
            name: clients_changes(session.id()),
            session_id: session.id().to_owned(),
        })
        .unwrap();
}

async fn until_block(session: &Session, needle: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = inputs(session)
                .await
                .iter()
                .flat_map(|input| &input.blocks)
                .any(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    text.contains(needle)
                });
            if found {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("presence was appended");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_is_appended_and_unchanged_counts_do_not_duplicate() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(Vec::new())));
    presence.set(vec![connection(ClientChannel::Tui, Some("eric@laptop"))]);
    let env = env(
        &dirs,
        Options {
            clients: Some(presence.host()),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();

    run(&session, "hello").await;
    until_block(&session, "1 client connection attached:").await;
    run(&session, "hello again").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(text.contains("1 client connection attached:"), "{text}");
    assert!(text.contains("- tui \"eric@laptop\" (claimed)"), "{text}");

    presence.set(vec![
        connection(ClientChannel::Tui, Some("eric@laptop")),
        connection(ClientChannel::Tui, None),
        connection(ClientChannel::Web, Some("alice")),
    ]);
    changed(&env, &session);
    run(&session, "again").await;
    until_block(&session, "3 client connections attached:").await;
    run(&session, "third").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(text.contains("- tui \"eric@laptop\" (claimed)"), "{text}");
    assert!(text.contains("- tui\n"), "{text}");
    assert!(text.contains("- web \"alice\" (claimed)"), "{text}");
    assert_eq!(
        text.matches("3 client connections attached:").count(),
        1,
        "an unchanged presence must not write another block:\n{text}"
    );

    presence.set(Vec::new());
    changed(&env, &session);
    run(&session, "done").await;
    until_block(&session, "No client connection is attached.").await;
    run(&session, "later").await;
    let text = request_text(requests(&session).await.last().unwrap());
    assert!(
        text.contains(
            "<dim-state source=\"dim/environment/clients\">No client connection is attached.</dim-state>"
        ),
        "{text}"
    );

    env.agent.close().await.unwrap();
}

/// The instances of the branches that have a presence part, so a flow can
/// tell the line it opens from the lines that were already there.
fn presence_instances(root: &rutis::Ctx) -> HashSet<InstanceId> {
    root.diagnostics()
        .plugins
        .iter()
        .filter(|plugin| plugin.name == BRANCH_PART)
        .map(|plugin| plugin.instance)
        .collect()
}

/// Waits until a presence part is Active in an instance the line did not have
/// before it opened.
async fn until_presence_part_of_new_instance(
    root: &rutis::Ctx,
    known: &HashSet<InstanceId>,
) -> rutis::host::FiberView {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(plugin) = root.diagnostics().plugins.iter().find(|plugin| {
                plugin.name == BRANCH_PART
                    && plugin.state == FiberState::Active
                    && !known.contains(&plugin.instance)
            }) {
                return root.view(plugin.id).expect("the presence part's view");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the line's presence part mounts while the line loads")
}

async fn until_active(root: &rutis::Ctx, id: rutis::host::PluginId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let active = root
                .diagnostics()
                .plugins
                .iter()
                .any(|plugin| plugin.id == id && plugin.state == FiberState::Active);
            if active {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the replacement part becomes Active while the line loads");
}

/// Waits for the line's presence snapshot and the queue's `input/applied` for
/// it, then asserts the line got exactly one snapshot and that it follows the
/// line's identity input.
async fn one_presence_snapshot_after_the_identity_input(session: &Session, child: &str) {
    let marker = format!("<dim-state source=\"{CLIENTS}\">");
    let is_presence = |event: &dim_agent_types::AgentEvent| {
        matches!(&event.kind, EventKind::InputMessage(data)
            if data.user.is_none()
                && data.blocks.iter().any(|block| matches!(block,
                    InputBlock::Raw { text, .. } if text.starts_with(&marker))))
    };
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = branch_events(session, child, ..).await.unwrap();
            let applied = events
                .iter()
                .filter(|event| is_presence(event))
                .any(|snapshot| {
                    events.iter().any(|event| {
                        matches!(&event.kind,
                        EventKind::InputApplied(data) if data.input_id == snapshot.id)
                    })
                });
            if applied {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the line publishes its presence snapshot and its input/applied");
    let identity_marker =
        format!(r#"<dim-branch-line branch="{child}" parent="main" source="host">"#);
    let identity = events
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
                if data.blocks.first().is_some_and(|block| matches!(block,
                    InputBlock::Raw { text, .. } if text.starts_with(&identity_marker))))
        })
        .expect("the line's identity input")
        .sequence;
    let snapshots: Vec<_> = events.iter().filter(|event| is_presence(event)).collect();
    for snapshot in &snapshots {
        assert!(
            snapshot.sequence > identity,
            "a presence snapshot precedes the identity input: {} before {identity} ({} snapshots)",
            snapshot.sequence,
            snapshots.len()
        );
    }
    assert_eq!(
        snapshots.len(),
        1,
        "the line gets its presence snapshot exactly once"
    );
}

/// Replacing the presence part while a line still loads keeps the line's
/// identity input first: the retired receipt writes nothing, and the
/// replacement appends the line's one presence snapshot from its first pass
/// after activation, which publishes before the branch dispatches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_part_during_loading_writes_one_presence_snapshot_after_the_identity_input() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(Vec::new())));
    let hold = Arc::new(BranchHold::default());
    let env = env(
        &dirs,
        Options {
            clients: Some(presence.host()),
            branch_hold: Some(hold.clone()),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();
    let known = presence_instances(&env.root);
    let at = session.sequence("main").unwrap();

    hold.arm();
    let opening = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .branch(
                    "main".into(),
                    at,
                    BranchOptions {
                        prompt: "line work".into(),
                        source: BranchSource::Host {
                            operation_id: "part-replaced".into(),
                        },
                    },
                )
                .await
        }
    });
    hold.entered.notified().await;
    let part = until_presence_part_of_new_instance(&env.root, &known).await;
    part.restart().await.unwrap();
    until_active(&env.root, part.id).await;
    hold.release.notify_one();
    let child = opening.await.unwrap().expect("the line opens");

    one_presence_snapshot_after_the_identity_input(&session, &child).await;

    env.agent.close().await.unwrap();
}

/// A part that mounts after the line's `branch/start` was dispatched decides
/// from the line's own opening fact, so its receipt writes nothing ahead of
/// the identity input, which the tree appends after the opening seals: the
/// published line appends the one snapshot once it activates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_part_mounted_after_the_opening_event_dispatched_writes_after_the_identity_input() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(Vec::new())));
    let hold = Arc::new(OpeningDispatchHold::default());
    let env = env(
        &dirs,
        Options {
            clients: Some(presence.host()),
            opening_dispatch_hold: Some(hold.clone()),
            ..Options::default()
        },
    )
    .await;
    let session = env.session(&cwd, "").await.unwrap();
    let known = presence_instances(&env.root);
    let at = session.sequence("main").unwrap();

    hold.arm();
    let opening = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .branch(
                    "main".into(),
                    at,
                    BranchOptions {
                        prompt: "line work".into(),
                        source: BranchSource::Host {
                            operation_id: "opening-dispatched".into(),
                        },
                    },
                )
                .await
        }
    });
    hold.entered.notified().await;
    let part = until_presence_part_of_new_instance(&env.root, &known).await;
    part.restart().await.unwrap();
    until_active(&env.root, part.id).await;
    hold.release.notify_one();
    let child = opening.await.unwrap().expect("the line opens");

    one_presence_snapshot_after_the_identity_input(&session, &child).await;

    env.agent.close().await.unwrap();
}

/// The in-memory store that holds the publication of every environment
/// snapshot it accepts until the test releases them: a line's opening can read
/// its opening fact while a batch it just appended is committed and still
/// unpublished.
struct SnapshotHoldStore {
    memory: MemoryTimelineStore,
    /// `Some` while the hold is on: the batches whose publications wait.
    /// `None` once the test released them.
    held: Mutex<Option<Vec<PublishCallback>>>,
    /// Notified when a snapshot batch is held.
    entered: Notify,
}
impl Default for SnapshotHoldStore {
    fn default() -> Self {
        Self {
            memory: MemoryTimelineStore::default(),
            held: Mutex::new(Some(Vec::new())),
            entered: Notify::new(),
        }
    }
}

/// Whether the batch is an environment snapshot input: one of the
/// `<dim-state>` blocks the environment parts append at a line's opening.
fn environment_snapshot(events: &[StoredEvent]) -> bool {
    events.iter().any(|record| {
        matches!(&record.event.kind, EventKind::InputMessage(data)
            if data.user.is_none()
                && data.blocks.first().is_some_and(|block| matches!(block,
                    InputBlock::Raw { text, .. } if text.starts_with("<dim-state source=\""))))
    })
}

impl SnapshotHoldStore {
    /// Publishes the held snapshots in commit order, ends the hold and returns
    /// how many batches it held.
    fn release(&self) -> usize {
        let held = self.held.lock().unwrap().take().unwrap_or_default();
        let count = held.len();
        for publish in held {
            publish(Ok(()));
        }
        count
    }
}

impl TimelineStore for SnapshotHoldStore {
    fn query<'a>(
        &'a self,
        scope: &'a TimelineReadScope,
        query: &'a QuerySpec,
        page: Option<&'a QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<QueryResult>> {
        self.memory.query(scope, query, page)
    }

    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Option<SessionConfig>>> {
        self.memory.session_config(id)
    }
    fn branches<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Vec<StoredBranch>>> {
        self.memory.branches(id)
    }
    fn head<'a>(
        &'a self,
        key: &'a TimelineKey,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.memory.head(key)
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        if !environment_snapshot(&events) {
            return self.memory.append(key, events, publish);
        }
        let mut hold = self.held.lock().unwrap();
        let Some(held) = hold.as_mut() else {
            drop(hold);
            return self.memory.append(key, events, publish);
        };
        self.memory.append(key, events, Box::new(|_| ()))?;
        held.push(publish);
        drop(hold);
        self.entered.notify_one();
        Ok(())
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
}

/// Registers the two environment branches this flow opens with: client
/// presence and execution settings.
struct OpeningPartsPlugin {
    parts: Key<PartRegistry>,
}
impl OpeningPartsPlugin {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            parts: names.key()?,
        })
    }
}
impl DynPlugin for OpeningPartsPlugin {
    fn name(&self) -> &str {
        "OpeningParts"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let parts = ctx.require_at(&self.parts)?;
            dim_agent_capabilities::environment_clients::register_parts(ctx, &parts)?;
            dim_agent_capabilities::execution_settings::prompt::register_parts(ctx, &parts)?;
            Ok(Effect::Done)
        })
    }
}

/// The default profile plus the two environment branches the opening collects,
/// with `store` as the Session's storage and `budget` as the opening budget
/// (`None` keeps the default): the flow supplies a store of its own and leaves
/// the shared fixture's alone.
async fn env_with_store(
    dirs: &Dirs,
    store: Arc<dyn TimelineStore>,
    budget: Option<Duration>,
    presence: SessionPresences,
    execution: Arc<ExecutionSettingsHost>,
) -> MinimalHost {
    let platform = platform(&dirs.home).with_execution_settings((*execution).clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder
        .timeline_store(TimelineStoreBinding::new(store))
        .plugin(|names| Ok(Box::new(OpeningPartsPlugin::new(names)?)));
    if let Some(budget) = budget {
        builder.startup_timeout(budget);
    }
    builder
        .require_part(BRANCH_PART)
        .require_part(SETTINGS_BRANCH_PART)
        .require_part(SESSION_PART);
    MinimalHost::start_with(&builder, move |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        root.provide_as::<SessionClientsHost>(Arc::new(SessionClientsHost::new(presence)))?;
        root.provide_as::<ExecutionSettingsHost>(execution)?;
        Ok(())
    })
    .await
    .unwrap()
}

/// Both environment receipts append their snapshot while the opening still
/// collects them: the store holds every snapshot at publication, so a batch the
/// line committed and has not published yet does not keep the second receipt
/// out of the opening. Otherwise the receipt that appends after the other
/// part's snapshot leaves its own to the first pass after activation. The flow
/// runs on a current-thread runtime, so the release below runs only once the
/// Session's build task yielded, i.e. after both receipts appended; on a
/// multi-threaded runtime it could overtake the second append.
#[tokio::test]
async fn a_line_opens_with_both_snapshots_while_a_later_batch_is_unpublished() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(vec![connection(
        ClientChannel::Tui,
        Some("eric@laptop"),
    )])));
    let store = Arc::new(SnapshotHoldStore::default());
    let execution = ExecutionSettingsHost::new(Arc::new(|_| {
        Box::pin(async { Ok(ExecutionSettings::default()) })
    }));
    let host = env_with_store(
        &dirs,
        store.clone(),
        None,
        presence.host(),
        Arc::new(execution),
    )
    .await;
    let opener = host.agent().clone();
    let creation = opener.create_session(SessionConfig {
        cwd: cwd.to_string_lossy().into_owned(),
        ..SessionConfig::default()
    });
    tokio::pin!(creation);
    let mut held = None;
    let session = tokio::select! {
        result = &mut creation => result.expect("the Session opens"),
        count = async {
            store.entered.notified().await;
            store.release()
        } => {
            held = Some(count);
            creation.await.unwrap()
        }
    };
    assert_eq!(
        held,
        Some(2),
        "both opening receipts append their snapshot before the opening seals"
    );
    let events = branch_events(&session, "main", ..).await.unwrap();
    for (source, label) in [
        (CLIENTS, "the client presence snapshot"),
        (SNAPSHOT_SOURCE, "the execution settings snapshot"),
    ] {
        let marker = format!("<dim-state source=\"{source}\">");
        let snapshots: Vec<_> = events
            .iter()
            .filter(|event| {
                matches!(&event.kind, EventKind::InputMessage(data)
                    if data.user.is_none()
                        && data.blocks.iter().any(|block| matches!(block,
                            InputBlock::Raw { text, .. } if text.starts_with(&marker))))
            })
            .collect();
        assert_eq!(
            snapshots.len(),
            1,
            "{label} is published once: {:?}",
            events
                .iter()
                .map(|event| event.kind.key())
                .collect::<Vec<_>>()
        );
    }

    host.close().await.unwrap();
}

/// The Session's store, which can hold the query for a line's opening fact: the
/// read a receipt used to make to learn whether its line opens with an identity
/// input. `reads` counts every such query whether or not it is held, so a flow
/// pins that the receipts decide without one.
struct OpeningFactHoldStore {
    memory: MemoryTimelineStore,
    /// Set once the flow wants a line's opening-fact read held.
    armed: AtomicBool,
    reads: AtomicUsize,
}
impl Default for OpeningFactHoldStore {
    fn default() -> Self {
        Self {
            memory: MemoryTimelineStore::default(),
            armed: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        }
    }
}
impl OpeningFactHoldStore {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

/// Whether the query asks a line's Timeline for its opening fact: the last
/// `branch/start` or `branch/resume` it has.
fn opening_fact_query(query: &QuerySpec) -> bool {
    matches!(
        &query.filter,
        Some(QueryFilter::In { value: QueryOperand::Field { path }, values })
            if path == "kind" && values.as_slice() == ["branch/start", "branch/resume"]
    )
}

impl TimelineStore for OpeningFactHoldStore {
    fn query<'a>(
        &'a self,
        scope: &'a TimelineReadScope,
        query: &'a QuerySpec,
        page: Option<&'a QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<QueryResult>> {
        if !opening_fact_query(query) {
            return self.memory.query(scope, query, page);
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        if !self.armed.load(Ordering::SeqCst) {
            return self.memory.query(scope, query, page);
        }
        // A held read never answers: only the opening's own budget ends it.
        Box::pin(std::future::pending())
    }

    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Option<SessionConfig>>> {
        self.memory.session_config(id)
    }
    fn branches<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Vec<StoredBranch>>> {
        self.memory.branches(id)
    }
    fn head<'a>(
        &'a self,
        key: &'a TimelineKey,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.memory.head(key)
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        self.memory.append(key, events, publish)
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
}

/// The `branch`'s `<dim-state>` snapshots of `source`, oldest first.
async fn state_snapshots(session: &Session, branch: &str, source: &str) -> Vec<u64> {
    let marker = format!("<dim-state source=\"{source}\">");
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(&event.kind, EventKind::InputMessage(data)
                if data.user.is_none()
                    && data.blocks.first().is_some_and(|block| matches!(block,
                        InputBlock::Raw { text, .. } if text.starts_with(&marker))))
        })
        .map(|event| event.sequence)
        .collect()
}

/// A receipt decides from the opening kind its line's owner wrote, so a stalled
/// read of the line's Timeline cannot hold the opening: under a short budget a
/// held opening-fact read still lets a child line open, the root line keeps both
/// of its opening snapshots, and the child's identity input stays first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_opening_fact_read_does_not_hold_a_line_open() {
    let dirs = Dirs::new();
    let cwd = dirs.dir("work");
    let presence = Arc::new(Presence(Mutex::new(vec![connection(
        ClientChannel::Tui,
        Some("eric@laptop"),
    )])));
    let store = Arc::new(OpeningFactHoldStore::default());
    let execution = ExecutionSettingsHost::new(Arc::new(|_| {
        Box::pin(async { Ok(ExecutionSettings::default()) })
    }));
    let host = env_with_store(
        &dirs,
        store.clone(),
        Some(Duration::from_millis(500)),
        presence.host(),
        Arc::new(execution),
    )
    .await;
    let session = host
        .agent()
        .clone()
        .create_session(SessionConfig {
            cwd: cwd.to_string_lossy().into_owned(),
            ..SessionConfig::default()
        })
        .await
        .expect("the Session opens inside its shorter opening budget");
    for (source, label) in [
        (CLIENTS, "the client presence snapshot"),
        (SNAPSHOT_SOURCE, "the execution settings snapshot"),
    ] {
        assert_eq!(
            state_snapshots(&session, "main", source).await.len(),
            1,
            "{label} is in the root line's opening"
        );
    }

    store.arm();
    let at = session.sequence("main").unwrap();
    let opened = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "line work".into(),
                source: BranchSource::Host {
                    operation_id: "held-opening-fact".into(),
                },
            },
        )
        .await;
    assert!(
        opened.is_ok(),
        "the line opens inside a short budget while only its opening-fact read is held: {opened:?}"
    );
    let child = opened.unwrap();

    one_presence_snapshot_after_the_identity_input(&session, &child).await;
    assert_eq!(
        store.reads(),
        0,
        "the opening receipts decide from the owner's kind without reading a line's Timeline"
    );

    host.close().await.unwrap();
}
