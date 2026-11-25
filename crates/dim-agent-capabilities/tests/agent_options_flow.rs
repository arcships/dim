use dim_agent_capabilities::{
    defaults,
    host_platform::{HostPlatform, ShellStatus},
    workspace_tools::{self, WorkspaceToolsConfig},
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, BoxFuture, CreateSessionOptions, MemoryTimelineStore,
    PublishCallback, QueryPage, QueryResult, QuerySpec, RestoreSessionOptions, Result,
    SessionConfig, StoredBranch, StoredEvent, TimelineKey, TimelineReadScope, TimelineStore,
    TimelineStoreBinding, TimelineTrace,
};
use rutis::host::{DynPlugin, ErrorKind};
use rutis::{Ctx, Plugin};
use std::{
    collections::BTreeMap,
    ops::RangeInclusive,
    path::Path,
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
                .id("named-session")
                .prepare({
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
        "a prepared creation is created under the id the options name"
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

    let generated = Arc::new(Mutex::new(None::<String>));
    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default().prepare({
                let store = store.clone();
                let generated = generated.clone();
                move |id| {
                    *generated.lock().unwrap() = Some(id.clone());
                    let config = SessionConfig::default();
                    Box::pin(async move {
                        store.create_session(&id, &config).await?;
                        Ok(())
                    })
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        generated.lock().unwrap().as_deref(),
        Some(session.id()),
        "a prepared creation without an id is created under a generated one, which the prepare step receives"
    );
    session.close().await.unwrap();

    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default().id("explicit-session"),
        )
        .await
        .unwrap();
    assert_eq!(
        session.id(),
        "explicit-session",
        "the options name a plain creation"
    );
    session.close().await.unwrap();

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
                    CreateSessionOptions::default()
                        .id(id)
                        .prepare(preparing(&store, entered.clone())),
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
    for id in [
        ".session-ids",
        ".SESSION-IDS",
        ".session-ids.",
        ".session-ids ",
        ".\u{017f}ession-ids",
        ".se\u{00df}ion-ids",
    ] {
        let entered = Arc::new(AtomicBool::new(false));
        let error = rejected(
            agent
                .create_session(
                    SessionConfig::default(),
                    CreateSessionOptions::default()
                        .id(id)
                        .prepare(preparing(&store, entered.clone())),
                )
                .await,
        );
        assert!(
            matches!(&error, AgentError::InvalidInput(message) if message == &format!("invalid Session id {id}")),
            "{id:?} names the id bookings rather than a Session: {error}"
        );
        assert!(
            !entered.load(Ordering::SeqCst),
            "{id:?}: a rejected id reaches no prepare step"
        );
    }
    let error = rejected(
        agent
            .create_session(
                SessionConfig::default(),
                CreateSessionOptions::default().id("nested/plain"),
            )
            .await,
    );
    assert!(
        matches!(&error, AgentError::InvalidInput(_)),
        "a rejected id without a prepare step: {error}"
    );
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
            CreateSessionOptions::default().id("explicit-session"),
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
            CreateSessionOptions::default().id("shared"),
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
                CreateSessionOptions::default().id("shared"),
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

/// A host whose WorkspaceTools output root is `root`: the product's wiring of
/// the id admission. No tool runs, so the platform needs no exec shell.
async fn output_host(store: &Arc<Shared>, root: &Path) -> MinimalHost {
    output_host_with(store, root, |_| {}).await
}

/// [`output_host`] with `install` run on the builder, for a fixture part.
async fn output_host_with(
    store: &Arc<Shared>,
    root: &Path,
    install: impl FnOnce(&mut AgentBuilder),
) -> MinimalHost {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_model_hosts(&mut builder);
    defaults::install(&mut builder);
    install(&mut builder);
    builder.timeline_store(TimelineStoreBinding::new(store.clone()));
    workspace_tools::install(
        &mut builder,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: root.to_owned(),
        },
    );
    let platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "test",
        reason: "the id admission runs no shell".into(),
    })
    .expect("the process platform");
    MinimalHost::start_with(&builder, move |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(platform))?;
        Ok(())
    })
    .await
    .expect("the output host starts")
}

/// A Session that declares one tool the output host serves and runs none.
fn output_session() -> SessionConfig {
    SessionConfig {
        tools: Some(vec!["read".into()]),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_id_without_a_usable_output_directory_is_rejected_before_preparation() {
    let root = std::env::temp_dir().join(format!("dim-output-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(Shared::default());
    let host = output_host(&store, &root).await;
    let agent = host.agent().clone();
    // One component this filesystem cannot hold as a directory name.
    let id = "s".repeat(300);
    let entered = Arc::new(AtomicBool::new(false));
    let error = rejected(
        agent
            .create_session(
                output_session(),
                CreateSessionOptions::default()
                    .id(id.clone())
                    .prepare(preparing(&store, entered.clone())),
            )
            .await,
    );
    assert!(
        matches!(&error, AgentError::InvalidInput(message) if message.starts_with(&format!("Session id {id} has no usable output directory"))),
        "a name this filesystem cannot hold is rejected: {error}"
    );
    assert!(
        !entered.load(Ordering::SeqCst),
        "a rejected id reaches no prepare step"
    );
    assert!(
        store.created.lock().unwrap().is_empty(),
        "a rejected id writes no rows: {:?}",
        store.created.lock().unwrap()
    );
    assert!(
        agent.get_session(&id).is_none(),
        "a rejected id publishes no Session"
    );
    host.close().await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ids_the_filesystem_cannot_tell_apart_do_not_share_an_output_directory() {
    let root = std::env::temp_dir().join(format!("dim-output-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(Shared::default());
    let host = output_host(&store, &root).await;
    let agent = host.agent().clone();
    let first = agent
        .create_session(
            output_session(),
            CreateSessionOptions::default().id("Named-Session"),
        )
        .await
        .unwrap();
    assert_eq!(
        first.id(),
        "Named-Session",
        "an accepted id keeps its value"
    );
    let entered = Arc::new(AtomicBool::new(false));
    match agent
        .create_session(
            output_session(),
            CreateSessionOptions::default()
                .id("named-session")
                .prepare(preparing(&store, entered.clone())),
        )
        .await
    {
        Ok(second) => {
            assert_ne!(second.id(), first.id());
            std::fs::create_dir_all(root.join(first.id())).unwrap();
            assert!(
                !root.join(second.id()).exists(),
                "ids this filesystem can tell apart keep distinct output directories"
            );
            second.close().await.unwrap();
        }
        Err(error) => {
            assert!(
                matches!(&error, AgentError::InvalidInput(message) if message == "session named-session is already in use"),
                "an id this filesystem cannot tell apart from an accepted one is refused: {error}"
            );
            assert!(
                !entered.load(Ordering::SeqCst),
                "a refused id reaches no prepare step"
            );
            assert!(
                !store
                    .created
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|id| id == "named-session"),
                "a refused id writes no rows: {:?}",
                store.created.lock().unwrap()
            );
            assert!(
                agent.get_session("named-session").is_none(),
                "a refused id publishes no Session"
            );
        }
    }
    first.close().await.unwrap();
    host.close().await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_creation_releases_its_booked_name() {
    let root = std::env::temp_dir().join(format!("dim-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(Shared::default());
    let host = output_host_with(&store, &root, |builder| {
        builder.session_extension_part::<u32>(GROUP, "options-probe", |_, _: &u32| {
            Err(rutis::Error::from(ErrorKind::PluginFailed(
                "extension fixture refused".into(),
            )))
        });
    })
    .await;
    let agent = host.agent().clone();
    let error = rejected(
        agent
            .create_session(
                output_session(),
                CreateSessionOptions::default().id("retried").prepare(|_| {
                    Box::pin(async { Err(AgentError::InvalidInput("prepare refused".into())) })
                }),
            )
            .await,
    );
    assert!(
        matches!(&error, AgentError::InvalidInput(message) if message == "prepare refused"),
        "a failed prepare is the creation's error: {error}"
    );
    assert!(
        store.created.lock().unwrap().is_empty(),
        "a failed prepare writes no rows: {:?}",
        store.created.lock().unwrap()
    );
    let entered = Arc::new(AtomicBool::new(false));
    let error = rejected(
        agent
            .create_session(
                output_session(),
                CreateSessionOptions::default()
                    .id("retried")
                    .prepare(preparing(&store, entered.clone()))
                    .extension(GROUP, 7_u32),
            )
            .await,
    );
    assert!(
        entered.load(Ordering::SeqCst),
        "the creation committed its rows before the build failed"
    );
    assert!(
        error.to_string().contains("extension fixture refused"),
        "a build that fails after its rows keeps its own error: {error}"
    );
    assert!(
        store.session_config("retried").await.unwrap().is_none(),
        "a failed build rolls its rows back"
    );
    let session = agent
        .create_session(
            output_session(),
            CreateSessionOptions::default().id("retried"),
        )
        .await
        .expect("a failed creation releases its booked name");
    assert_eq!(
        session.id(),
        "retried",
        "the id is the one it was booked under"
    );
    session.close().await.unwrap();
    host.close().await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}
