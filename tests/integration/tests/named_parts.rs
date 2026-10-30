//! Rust parts that depend on names only (plugin contract P2): no layer type,
//! no scope, no group in the part. Where the row is placed decides.
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, Feature, Modules, Names, SessionConfig, SessionIdentity, Timeline,
    TimelineIdentity, names, register_part,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, InstanceId, Key, TypeKey};
use rutis::{Ctx, Plugin};
use rutis_bridge::session::HostDispatch;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Log = Arc<Mutex<Vec<String>>>;

/// Records the Session it runs in.
struct SessionPart {
    log: Log,
    identity: Key<SessionIdentity>,
}
impl SessionPart {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for SessionPart {
    fn name(&self) -> &str {
        "test.session-part"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            self.log
                .lock()
                .unwrap()
                .push(format!("session {}", identity.session_id));
            Ok(Effect::Done)
        })
    }
}

/// Records the branch it runs in, reading a Session and a branch name.
struct BranchPart {
    log: Log,
    session: Key<SessionIdentity>,
    branch: Key<TimelineIdentity>,
    timeline: Key<Timeline>,
}
impl BranchPart {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            session: names.key()?,
            branch: names.key()?,
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for BranchPart {
    fn name(&self) -> &str {
        "test.branch-part"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.session.erase(),
            self.branch.erase(),
            self.timeline.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let session = ctx.require_at(&self.session)?;
            let branch = ctx.require_at(&self.branch)?;
            let timeline = ctx.require_at(&self.timeline)?;
            assert_eq!(session.session_id, branch.session_id);
            self.log.lock().unwrap().push(format!(
                "branch {}/{} at {}",
                branch.session_id,
                branch.branch_id,
                timeline.sequence()
            ));
            Ok(Effect::Done)
        })
    }
}

/// A Session service a Session part provides by name and a branch part
/// injects by name: each Session's branches see their own Session's copy.
struct Greeting(String);
struct GreetingPart {
    greeting: Key<Greeting>,
    identity: Key<SessionIdentity>,
}
impl GreetingPart {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            greeting: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for GreetingPart {
    fn name(&self) -> &str {
        "test.greeting"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            ctx.provide_at(
                self.greeting.clone(),
                Arc::new(Greeting(format!("hello {}", identity.session_id))),
            )?;
            Ok(Effect::Done)
        })
    }
}
struct GreetedPart {
    log: Log,
    greeting: Key<Greeting>,
    branch: Key<TimelineIdentity>,
}
impl GreetedPart {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            greeting: names.key()?,
            branch: names.key()?,
        })
    }
}
impl DynPlugin for GreetedPart {
    fn name(&self) -> &str {
        "test.greeted"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.greeting.erase(), self.branch.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let greeting = ctx.require_at(&self.greeting)?;
            let branch = ctx.require_at(&self.branch)?;
            self.log
                .lock()
                .unwrap()
                .push(format!("{} in {}", greeting.0, branch.session_id));
            Ok(Effect::Done)
        })
    }
}

/// A service that crosses languages: provided in each Session at its
/// dispatcher's key, as a row of another language provides it, and read
/// there by a branch part through the catalog.
struct Greeter(String);
impl rutis_bridge::session::HostDispatch for Greeter {
    fn invoke(&self, _: &str, _: rutis_bridge::session::Value) -> rutis_bridge::session::Reply {
        Ok(json!(self.0).into())
    }
    fn provider(&self) -> Option<String> {
        Some(self.0.clone())
    }
}
struct GreeterPart {
    instance: InstanceId,
    identity: Key<SessionIdentity>,
}
impl GreeterPart {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            instance: names.instance(dim_agent_runtime::SESSION_GROUP)?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for GreeterPart {
    fn name(&self) -> &str {
        "test.greeter"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            ctx.provide_at::<dyn HostDispatch>(
                rutis_bridge::session::host_key_in("greeter", self.instance),
                Arc::new(Greeter(format!("greeter of {}", identity.session_id))),
            )?;
            Ok(Effect::Done)
        })
    }
}
struct GreeterReader {
    log: Log,
    greeter: Key<dyn HostDispatch>,
    branch: Key<TimelineIdentity>,
}
impl GreeterReader {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            greeter: names.named("greeter")?,
            branch: names.key()?,
        })
    }
}
impl DynPlugin for GreeterReader {
    fn name(&self) -> &str {
        "test.greeter-reader"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.greeter.erase(), self.branch.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let greeter = ctx.require_at(&self.greeter)?;
            let branch = ctx.require_at(&self.branch)?;
            self.log.lock().unwrap().push(format!(
                "{} for {}",
                greeter.provider().unwrap(),
                branch.session_id
            ));
            Ok(Effect::Done)
        })
    }
}

/// A process service and its provider and consumer parts.
struct Clock(u64);
struct ClockPart;
impl Plugin for ClockPart {
    type Deps = ();
    fn name(&self) -> &str {
        "test.clock"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide(Clock(7))?;
        Ok(())
    }
}
struct ProcessPart {
    log: Log,
    clock: Key<Clock>,
}
impl ProcessPart {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            clock: names.key()?,
        })
    }
}
impl DynPlugin for ProcessPart {
    fn name(&self) -> &str {
        "test.process-part"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.clock.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let clock = ctx.require_at(&self.clock)?;
            self.log
                .lock()
                .unwrap()
                .push(format!("process {}", clock.0));
            Ok(Effect::Done)
        })
    }
}

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_part_without_layer_types_mounts_in_every_group() {
    let log = Log::default();
    let mut builder = builder();
    let (session_log, branch_log) = (log.clone(), log.clone());
    builder.require_feature(
        Feature::new("test.named-parts")
            .session("test.session-part", move |names| {
                Ok(Box::new(SessionPart::new(names, session_log.clone())?))
            })
            .branch("test.branch-part", move |names| {
                Ok(Box::new(BranchPart::new(names, branch_log.clone())?))
            }),
    );
    let agent = builder.build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    assert!(log.lock().unwrap().contains(&format!("session {id}")));
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|entry| entry.starts_with(&format!("branch {id}/main at "))),
        "{:?}",
        log.lock().unwrap()
    );
    agent.close().await.unwrap();

    let mut catalog = names::catalog();
    names::register::<Clock>(&mut catalog, None);
    let mut builtins = Modules::new();
    let provider = register_part(&mut builtins, &catalog, |_| Ok(Box::new(ClockPart))).unwrap();
    let process_log = log.clone();
    let consumer = register_part(&mut builtins, &catalog, move |names| {
        Ok(Box::new(ProcessPart::new(names, process_log.clone())?))
    })
    .unwrap();
    let root = Ctx::root().unwrap();
    let plugin = rutis_loader::LoaderPlugin::new(
        builtins,
        rutis_loader::LoaderOptions {
            catalog,
            ..Default::default()
        },
    );
    let loader = plugin.handle();
    let view = root.plugin(plugin);
    dim_agent_runtime::wait_active(
        &root,
        std::slice::from_ref(&view),
        Duration::from_secs(5),
        &Default::default(),
    )
    .await
    .unwrap();
    let rows = serde_json::from_value(json!([{ "insert": [
        { "id": provider, "name": provider },
        { "id": consumer, "name": consumer },
    ] }]))
    .unwrap();
    let report = loader
        .reconcile(vec![rutis_loader::Layer::new("test", rows)], None)
        .await
        .unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !log.lock().unwrap().contains(&"process 7".to_owned()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_part_provides_by_name_and_each_instance_sees_its_own_copy() {
    let log = Log::default();
    let mut builder = builder();
    names::register::<Greeting>(
        builder.catalog_mut(),
        Some(dim_agent_runtime::SESSION_GROUP),
    );
    let greeted = log.clone();
    builder.require_feature(
        Feature::new("test.greeting")
            .session("test.greeting", |names| {
                Ok(Box::new(GreetingPart::new(names)?))
            })
            .branch("test.greeted", move |names| {
                Ok(Box::new(GreetedPart::new(names, greeted.clone())?))
            }),
    );
    let agent = builder.build();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        ids.push(session.id().to_owned());
    }
    let log = log.lock().unwrap().clone();
    for id in &ids {
        assert!(log.contains(&format!("hello {id} in {id}")), "{log:?}");
    }
    assert_eq!(log.len(), 2, "{log:?}");
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rust_part_reads_a_crossing_name_at_its_catalog_key() {
    let log = Log::default();
    let mut builder = builder();
    builder
        .catalog_mut()
        .register_shared_instance("greeter", dim_agent_runtime::SESSION_GROUP);
    let read = log.clone();
    builder.require_feature(
        Feature::new("test.greeter")
            .session("test.greeter", |names| {
                Ok(Box::new(GreeterPart::new(names)?))
            })
            .branch("test.greeter-reader", move |names| {
                Ok(Box::new(GreeterReader::new(names, read.clone())?))
            }),
    );
    let agent = builder.build();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        ids.push(session.id().to_owned());
    }
    let log = log.lock().unwrap().clone();
    for id in &ids {
        assert!(
            log.contains(&format!("greeter of {id} for {id}")),
            "{log:?}"
        );
    }
    assert_eq!(log.len(), 2, "{log:?}");
    agent.close().await.unwrap();
}

/// A registered part's names are checked where an instance mounts it: a
/// Session part reading a branch name fails that Session's creation as a
/// build failure, and a process row reading a Session name fails its
/// registration.
#[tokio::test]
async fn a_misplaced_part_fails_where_it_is_mounted_naming_the_group() {
    let log = Log::default();
    let mut builder = builder();
    builder.require_feature(
        Feature::new("test.misplaced").session("test.branch-part", move |names| {
            Ok(Box::new(BranchPart::new(names, log.clone())?))
        }),
    );
    let agent = builder.build();
    let error = agent
        .create_session(SessionConfig::default())
        .await
        .err()
        .expect("a Session part reading branch names fails");
    assert!(
        matches!(error, dim_agent_runtime::AgentError::Build(_)),
        "{error:?}"
    );
    assert!(
        error.to_string().contains(
            "timelineIdentity is a branch service: place the part inside the branch group"
        ),
        "{error}"
    );
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();

    let mut builtins = Modules::new();
    let error = register_part(&mut builtins, &names::catalog(), |names| {
        Ok(Box::new(SessionPart::new(names, Log::default())?))
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("place the part inside the session group"),
        "{error}"
    );
}

/// Waits in `apply` for work that never comes, until its generation is
/// cancelled: rutis waits for `apply` to return and never aborts it.
struct Stuck {
    identity: Key<SessionIdentity>,
}

struct Progressing {
    name: String,
    dependencies: Vec<TypeKey>,
    step: usize,
    completed: tokio::sync::watch::Sender<usize>,
}
impl DynPlugin for Progressing {
    fn name(&self) -> &str {
        &self.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.dependencies.clone()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let mut completed = self.completed.subscribe();
            while *completed.borrow_and_update() < self.step {
                tokio::select! {
                    result = completed.changed() => result.map_err(|error| ErrorKind::PluginFailed(error.into()))?,
                    _ = ctx.cancelled() => return Err(ErrorKind::PluginFailed("opening cancelled".into()).into()),
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = ctx.cancelled() => return Err(ErrorKind::PluginFailed("opening cancelled".into()).into()),
            }
            self.completed.send_replace(self.step + 1);
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_and_its_main_branch_keep_opening_while_rows_become_active() {
    let mut builder = builder();
    builder.startup_timeout(Duration::from_millis(500));
    let (completed, _) = tokio::sync::watch::channel(0);
    let mut feature = Feature::new("test.progress");
    for step in 0..4 {
        let completed = completed.clone();
        let name = format!("test.progress-session-{step}");
        feature = feature.session(name.clone(), move |names| {
            Ok(Box::new(Progressing {
                name: name.clone(),
                dependencies: vec![names.key::<SessionIdentity>()?.erase()],
                step,
                completed: completed.clone(),
            }))
        });
    }
    for step in 4..8 {
        let completed = completed.clone();
        let name = format!("test.progress-branch-{step}");
        feature = feature.branch(name.clone(), move |names| {
            Ok(Box::new(Progressing {
                name: name.clone(),
                dependencies: vec![names.key::<TimelineIdentity>()?.erase()],
                step,
                completed: completed.clone(),
            }))
        });
    }
    builder.require_feature(feature);
    let agent = builder.build();
    let session = tokio::time::timeout(
        Duration::from_secs(20),
        agent.create_session(SessionConfig::default()),
    )
    .await
    .expect("a progressing opening finishes")
    .unwrap();
    assert_eq!(*completed.borrow(), 8);
    assert!(session.state("main").is_ok());
    agent.close().await.unwrap();
}
impl Stuck {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            identity: names.key()?,
        })
    }
}
impl DynPlugin for Stuck {
    fn name(&self) -> &str {
        "test.stuck"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.cancelled().await;
            Err(rutis::Error::from(ErrorKind::PluginFailed(
                "cancelled while waiting".into(),
            )))
        })
    }
}

#[tokio::test]
async fn a_row_blocking_in_apply_fails_creation_after_no_progress() {
    let mut builder = builder();
    builder
        .startup_timeout(Duration::from_millis(100))
        .require_feature(
            Feature::new("test.stuck")
                .session("test.stuck", |names| Ok(Box::new(Stuck::new(names)?))),
        );
    let agent = builder.build();
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(20),
        agent.create_session(SessionConfig::default()),
    )
    .await
    .expect("creation returns after no progress")
    .err()
    .expect("the stuck row fails the creation");
    assert!(started.elapsed() < Duration::from_secs(5), "{error}");
    assert!(error.to_string().contains("test.stuck"), "{error}");
    assert!(agent.sessions().is_empty());
    tokio::time::timeout(Duration::from_secs(20), agent.close())
        .await
        .expect("close returns")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activity_outside_an_opening_cannot_keep_a_stuck_session_alive() {
    let root = Ctx::root().unwrap();
    let mut builder = builder();
    builder
        .startup_timeout(Duration::from_millis(100))
        .require_feature(
            Feature::new("test.stuck")
                .session("test.stuck", |names| Ok(Box::new(Stuck::new(names)?))),
        );
    let agent = builder.build_in(&root);
    let unrelated = async {
        for _ in 0..8 {
            let view = root.plugin(ClockPart);
            (&view).await.unwrap();
            view.dispose().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let error = tokio::select! {
        result = agent.create_session(SessionConfig::default()) => result.err().expect("the stuck opening fails"),
        () = unrelated => panic!("unrelated Active rows kept the opening alive"),
    };
    assert!(error.to_string().contains("test.stuck"), "{error}");
    agent.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarting_an_optional_row_cannot_keep_a_stuck_opening_alive() {
    let root = Ctx::root().unwrap();
    let mut builder = builder();
    builder
        .startup_timeout(Duration::from_millis(500))
        .require_feature(
            Feature::new("test.stuck")
                .session("test.stuck", |names| Ok(Box::new(Stuck::new(names)?))),
        );
    builder.feature(Feature::new("test.churn").session("test.churn", |names| {
        Ok(Box::new(SessionPart::new(names, Arc::default())?))
    }));
    let agent = builder.build_in(&root);
    let opening = agent.create_session(SessionConfig::default());
    tokio::pin!(opening);
    let churn = async {
        let view = loop {
            if let Some(view) = root
                .diagnostics()
                .plugins
                .iter()
                .find(|plugin| {
                    plugin.name == "test.session-part"
                        && plugin.state == rutis::host::FiberState::Active
                })
                .and_then(|plugin| root.view(plugin.id))
            {
                break view;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        for _ in 0..24 {
            let _ = view.restart().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let error = tokio::select! {
        result = &mut opening => result.err().expect("the stuck opening fails"),
        () = churn => panic!("repeated activation of one row kept startup alive"),
    };
    assert!(error.to_string().contains("test.stuck"), "{error}");
    agent.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn optional_rows_extend_a_session_or_branch_opening_until_required_rows_are_ready() {
    for branch in [false, true] {
        let root = Ctx::root().unwrap();
        let mut builder = builder();
        builder.startup_timeout(Duration::from_millis(500));
        names::register::<Clock>(builder.catalog_mut(), None);
        let (completed, _) = tokio::sync::watch::channel(0);
        for step in 0..4 {
            let completed = completed.clone();
            let name = format!("test.optional-progress-{step}");
            let part = name.clone();
            let make = move |names: &Names<'_>| -> rutis::Result<Box<dyn DynPlugin>> {
                let identity = if branch {
                    names.key::<TimelineIdentity>()?.erase()
                } else {
                    names.key::<SessionIdentity>()?.erase()
                };
                Ok(Box::new(Progressing {
                    name: part.clone(),
                    dependencies: vec![identity, names.key::<Clock>()?.erase()],
                    step,
                    completed: completed.clone(),
                }))
            };
            let feature = Feature::new(name.clone());
            let feature = if branch {
                feature.branch(name, make)
            } else {
                feature.session(name, make)
            };
            if step == 3 {
                builder.require_feature(feature);
            } else {
                builder.feature(feature);
            }
        }
        let agent = builder.build_in(&root);
        let opening = agent.create_session(SessionConfig::default());
        tokio::pin!(opening);
        tokio::select! {
            result = &mut opening => panic!("gated opening finished before publication: {:?}", result.err()),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        let clock = root.plugin(ClockPart);
        (&clock).await.unwrap();
        let session = tokio::time::timeout(Duration::from_secs(20), opening)
            .await
            .expect("progressing optional rows finish opening")
            .unwrap();
        assert_eq!(*completed.borrow(), 4);
        assert!(session.state("main").is_ok());
        agent.close().await.unwrap();
        root.shutdown().await.unwrap();
    }
}
