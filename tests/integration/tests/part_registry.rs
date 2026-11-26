//! The part registry and the instances' mounters (ADR 006 D3): a plugin
//! registers parts while it runs, each Session and branch mounts them, and
//! the plugin's unload takes them out of every instance. A minimal host has
//! a registry of its own; a plugin of the host hands it to the test, which
//! mounts producers that register there.
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, BranchOptions, Names, PartRegistry, PartScope, Session,
    SessionConfig, SessionIdentity, TimelineIdentity,
};
use dim_agent_types::BranchSource;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberView, Key, TypeKey};
use rutis::{Ctx, Plugin};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

type Log = Arc<Mutex<Vec<String>>>;

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder
}

type Registries = tokio::sync::mpsc::UnboundedReceiver<Arc<PartRegistry>>;
/// A plugin of the host that hands the test the host's registry when the
/// host starts.
struct Host {
    send: tokio::sync::mpsc::UnboundedSender<Arc<PartRegistry>>,
    registry: Key<PartRegistry>,
}
impl Host {
    fn new(
        names: &Names<'_>,
        send: tokio::sync::mpsc::UnboundedSender<Arc<PartRegistry>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            send,
            registry: names.key()?,
        })
    }
}
impl DynPlugin for Host {
    fn name(&self) -> &str {
        "test.host"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            let _ = self.send.send(registry);
            Ok(Effect::Done)
        })
    }
}
/// `builder` with [`Host`], and where its Agent's registry arrives.
fn hosted(mut builder: AgentBuilder) -> (AgentBuilder, Registries) {
    let (send, registries) = tokio::sync::mpsc::unbounded_channel();
    builder.plugin(move |names| Ok(Box::new(Host::new(names, send.clone())?)));
    (builder, registries)
}
/// A minimal host for `builder` and its registry.
async fn placed(builder: AgentBuilder) -> (MinimalHost, Arc<PartRegistry>) {
    let (builder, mut registries) = hosted(builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    (agent, registries.recv().await.unwrap())
}

/// A process plugin that registers one part while it runs.
struct Producer {
    registry: Arc<PartRegistry>,
    scope: PartScope,
    name: &'static str,
    generator: dim_agent_runtime::Generator,
}
impl Plugin for Producer {
    type Deps = ();
    fn name(&self) -> &str {
        "test.producer"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        let generator = self.generator.clone();
        self.registry
            .register(ctx, self.scope, self.name, move |mount| generator(mount))?;
        Ok(())
    }
}
async fn produce(
    root: &Ctx,
    registry: Arc<PartRegistry>,
    scope: PartScope,
    name: &'static str,
    generator: dim_agent_runtime::Generator,
) -> FiberView {
    let view = root.plugin(Producer {
        registry,
        scope,
        name,
        generator,
    });
    dim_agent_runtime::wait_active(
        root,
        std::slice::from_ref(&view),
        Duration::from_secs(5),
        &Default::default(),
    )
    .await
    .unwrap();
    view
}

/// Records where it is mounted and when it unloads.
struct Recorder {
    log: Log,
    identity: Key<SessionIdentity>,
}
impl Recorder {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for Recorder {
    fn name(&self) -> &str {
        "test.recorder"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            let (log, id) = (self.log.clone(), identity.session_id.clone());
            log.lock().unwrap().push(format!("mounted {id}"));
            ctx.effect(move || async move {
                log.lock().unwrap().push(format!("unmounted {id}"));
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}
fn recorder(log: &Log) -> dim_agent_runtime::Generator {
    let log = log.clone();
    Arc::new(move |mount| {
        Ok(Some(
            Box::new(Recorder::new(mount.names, log.clone())?) as Box<dyn DynPlugin>
        ))
    })
}

/// A branch part that records its branch.
struct BranchRecorder {
    log: Log,
    identity: Key<TimelineIdentity>,
}
impl BranchRecorder {
    fn new(names: &Names<'_>, log: Log) -> rutis::Result<Self> {
        Ok(Self {
            log,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for BranchRecorder {
    fn name(&self) -> &str {
        "test.branch-recorder"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.identity)?;
            self.log.lock().unwrap().push(format!(
                "branch {}/{}",
                identity.session_id, identity.branch_id
            ));
            Ok(Effect::Done)
        })
    }
}

/// A part whose `apply` waits for a gate (or its unload: rutis does not
/// abort an `apply`), or fails.
struct Gated(Option<Arc<Notify>>);
impl Plugin for Gated {
    type Deps = ();
    fn name(&self) -> &str {
        "test.gated"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        match &self.0 {
            Some(gate) => {
                let cancel = ctx.cancellation_token();
                tokio::select! {
                    _ = gate.notified() => {}
                    _ = cancel.cancelled() => {}
                }
                Ok(())
            }
            None => Err(rutis::Error::from(ErrorKind::PluginFailed(
                "gated part refused".into(),
            ))),
        }
    }
}

/// Whether `session` holds a copy of `name` that has not been disposed.
fn mounted(session: &Session, name: &str) -> bool {
    session
        .diagnostics()
        .unwrap()
        .plugins
        .iter()
        .any(|plugin| plugin.name == name && plugin.state != rutis::host::FiberState::Disposed)
}
async fn eventually(mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}
async fn close(agent: MinimalHost) {
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registered_part_reaches_open_and_later_sessions_and_leaves_with_its_plugin() {
    let (agent, registry) = placed(builder()).await;
    let root = agent.root().clone();
    let before = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    let log = Log::default();
    let producer = produce(
        &root,
        registry,
        PartScope::Session,
        "test.recorder",
        recorder(&log),
    )
    .await;
    let first = format!("mounted {}", before.id());
    eventually(|| log.lock().unwrap().contains(&first)).await;
    let after = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    let second = format!("mounted {}", after.id());
    eventually(|| log.lock().unwrap().contains(&second)).await;
    assert!(mounted(&before, "test.recorder") && mounted(&after, "test.recorder"));

    producer.dispose().await.unwrap();
    let entries = log.lock().unwrap().clone();
    for session in [&before, &after] {
        assert!(
            entries.contains(&format!("unmounted {}", session.id())),
            "{entries:?}"
        );
        assert!(!mounted(session, "test.recorder"));
    }
    let later = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!mounted(&later, "test.recorder"));
    assert!(
        !log.lock()
            .unwrap()
            .contains(&format!("mounted {}", later.id()))
    );
    close(agent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_generator_may_mount_nothing_for_an_instance() {
    let (agent, registry) = placed(builder()).await;
    let root = agent.root().clone();
    let log = Log::default();
    let inner = recorder(&log);
    let generator: dim_agent_runtime::Generator = Arc::new(move |mount| {
        if !mount.session.system_prompt.contains("recorded") {
            return Ok(None);
        }
        inner(mount)
    });
    let _producer = produce(
        &root,
        registry,
        PartScope::Session,
        "test.recorder",
        generator,
    )
    .await;
    let off = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    let on = agent
        .create_session(
            SessionConfig {
                system_prompt: "recorded".into(),
                ..SessionConfig::default()
            },
            Default::default(),
        )
        .await
        .unwrap();
    let mounted_on = format!("mounted {}", on.id());
    eventually(|| log.lock().unwrap().contains(&mounted_on)).await;
    assert!(mounted(&on, "test.recorder"));
    assert!(!mounted(&off, "test.recorder"));
    assert!(
        !log.lock()
            .unwrap()
            .contains(&format!("mounted {}", off.id()))
    );
    close(agent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_required_part_gates_publication_and_its_failure_fails_the_session() {
    let mut builder = builder();
    builder.require_part("test.gated");
    let (builder, mut registries) = hosted(builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let root = agent.root().clone();
    let creating = {
        let agent = agent.agent().clone();
        tokio::spawn(async move {
            agent
                .create_session(SessionConfig::default(), Default::default())
                .await
        })
    };
    let registry = registries.recv().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !creating.is_finished(),
        "published before its required part was registered"
    );
    let gate = Arc::new(Notify::new());
    let held = gate.clone();
    let producer = produce(
        &root,
        registry.clone(),
        PartScope::Session,
        "test.gated",
        Arc::new(move |_| {
            Ok(Some(
                Box::new(Gated(Some(held.clone()))) as Box<dyn DynPlugin>
            ))
        }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !creating.is_finished(),
        "published before its required part"
    );
    gate.notify_waiters();
    let session = creating.await.unwrap().unwrap();
    assert!(mounted(&session, "test.gated"));
    session.close().await.unwrap();
    producer.dispose().await.unwrap();

    let _failing = produce(
        &root,
        registry,
        PartScope::Session,
        "test.gated",
        Arc::new(|_| Ok(Some(Box::new(Gated(None)) as Box<dyn DynPlugin>))),
    )
    .await;
    let error = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .err()
        .expect("a failed required part fails its Session");
    assert!(error.to_string().contains("gated part refused"), "{error}");
    close(agent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_optional_part_settles_before_publication_and_its_failure_stays_local() {
    let (agent, registry) = placed(builder()).await;
    let root = agent.root().clone();
    let gate = Arc::new(Notify::new());
    let held = gate.clone();
    let _waiting = produce(
        &root,
        registry.clone(),
        PartScope::Session,
        "test.gated",
        Arc::new(move |_| {
            Ok(Some(
                Box::new(Gated(Some(held.clone()))) as Box<dyn DynPlugin>
            ))
        }),
    )
    .await;
    let _failing = produce(
        &root,
        registry,
        PartScope::Session,
        "test.failing",
        Arc::new(|_| Ok(Some(Box::new(Gated(None)) as Box<dyn DynPlugin>))),
    )
    .await;
    let creating = {
        let agent = agent.agent().clone();
        tokio::spawn(async move {
            agent
                .create_session(SessionConfig::default(), Default::default())
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !creating.is_finished(),
        "published before an optional part it opened with settled"
    );
    gate.notify_one();
    let session = tokio::time::timeout(Duration::from_secs(5), creating)
        .await
        .expect("a settled optional part releases publication")
        .unwrap()
        .expect("an optional part's failure does not fail its Session");
    let states: Vec<_> = session
        .diagnostics()
        .unwrap()
        .plugins
        .into_iter()
        .filter(|plugin| plugin.name == "test.gated")
        .map(|plugin| plugin.state)
        .collect();
    assert!(
        states.contains(&rutis::host::FiberState::Active),
        "{states:?}"
    );
    assert!(
        states.contains(&rutis::host::FiberState::Failed),
        "{states:?}"
    );
    close(agent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_part_mounts_in_each_branch() {
    let (agent, registry) = placed(builder()).await;
    let root = agent.root().clone();
    let log = Log::default();
    let branch_log = log.clone();
    let _producer = produce(
        &root,
        registry,
        PartScope::Branch,
        "test.branch-recorder",
        Arc::new(move |mount| {
            assert!(mount.branch_id.is_some());
            Ok(Some(
                Box::new(BranchRecorder::new(mount.names, branch_log.clone())?)
                    as Box<dyn DynPlugin>,
            ))
        }),
    )
    .await;
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    let main = format!("branch {}/main", session.id());
    eventually(|| log.lock().unwrap().contains(&main)).await;
    let child = session
        .branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: "child".into(),
                source: BranchSource::Host {
                    operation_id: "test-branch".into(),
                },
            },
        )
        .await
        .unwrap();
    let child = format!("branch {}/{child}", session.id());
    eventually(|| log.lock().unwrap().contains(&child)).await;
    close(agent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_of_one_builder_mount_only_their_own_plugins_parts() {
    let (builder, mut registries) = hosted(builder());
    let one = MinimalHost::start(&builder).await.unwrap();
    let registry = registries.recv().await.unwrap();
    let two = MinimalHost::start(&builder).await.unwrap();
    let other = registries.recv().await.unwrap();
    let first = one
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    let second = two
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&registry, &other), "each host has its own");
    assert!(!Arc::ptr_eq(&registry, &builder.parts()));
    let log = Log::default();
    let _producer = produce(
        one.root(),
        registry,
        PartScope::Session,
        "test.recorder",
        recorder(&log),
    )
    .await;
    let mounted_first = format!("mounted {}", first.id());
    eventually(|| log.lock().unwrap().contains(&mounted_first)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!mounted(&second, "test.recorder"));
    assert!(mounted(&second, "ToolRegistry"));
    one.close().await.unwrap();
    two.close().await.unwrap();
}
