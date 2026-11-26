use dim_agent_capabilities::defaults;
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, BoxFuture, CreateSessionOptions, MemoryTimelineStore,
    PublishCallback, QueryPage, QueryResult, QuerySpec, RestoreSessionOptions, Result,
    SessionConfig, StoredBranch, StoredEvent, TimelineKey, TimelineReadScope, TimelineStore,
    TimelineStoreBinding, TimelineTrace,
};
use rutis::host::DynPlugin;
use rutis::{Ctx, Plugin};
use std::{
    collections::BTreeMap,
    ops::RangeInclusive,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

const GROUP: &str = "options-probe";

struct Probe {
    value: u32,
    seen: Arc<AtomicU32>,
}
impl Plugin for Probe {
    type Deps = ();
    fn name(&self) -> &str {
        "options-probe"
    }
    async fn apply(&self, _ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        self.seen.store(self.value, Ordering::SeqCst);
        Ok(())
    }
}

fn installed() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder
}

fn builder(store: &Arc<MemoryTimelineStore>, seen: Arc<AtomicU32>) -> AgentBuilder {
    let mut builder = installed();
    builder
        .timeline_store(TimelineStoreBinding::new(store.clone()))
        .session_extension_part::<u32>(GROUP, "options-probe", move |_, value: &u32| {
            Ok(Box::new(Probe {
                value: *value,
                seen: seen.clone(),
            }) as Box<dyn DynPlugin>)
        });
    builder
}

/// A store two Agents can share. `release_session` drops the admission state
/// the product store keys by Session id, so a creation that releases a Session
/// it never admitted is visible as lost state.
#[derive(Default)]
struct Shared {
    memory: MemoryTimelineStore,
    created: Mutex<Vec<String>>,
    released: Mutex<Vec<String>>,
    failures: Mutex<BTreeMap<String, String>>,
}
impl TimelineStore for Shared {
    fn query<'a>(
        &'a self,
        scope: &'a TimelineReadScope,
        query: &'a QuerySpec,
        page: Option<&'a QueryPage>,
    ) -> BoxFuture<'a, Result<QueryResult>> {
        self.memory.query(scope, query, page)
    }
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.created.lock().unwrap().push(session_id.to_owned());
        self.memory.create_session(session_id, config)
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(session_id)
    }
    fn release_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.released.lock().unwrap().push(session_id.to_owned());
            self.failures.lock().unwrap().remove(session_id);
            self.memory.release_session(session_id).await
        })
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(session_id)
    }
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(session_id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn batch_end<'a>(&'a self, key: &'a TimelineKey, sequence: u64) -> BoxFuture<'a, Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        self.memory.append(key, events, publish)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
}

/// A host on this store, without the extension probe.
async fn store_host(store: &Arc<Shared>) -> MinimalHost {
    let mut builder = installed();
    builder.timeline_store(TimelineStoreBinding::new(store.clone()));
    MinimalHost::start(&builder).await.unwrap()
}

/// The prepare step of a host that commits the Session's rows in the store, so
/// a creation that reaches it is visible there.
fn preparing(
    store: &Arc<Shared>,
    entered: Arc<AtomicBool>,
) -> impl FnOnce(String) -> BoxFuture<'static, Result<()>> + Send + 'static {
    let store = store.clone();
    move |id| {
        entered.store(true, Ordering::SeqCst);
        let store = store.clone();
        Box::pin(async move { store.create_session(&id, &SessionConfig::default()).await })
    }
}

/// The rows the store keeps for one Session: its configuration and its branch
/// directory. `None` once neither is stored.
async fn stored_rows(store: &Shared, id: &str) -> Option<(serde_json::Value, serde_json::Value)> {
    let config = store
        .session_config(id)
        .await
        .expect("the stored configuration is readable")?;
    let branches = store
        .branches(id)
        .await
        .expect("the stored branch directory is readable");
    Some((
        serde_json::to_value(config).expect("the stored configuration serializes"),
        serde_json::to_value(branches).expect("the stored branch directory serializes"),
    ))
}

/// The error of a rejected creation; a `Session` is not `Debug`, so `unwrap_err`
/// does not apply.
fn rejected<T>(created: Result<T>) -> AgentError {
    match created {
        Ok(_) => panic!("the creation was accepted"),
        Err(error) => error,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_and_restore_options_apply_their_fields() {
    let seen = Arc::new(AtomicU32::new(0));
    let store = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&builder(&store, seen.clone()))
        .await
        .unwrap();

    let prepared = Arc::new(Mutex::new(None::<String>));
    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default()
                .prepare("named-session", {
                    let store = store.clone();
                    let prepared = prepared.clone();
                    move |id| {
                        *prepared.lock().unwrap() = Some(id.clone());
                        let config = SessionConfig::default();
                        Box::pin(async move {
                            store.create_session(&id, &config).await?;
                            Ok(())
                        })
                    }
                })
                .extension(GROUP, 7_u32),
        )
        .await
        .unwrap();
    assert_eq!(
        session.id(),
        "named-session",
        "a prepared creation is created under the id its options name"
    );
    assert_eq!(
        *prepared.lock().unwrap(),
        Some("named-session".to_owned()),
        "the prepare step commits the rows of the id its Session is created under"
    );
    assert_eq!(
        seen.load(Ordering::SeqCst),
        7,
        "the Session loaded the extension's value"
    );
    session.close().await.unwrap();

    let first = agent
        .create_session(SessionConfig::default(), CreateSessionOptions::default())
        .await
        .unwrap();
    let second = agent
        .create_session(SessionConfig::default(), CreateSessionOptions::default())
        .await
        .unwrap();
    for session in [&first, &second] {
        let id = uuid::Uuid::parse_str(session.id())
            .unwrap_or_else(|error| panic!("a plain creation is created under a UUID: {error}"));
        assert_eq!(
            id.get_version_num(),
            4,
            "a plain creation is created under a UUIDv4: {}",
            session.id()
        );
        assert_eq!(
            id.get_variant(),
            uuid::Variant::RFC4122,
            "a plain creation's id is an RFC 4122 UUID: {}",
            session.id()
        );
    }
    assert_ne!(
        first.id(),
        second.id(),
        "two plain creations are created under different ids"
    );
    first.close().await.unwrap();
    second.close().await.unwrap();

    store
        .create_session("stored", &SessionConfig::default())
        .await
        .unwrap();
    let session = agent
        .restore_session("stored".into(), RestoreSessionOptions::default())
        .await
        .unwrap();
    assert!(
        session.config().excluded_tools.is_empty(),
        "without a protocol version the stored configuration is restored as written"
    );
    session.close().await.unwrap();

    let session = agent
        .restore_session(
            "stored".into(),
            RestoreSessionOptions::default().protocol_version(1),
        )
        .await
        .unwrap();
    assert_eq!(
        session.config().excluded_tools,
        vec!["codemode".to_owned()],
        "a protocol version below 4 keeps the codemode tool out"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_id_is_admitted_only_as_one_path_component() {
    let store = Arc::new(Shared::default());
    let host = store_host(&store).await;
    let agent = host.agent().clone();
    for id in ["", ".", "..", "../escape", "nested/id", "/absolute"] {
        let entered = Arc::new(AtomicBool::new(false));
        let error = rejected(
            agent
                .create_session(
                    SessionConfig::default(),
                    CreateSessionOptions::default().prepare(id, preparing(&store, entered.clone())),
                )
                .await,
        );
        assert!(
            matches!(&error, AgentError::InvalidInput(message) if message == &format!("invalid Session id {id}")),
            "{id:?} is not one path component: {error}"
        );
        assert!(
            !entered.load(Ordering::SeqCst),
            "{id:?}: a rejected id reaches no prepare step"
        );
        assert!(
            agent.get_session(id).is_none(),
            "{id:?}: a rejected id publishes no Session"
        );
    }
    assert!(
        store.created.lock().unwrap().is_empty(),
        "a rejected id writes no rows: {:?}",
        store.created.lock().unwrap()
    );
    assert!(
        agent.sessions().is_empty(),
        "a rejected id publishes no Session"
    );

    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default().prepare(
                "explicit-session",
                preparing(&store, Arc::new(AtomicBool::new(false))),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        session.id(),
        "explicit-session",
        "a legal explicit id is preserved"
    );
    session.close().await.unwrap();
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_duplicate_keeps_the_existing_sessions_state() {
    let store = Arc::new(Shared::default());
    let first = store_host(&store).await;
    let second = store_host(&store).await;
    let session = first
        .agent()
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default().prepare(
                "shared",
                preparing(&store, Arc::new(AtomicBool::new(false))),
            ),
        )
        .await
        .unwrap();
    store
        .failures
        .lock()
        .unwrap()
        .insert("shared".into(), "disk full".into());
    let rows = stored_rows(&store, "shared").await;

    let error = rejected(
        second
            .agent()
            .create_session(
                SessionConfig::default(),
                CreateSessionOptions::default().prepare(
                    "shared",
                    preparing(&store, Arc::new(AtomicBool::new(false))),
                ),
            )
            .await,
    );
    assert!(
        matches!(&error, AgentError::Storage(message) if message == "session shared already exists"),
        "a duplicate id is a rejected creation: {error}"
    );
    assert_eq!(
        store
            .failures
            .lock()
            .unwrap()
            .get("shared")
            .map(String::as_str),
        Some("disk full"),
        "the rejected duplicate keeps the existing Session's recorded write failure"
    );
    assert!(
        store.released.lock().unwrap().is_empty(),
        "the rejected duplicate releases no Session it never admitted: {:?}",
        store.released.lock().unwrap()
    );
    assert_eq!(
        stored_rows(&store, "shared").await,
        rows,
        "the rejected duplicate leaves the existing Session's stored rows as they were"
    );
    assert!(
        first
            .agent()
            .get_session("shared")
            .is_some_and(|session| session.id() == "shared"),
        "the first Agent's Session stays loaded"
    );

    session.close().await.unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
}
