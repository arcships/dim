//! Owner initialization of an Agents generation (#318) through `serve`, as
//! process owners see it through `sessions`. No public flow can call
//! `sessions` at a chosen point of that initialization, so these tests hold a
//! stored Agent's instance creation inside its creation timeout, as a slow
//! Agent would. The hold exists only in this crate's unit tests, like the
//! title-write hold of the storage flow tests.
use super::{AgentDirectory, AgentIndex, Inner, SessionRestored, sessions::LaunchRequest};
use crate::ServerConfig;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use rutis::Ctx;
use rutis::host::CtxHost;
use rutis_bridge::session::{HostDispatch, host_key};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

mod slots;

type Hold = (String, oneshot::Sender<Ctx>, oneshot::Receiver<()>);
static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

/// Holds the next instance creation of one Agent until it is dropped.
struct HeldCreation {
    arrived: oneshot::Receiver<Ctx>,
    _release: oneshot::Sender<()>,
}
impl HeldCreation {
    fn new(agent_id: &str) -> Self {
        let (arrive, arrived) = oneshot::channel();
        let (release, released) = oneshot::channel();
        HOLDS
            .lock()
            .unwrap()
            .push((agent_id.to_owned(), arrive, released));
        Self {
            arrived,
            _release: release,
        }
    }
    /// The process root, once the held creation has started.
    async fn arrived(&mut self) -> Ctx {
        tokio::time::timeout(Duration::from_secs(10), &mut self.arrived)
            .await
            .expect("the held Agent creation starts")
            .unwrap()
    }
}
/// Called by `AgentIndex::load` inside the creation timeout.
pub(super) async fn creation_held(agent_id: &str, root: &Ctx) {
    let hold = {
        let mut holds = HOLDS.lock().unwrap();
        let index = holds.iter().position(|(held, ..)| held == agent_id);
        index.map(|index| holds.swap_remove(index))
    };
    if let Some((_, arrive, released)) = hold {
        let _ = arrive.send(root.clone());
        let _ = released.await;
    }
}

const SUBMISSION: &str = "test:stored-owner";

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::install(&mut builder);
    dim_agent_capabilities::defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(
        &mut builder,
        Arc::new(dim_agent_capabilities::adapters::MockTools),
    );
    builder
}
fn config(directory: &Path) -> ServerConfig {
    ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    }
}
/// A configuration whose process carries the Settings service, so loading a
/// Session authorizes and owns the project of its cwd as the product does.
fn product_config(directory: &Path) -> ServerConfig {
    let models = dim_agent_aimux::AimuxModels::from_config(
        serde_json::from_value::<dim_agent_aimux::config::ModelConfigFile>(json!({
            "defaultModel":{"providerId":"local","modelId":"alpha"},
            "providers":[{"id":"local","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1",
                "models":[{"id":"alpha","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":64}}]}]
        }))
        .unwrap(),
    )
    .unwrap();
    ServerConfig {
        model_catalog: Some(Arc::new(models)),
        ..config(directory)
    }
}

struct ProgressingProcess {
    name: String,
    hub: rutis::host::Key<crate::connection::ConnectionHub>,
    step: usize,
    completed: tokio::sync::watch::Sender<usize>,
}
impl rutis::host::DynPlugin for ProgressingProcess {
    fn name(&self) -> &str {
        &self.name
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.hub.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            ctx.require_at(&self.hub)?;
            let mut completed = self.completed.subscribe();
            while *completed.borrow_and_update() < self.step {
                tokio::select! {
                    result = completed.changed() => {
                        result.map_err(|error| rutis::host::ErrorKind::PluginFailed(error.into()))?
                    }
                    _ = ctx.cancelled() => return Err(rutis::host::ErrorKind::PluginFailed("opening cancelled".into()).into()),
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = ctx.cancelled() => return Err(rutis::host::ErrorKind::PluginFailed("opening cancelled".into()).into()),
            }
            self.completed.send_replace(self.step + 1);
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_startup_and_restart_follow_rows_becoming_active() {
    let mut builder = builder();
    let (completed, _) = tokio::sync::watch::channel(0);
    for step in 0..4 {
        let completed = completed.clone();
        builder.plugin(move |names| {
            Ok(Box::new(ProgressingProcess {
                name: format!("test.process-progress-{step}"),
                hub: names.key()?,
                step,
                completed: completed.clone(),
            }))
        });
    }
    let server = crate::serve(
        builder,
        ServerConfig {
            startup_timeout: Duration::from_millis(500),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(*completed.borrow(), 4);
    completed.send_replace(0);
    tokio::time::timeout(Duration::from_secs(20), server.restart("Connection"))
        .await
        .expect("a progressing process restart finishes")
        .unwrap();
    assert_eq!(*completed.borrow(), 4);
    server.shutdown();
    server.join().await.unwrap();
}

/// One `sessions` call as a process owner makes it: the reply envelope.
async fn call(root: &Ctx, method: &str, request: Value) -> Value {
    let service = root
        .get_at::<dyn HostDispatch>(&host_key("sessions"))
        .expect("the sessions service");
    match service.invoke(method, json!([request]).into()).unwrap() {
        rutis_bridge::session::Value::Reference(reply) if reply.is_future() => {
            reply.wait_async().await.unwrap().json().unwrap()
        }
        reply => panic!("sessions replies asynchronously: {:?}", reply.json()),
    }
}
/// The routed call that asks the line for the input `SUBMISSION` names.
fn lookup(session: &str) -> Value {
    json!({
        "sessionId": session,
        "lineId": "main",
        "service": "timelineContext",
        "method": "lookup",
        "args": [SUBMISSION],
    })
}

/// The routed call that writes the stored input.
fn stored_input(session: &str) -> Value {
    json!({
        "sessionId": session,
        "lineId": "main",
        "service": "timelineContext",
        "method": "input",
        "args": [{
            "submission_id": SUBMISSION,
            "mode": "append",
            "blocks": [{"type": "raw", "text": "stored input"}],
        }],
    })
}

async fn launch(index: &Arc<AgentIndex>, agent: Option<String>, cwd: &Path) -> String {
    index
        .launch(LaunchRequest {
            agent,
            cwd: cwd.to_str().unwrap().into(),
            model: None,
            policy: None,
            starter: None,
            origin: None,
            title: None,
            worktree: None,
        })
        .await
        .unwrap()
        .session_id
}
/// A new peer Agent. Closing it keeps its record: a stored Agent that is not
/// running, which the next generation creates again.
async fn peer(index: &AgentIndex) -> String {
    let host = index
        .create(uuid::Uuid::new_v4(), None, None)
        .await
        .unwrap();
    host.agent_id.clone()
}

/// A native store whose default Agent owns a Session with one input.
struct Stored {
    directory: PathBuf,
    default_agent: String,
    session: String,
    /// The `lookup` reply for the input.
    accepted: Value,
}
async fn stored() -> Stored {
    let directory = std::env::temp_dir().join(format!("dim-owners-{}", uuid::Uuid::new_v4()));
    let server = crate::serve(builder(), config(&directory)).await.unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let session = launch(&index, None, &directory).await;
    call(&server.root, "call", stored_input(&session)).await;
    let accepted = call(&server.root, "call", lookup(&session)).await;
    assert_eq!(
        accepted["value"]["data"]["submission_id"], SUBMISSION,
        "the line answers the input it holds: {accepted}"
    );
    let default_agent = index.default_agent_id().to_owned();
    server.shutdown();
    server.join().await.unwrap();
    Stored {
        directory,
        default_agent,
        session,
        accepted,
    }
}

/// A process owner that looks up a committed input while the host starts
/// gets `reloading` until the stored owners are installed, then the input;
/// never a permanent `failed` for a healthy stored Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_never_fails_a_stored_session_while_its_owner_installs() {
    let stored = stored().await;
    let mut held = HeldCreation::new(&stored.default_agent);
    let mut starting = std::pin::pin!(crate::serve(builder(), config(&stored.directory)));
    let root = tokio::select! {
        root = held.arrived() => root,
        result = &mut starting => panic!("startup ended unheld: {:?}", result.err()),
    };
    let reply = call(&root, "call", lookup(&stored.session)).await;
    assert_eq!(reply["error"]["code"], "session_reloading", "{reply}");
    assert_eq!(reply["error"]["retryable"], true, "{reply}");
    drop(held);
    let server = starting.await.unwrap();
    assert_eq!(
        call(&server.root, "call", lookup(&stored.session)).await,
        stored.accepted
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stored_agent_creation_timeout_settles_during_progressing_startup() {
    let stored = stored().await;
    let mut held = HeldCreation::new(&stored.default_agent);
    let mut progressing = builder();
    let (completed, _) = tokio::sync::watch::channel(0);
    progressing.plugin(move |names| {
        Ok(Box::new(ProgressingProcess {
            name: "test.stored-owner-progress".into(),
            hub: names.key()?,
            step: 0,
            completed: completed.clone(),
        }))
    });
    let mut starting = std::pin::pin!(crate::serve(
        progressing,
        ServerConfig {
            startup_timeout: Duration::from_secs(2),
            ..config(&stored.directory)
        },
    ));
    tokio::select! {
        _ = held.arrived() => {}
        result = &mut starting => panic!("startup ended unheld: {:?}", result.err()),
    }
    let server = tokio::time::timeout(Duration::from_secs(10), starting)
        .await
        .expect("the stored Agent's timeout settles initialization")
        .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let reason = index.unavailable_agent(&stored.default_agent).unwrap();
    assert!(reason.contains("timed out"), "{reason}");
    let reply = call(&server.root, "call", lookup(&stored.session)).await;
    assert_eq!(reply["error"]["code"], "session_unavailable", "{reply}");
    drop(held);
    server.shutdown();
    server.join().await.unwrap();
    let server = crate::serve(builder(), config(&stored.directory))
        .await
        .unwrap();
    assert_eq!(
        call(&server.root, "call", lookup(&stored.session)).await,
        stored.accepted
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

/// An execution-disabled host keeps its gate: it loads neither the default
/// nor a stored Agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_disabled_hosts_load_no_stored_agent() {
    let stored = stored().await;
    let mut held = HeldCreation::new(&stored.default_agent);
    let server = crate::serve(
        builder(),
        ServerConfig {
            session_execution: false,
            ..config(&stored.directory)
        },
    )
    .await
    .unwrap();
    assert!(server.agent().is_none());
    assert!(server.root.get::<AgentIndex>().unwrap().hosts().is_empty());
    assert!(
        held.arrived.try_recv().is_err(),
        "no Agent creation started"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

/// A failure reading the stored Agents is a returned initialization error:
/// startup fails with it and cleans up, and is not recorded as an unavailable
/// Agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_owner_read_fails_startup_and_cleans_up() {
    let stored = stored().await;
    let database = stored.directory.join("dimcode.sqlite");
    let mark_default = |default: bool| {
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE agents SET is_default = ?1 WHERE agent_id = ?2",
                rusqlite::params![default, stored.default_agent],
            )
            .unwrap();
    };
    mark_default(false);
    let error = match crate::serve(builder(), config(&stored.directory)).await {
        Err(error) => error.to_string(),
        Ok(server) => {
            server.shutdown();
            server.join().await.unwrap();
            panic!("startup ignored an unreadable Agent directory");
        }
    };
    assert!(error.contains("no unique default Agent"), "{error}");
    mark_default(true);
    let server = crate::serve(builder(), config(&stored.directory))
        .await
        .unwrap();
    assert_eq!(
        call(&server.root, "call", lookup(&stored.session)).await,
        stored.accepted
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

/// `restart("Agents")` keeps the per-Agent creation timeout and no overall
/// deadline: a stored Agent whose creation times out is recorded
/// unavailable, initialization continues, loaded Sessions stay loaded and
/// the unavailable owner's Sessions keep their real error. A read on a
/// Session that stays loaded is served by the directory while the next
/// generation initializes ([#2227](https://github.com/arcships/dim/pull/2227)).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agents_restart_records_a_creation_timeout_and_keeps_loaded_sessions() {
    let stored = stored().await;
    let server = crate::serve(builder(), config(&stored.directory))
        .await
        .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    assert_eq!(
        call(&server.root, "call", lookup(&stored.session)).await,
        stored.accepted
    );
    let (_, loaded) = index.session(&stored.session).unwrap();
    let peer = peer(&index).await;
    let peer_session = launch(&index, Some(peer.clone()), &stored.directory).await;
    index.close(&peer).await.unwrap();
    let mut held = HeldCreation::new(&peer);
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        let root = held.arrived().await;
        let reply = call(&root, "call", lookup(&stored.session)).await;
        assert_eq!(reply, stored.accepted, "a loaded Session answers its read");
        // A write is not served meanwhile: `timeline.append` answers the
        // reload result and appends nothing.
        let before = dim_agent_runtime::testing::SessionLines::sequence(&loaded, "main").unwrap();
        let append = call(
            &root,
            "call",
            json!({
                "sessionId": stored.session,
                "lineId": "main",
                "service": "timeline",
                "method": "append",
                "args": [{"type": "input", "name": "message", "data": {
                    "mode": "append",
                    "blocks": [{"type": "raw", "text": "during the reload"}],
                }}],
            }),
        )
        .await;
        assert_eq!(append["error"]["code"], "session_reloading", "{append}");
        assert_eq!(
            dim_agent_runtime::testing::SessionLines::sequence(&loaded, "main").unwrap(),
            before,
            "nothing was appended"
        );
    });
    restarted.unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let reason = index.unavailable_agent(&peer).unwrap();
    assert!(reason.contains("timed out"), "{reason}");
    assert!(index.host(Some(&peer)).is_none());
    assert!(index.session(&stored.session).is_some() && !loaded.is_unloading());
    assert_eq!(
        call(&server.root, "call", lookup(&stored.session)).await,
        stored.accepted
    );
    let reply = call(&server.root, "call", lookup(&peer_session)).await;
    assert_eq!(reply["error"]["code"], "session_unavailable", "{reply}");
    assert_eq!(reply["error"]["retryable"], false, "{reply}");
    drop(held);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

/// A dependency-driven Agents reload initializes owners as a restart does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dependency_reload_records_a_creation_timeout_and_continues() {
    let home = std::env::temp_dir().join(format!("dim-owners-{}", uuid::Uuid::new_v4()));
    let server = crate::serve(
        builder(),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("native")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let peer = peer(&index).await;
    index.close(&peer).await.unwrap();
    let mut held = HeldCreation::new(&peer);
    let (restarted, ()) = tokio::join!(server.processes.loader.restart("Settings"), async {
        let root = held.arrived().await;
        let reply = call(&root, "call", lookup("unknown")).await;
        assert_eq!(reply["error"]["code"], "session_reloading", "{reply}");
    });
    restarted.unwrap();
    server
        .processes
        .wait_active(&server.root, Duration::from_secs(30))
        .await
        .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let reason = index.unavailable_agent(&peer).unwrap();
    assert!(reason.contains("timed out"), "{reason}");
    assert!(index.default_host().is_some());
    drop(held);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A settlement ends only its own Session's ownership, and an Agent's close
/// ends every ownership it registered: another Session of the same cwd keeps
/// the project in use, and only the last settlement frees its root.
#[test]
fn settlements_end_only_the_ownership_they_name() {
    let directory = Arc::new(AgentDirectory {
        inner: Mutex::new(Inner::default()),
        settings: Mutex::new(std::sync::Weak::new()),
        finished: std::sync::atomic::AtomicBool::new(false),
        default_agent_id: std::sync::OnceLock::new(),
        server_instance_id: "test".to_owned(),
    });
    let first = directory.own("first", "agent-a", "/project");
    let _second = directory.own("second", "agent-a", "/project");
    let _member = directory.own("member", "agent-b", "/project");
    drop(first);
    assert!(
        directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key("second"),
        "another Session of the same cwd still owns the project"
    );
    directory.settle_agent("agent-a");
    assert!(
        !directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key("second"),
        "the Agent's close ends every ownership it registered"
    );
    assert!(
        directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key("member"),
        "another Agent's Session keeps its ownership"
    );
}

/// A guard settles only the ownership of the load that registered it. The
/// runtime's fault supervisor closes a Session without the slot's close flag,
/// so the same ID can load again before the guard the first load handed to its
/// close is dropped; that guard must leave the later load's ownership.
#[test]
fn a_guard_dropped_after_a_later_load_keeps_that_loads_ownership() {
    let directory = Arc::new(AgentDirectory {
        inner: Mutex::new(Inner::default()),
        settings: Mutex::new(std::sync::Weak::new()),
        finished: std::sync::atomic::AtomicBool::new(false),
        default_agent_id: std::sync::OnceLock::new(),
        server_instance_id: "test".to_owned(),
    });
    let first = directory.own("session", "agent-a", "/project");
    let _second = directory.own("session", "agent-a", "/project");
    drop(first);
    let (owned, in_use) = {
        let inner = directory.inner.lock().unwrap();
        (inner.owners.contains_key("session"), inner.owns("/project"))
    };
    assert!(
        owned,
        "the later load's ownership outlives the guard of the load before it"
    );
    assert!(
        in_use,
        "the later load's ownership keeps the project's root held"
    );
}

/// Refuses the announcement of every restored Session, as a listener that
/// cannot accept the load would.
struct RefusesSessionRestored;
impl rutis::host::DynPlugin for RefusesSessionRestored {
    fn name(&self) -> &str {
        "test.refused-announcement"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            ctx.on::<SessionRestored>(|_, _| async {
                Err::<Option<()>, _>(rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    "the stored announcement is refused".into(),
                )))
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// A restore that loaded the Session keeps the project ownership it registered
/// when announcing that load fails: the Session is live and still uses the
/// project, so its ownership ends with its close, not with the failed
/// announcement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loaded_session_keeps_its_project_when_its_announcement_fails() {
    use crate::settings::SettingsService;
    use dim_agent_capabilities::file_storage::{FileStorage, testing};

    let stored = stored().await;
    let mut refusing = builder();
    refusing.plugin(|_| Ok(Box::new(RefusesSessionRestored)));
    let server = crate::serve(refusing, product_config(&stored.directory))
        .await
        .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();

    let refused = index
        .acquire(&stored.session)
        .await
        .err()
        .expect("the announcement refuses the acquisition");
    assert!(
        refused.to_string().contains("session-restored"),
        "the announcement failed: {refused}"
    );
    assert!(
        index.session(&stored.session).is_some(),
        "the runtime loaded the Session before the announcement"
    );
    let root = settings
        .project_root(&stored.directory.canonicalize().unwrap())
        .expect("the restore authorized the project");
    assert!(
        index
            .directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key(&stored.session),
        "the Session that loaded owns its project"
    );
    assert!(
        testing::root_held(&files, root),
        "the project's root stays held for the Session that loaded"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

/// A Session the runtime closes on its own, as the fault supervisor does, ends
/// its project ownership and releases the project's root, without the App
/// Server closing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runtime_close_ends_the_project_ownership_it_did_not_start() {
    use crate::settings::SettingsService;
    use dim_agent_capabilities::file_storage::{FileStorage, testing};

    let stored = stored().await;
    let server = crate::serve(builder(), product_config(&stored.directory))
        .await
        .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = stored.directory.canonicalize().unwrap();
    let usage = index.acquire(&stored.session).await.unwrap();
    let root = settings
        .project_root(&project)
        .expect("the restore authorized the project");
    assert!(
        testing::root_held(&files, root),
        "the loaded Session holds the project's directory"
    );
    let released = testing::root_releases(&files, root);
    drop(usage);
    let (_, session) = index
        .session(&stored.session)
        .expect("the Session is loaded");
    // The guard the load registered is watched by its close, which this test
    // cannot await directly. Registering the same Session again under the same
    // agent and cwd gives this test the same guard, and joining its close with
    // the Session's close settles the ownership before the assertions run.
    let ownership = index.directory.own(
        &stored.session,
        index.default_agent_id(),
        &session.config().cwd,
    );
    let watcher = ownership.when_closed(session.closed());
    let (closed, ()) = tokio::join!(session.close(), watcher);
    closed.unwrap();
    assert!(
        !index
            .directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key(&stored.session),
        "the Session the runtime closed ends its ownership"
    );
    assert_eq!(
        testing::root_releases(&files, root),
        released + 1,
        "the last owner's close released the project's directory"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}

/// A load's ownership ends only with the load that registered it. One close has
/// two settlements: the idle pass settles the Session it unloaded under its
/// admission gate, and the guard the load registered waits in a detached task
/// for the same close. Here that watcher reaches its awaited close only after
/// the pass unloaded the Session and a second load of the same ID registered a
/// new ownership, so a settlement that names only the Session would end the
/// load that replaced it and release the root it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_close_watcher_keeps_the_ownership_of_the_load_after_it() {
    use crate::settings::SettingsService;
    use dim_agent_capabilities::file_storage::{FileStorage, testing};

    let stored = stored().await;
    let server = crate::serve(
        builder(),
        ServerConfig {
            idle_unload_after: Duration::from_secs(3600),
            ..product_config(&stored.directory)
        },
    )
    .await
    .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let files = server.service::<FileStorage>().unwrap();
    let settings = server.service::<SettingsService>().unwrap();
    let project = stored.directory.canonicalize().unwrap();

    let first = index.acquire(&stored.session).await.unwrap();
    let root = settings
        .project_root(&project)
        .expect("the restore authorized the project");
    assert!(
        testing::root_held(&files, root),
        "the first load holds the project's directory"
    );
    let (_, session) = index
        .session(&stored.session)
        .expect("the Session is loaded");
    // The guard the first load registered is watched by its close, which this
    // test cannot await directly. Registering the same Session again under the
    // same agent and cwd gives this test the same guard, and the test keeps its
    // watcher unawaited across the unload and the next load.
    let watcher = index
        .directory
        .own(
            &stored.session,
            index.default_agent_id(),
            &session.config().cwd,
        )
        .when_closed(session.closed());
    drop(first);
    let mut marks = HashMap::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            index.unload_idle(&mut marks).await;
            if index.session(&stored.session).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the idle pass unloads the Session");

    let second = index.acquire(&stored.session).await.unwrap();
    assert!(
        index
            .directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key(&stored.session),
        "the second load registers its own ownership"
    );
    assert!(
        testing::root_held(&files, root),
        "the second load holds the project's directory"
    );

    watcher.await;
    assert!(
        testing::root_held(&files, root),
        "the first load's delayed close does not release the root the restored Session holds"
    );
    assert!(
        index
            .directory
            .inner
            .lock()
            .unwrap()
            .owners
            .contains_key(&stored.session),
        "the first load's delayed close leaves the ownership of the load that replaced it"
    );
    drop(second);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(stored.directory).unwrap();
}
