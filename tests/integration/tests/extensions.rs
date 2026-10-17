use dim_agent_runtime::*;
use dim_agent_types::BranchSource;
use dim_agent_types::InputMode;
use rutis::TypedPlugin;
use std::time::Duration;
use test_extension_a::Provider;
use test_extension_b::{Consumer, Snapshot};
fn builder() -> AgentBuilder {
    let mut b = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut b);
    dim_agent_capabilities::defaults::install(&mut b);
    b
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_cross_crate_capabilities_are_isolated_and_installed_in_every_branch() {
    let mut b = builder();
    b.timeline_plugin(|scope| Consumer::new(scope, true))
        .timeline_plugin(Provider::new);
    let agent = b.build();
    let one = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let two = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    one.submit("hello".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    one.idle("main").await.unwrap();
    assert_eq!(one.get::<Snapshot>("main").unwrap().unwrap().inputs, 1);
    assert_eq!(two.get::<Snapshot>("main").unwrap().unwrap().inputs, 0);
    let branch = one
        .branch(
            "main".into(),
            one.sequence("main").unwrap(),
            BranchOptions {
                prompt: "branch".into(),
                source: BranchSource::Host {
                    operation_id: "fork".into(),
                },
            },
        )
        .await
        .unwrap();
    one.idle(&branch).await.unwrap();
    let initial = one.get::<Snapshot>(&branch).unwrap().unwrap().inputs;
    one.submit("child".into(), InputMode::Queue, &branch)
        .await
        .unwrap();
    one.idle(&branch).await.unwrap();
    assert_eq!(
        one.get::<Snapshot>(&branch).unwrap().unwrap().inputs,
        initial + 1
    );
    assert_eq!(one.get::<Snapshot>("main").unwrap().unwrap().inputs, 1);
    let mut changes = one.on::<Snapshot>(&branch).unwrap().unwrap();
    one.submit("another".into(), InputMode::Queue, &branch)
        .await
        .unwrap();
    changes.changed().await.unwrap();
    assert_eq!(changes.borrow().inputs, initial + 2);
    let diag = one.branch_diagnostics(&branch).unwrap();
    let consumer = diag
        .plugins
        .iter()
        .find(|p| p.name == "ExternalConsumer")
        .unwrap();
    assert!(consumer.accesses.iter().all(|a| !a.external || a.declared));
    assert!(
        consumer
            .resolved_dependencies
            .iter()
            .any(|d| d.key.describe().contains("Counter"))
    );
    agent.close().await.unwrap();
    assert!(agent.sessions().is_empty());
}
#[tokio::test]
async fn missing_dependency_and_undeclared_read_fail_before_session_publication() {
    let mut missing = builder();
    missing
        .startup_timeout(Duration::from_millis(50))
        .timeline_plugin(|scope| Consumer::new(scope, true));
    let agent = missing.build();
    let error = agent
        .create_session(SessionConfig::default())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("ExternalConsumer"));
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();

    // Debug publication checks reject an owner returning with an unready child.
    if cfg!(debug_assertions) {
        for session_part in [true, false] {
            let mut b = builder();
            if session_part {
                b.session_plugin(|_| UnawaitedChild);
            } else {
                b.timeline_plugin(|_| UnawaitedChild);
            }
            let agent = b.build();
            let error = agent
                .create_session(SessionConfig::default())
                .await
                .err()
                .expect("required child prevents publication");
            assert!(error.to_string().contains("unawaited-child"), "{error}");
            assert!(agent.sessions().is_empty());
            agent.close().await.unwrap();
        }
    }
    // Scoped service reads are strict in every build.
    let mut undeclared = builder();
    undeclared
        .timeline_plugin(Provider::new)
        .timeline_plugin(|scope| Consumer::new(scope, false));
    let agent = undeclared.build();
    let error = agent
        .create_session(SessionConfig::default())
        .await
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("dependency not declared"),
        "{error}"
    );
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();
}

#[tokio::test]
async fn abandoned_first_creation_is_owned_until_agent_close_finishes() {
    let root = rutis::Ctx::root().unwrap();
    let agent = builder().build_in(&root);
    let mut creating = Box::pin(agent.create_session(SessionConfig::default()));
    assert!(futures_util::poll!(creating.as_mut()).is_pending());
    drop(creating);
    let (first, second) = tokio::join!(agent.close(), agent.close());
    first.unwrap();
    second.unwrap();
    assert!(agent.sessions().is_empty());
    assert_eq!(
        root.diagnostics().plugins.len(),
        1,
        "only the caller's root remains"
    );
    root.shutdown().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_continues_after_waiter_is_dropped_and_concurrent_closers_join() {
    let agent = builder().build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let first = session.close();
    drop(first);
    let (a, b) = tokio::join!(session.close(), session.close());
    a.unwrap();
    b.unwrap();
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();
}

#[test]
fn owning_agent_can_close_or_drop_outside_the_runtime_enter_guard() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let agent = builder().build();
    let session = runtime
        .block_on(agent.create_session(SessionConfig::default()))
        .unwrap();
    // Construct the close future before entering block_on.
    let close = agent.close();
    runtime.block_on(close).unwrap();
    runtime.block_on(session.close()).unwrap();
    let agent = builder().build();
    let session = runtime
        .block_on(agent.create_session(SessionConfig::default()))
        .unwrap();
    drop(agent);
    runtime.block_on(session.close()).unwrap();
}

struct FailingRollback {
    fail_apply: bool,
    cleanups: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl TypedPlugin for FailingRollback {
    type Deps = ();
    fn name(&self) -> &str {
        "test.failing-rollback"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            for _ in 0..if self.fail_apply { 2 } else { 1 } {
                let cleanups = self.cleanups.clone();
                ctx.effect(|| {
                    rutis::Effect::Disposer(Box::new(move || {
                        cleanups.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Err(rutis::CordisError::ServiceNotFound("cleanup-marker".into()))
                    }))
                })?;
            }
            if self.fail_apply {
                Err(rutis::CordisError::ServiceNotFound("build-marker".into()))
            } else {
                Ok(rutis::Effect::Done)
            }
        })
    }
}
impl Capability for FailingRollback {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test]
async fn initialization_preserves_each_distinct_cleanup_failure_once() {
    let cleanups = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let timeline = cleanups.clone();
    let session = cleanups.clone();
    let mut builder = builder();
    builder.timeline_plugin(move |_| FailingRollback {
        fail_apply: true,
        cleanups: timeline.clone(),
    });
    builder.session_plugin(move |_| FailingRollback {
        fail_apply: false,
        cleanups: session.clone(),
    });
    let agent = builder.build();
    let error = agent
        .create_session(SessionConfig::default())
        .await
        .err()
        .expect("initialization failed");
    let detail = format!("{error:?}");
    assert_eq!(cleanups.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(detail.matches("build-marker").count(), 1, "{detail}");
    // Equal text from different disposers must survive; repeated observation of
    // the very same failure through tree/root close must not multiply it.
    assert_eq!(detail.matches("cleanup-marker").count(), 3, "{detail}");
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();
}

struct Marker;
impl SessionService for Marker {}
/// Provides `Marker` under its Session's key and reads the marker of the
/// first Session that loaded a probe.
struct ScopeProbe {
    provide: Option<rutis::TypeKey>,
    read: std::sync::Arc<std::sync::Mutex<Option<rutis::TypeKey>>>,
    seen: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl TypedPlugin for ScopeProbe {
    type Deps = ();
    fn name(&self) -> &str {
        "test.scope-probe"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            if let Some(key) = &self.provide {
                ctx.provide_as(key.clone(), std::sync::Arc::new(Marker))?;
                self.read.lock().unwrap().get_or_insert(key.clone());
            }
            if let Some(key) = self.read.lock().unwrap().clone() {
                self.seen.store(
                    ctx.get_as::<Marker>(key).is_some(),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for ScopeProbe {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_services_stay_inside_their_session_and_unkeyed_ones_fail_publication() {
    let foreign = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut scoped = builder();
    let (read, observed) = (foreign.clone(), seen.clone());
    scoped.session_plugin(move |scope| ScopeProbe {
        provide: Some(scope.key::<Marker>()),
        read: read.clone(),
        seen: observed.clone(),
    });
    let agent = scoped.build();
    let first = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst));
    // The second Session reads the first Session's key: out of scope, not resolvable.
    let second = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert!(!seen.load(std::sync::atomic::Ordering::SeqCst));
    first.close().await.unwrap();
    second.close().await.unwrap();
    agent.close().await.unwrap();

    // Assembly validation runs in debug builds only.
    if !cfg!(debug_assertions) {
        return;
    }
    let mut unkeyed = builder();
    unkeyed.session_plugin(|_| ScopeProbe {
        provide: Some(rutis::TypeKey::of::<Marker>()),
        read: Default::default(),
        seen: Default::default(),
    });
    let agent = unkeyed.build();
    let error = agent
        .create_session(SessionConfig::default())
        .await
        .err()
        .expect("unkeyed scoped service");
    assert!(
        error.to_string().contains("without an instance key"),
        "{error}"
    );
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_session_and_branch_subtrees_leave_no_plugins_or_bindings() {
    let agent = builder().build();
    let mut baseline = None;
    for round in 0..20 {
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let branch = session
            .branch(
                "main".into(),
                0,
                BranchOptions {
                    prompt: "round".into(),
                    source: BranchSource::Host {
                        operation_id: format!("round-{round}"),
                    },
                },
            )
            .await
            .unwrap();
        session.idle(&branch).await.unwrap();
        session.close().await.unwrap();
        let diagnostics = agent.diagnostics().unwrap();
        let counts = (diagnostics.plugins.len(), diagnostics.bindings.len());
        assert_eq!(*baseline.get_or_insert(counts), counts, "round {round}");
    }
    // What remains is the Agent's own: the root, its loader, the Agent
    // instance and its anchor, and the loader's service.
    assert_eq!(baseline, Some((4, 1)));
    agent.close().await.unwrap();
}

struct NamedCapability {
    name: &'static str,
    bit: usize,
    installed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pending: bool,
}

struct UnawaitedChild;
impl TypedPlugin for UnawaitedChild {
    type Deps = ();
    fn name(&self) -> &str {
        "test.required-owner"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            ctx.plugin(capability_plugin(NamedCapability {
                name: "unawaited-child",
                bit: 0,
                installed: Default::default(),
                pending: true,
            }));
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for UnawaitedChild {
    fn keys(&self) -> KeysOf<Self> {}
}
impl Capability for NamedCapability {
    fn keys(&self) -> KeysOf<Self> {
        self.pending.then_some(())
    }
}
impl TypedPlugin for NamedCapability {
    type Deps = When<rutis::Gate<NamedCapability>>;
    fn name(&self) -> &str {
        self.name
    }
    fn apply<'a>(
        &'a self,
        _: &'a rutis::Ctx,
        _: Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            self.installed
                .fetch_or(self.bit, std::sync::atomic::Ordering::SeqCst);
            Ok(rutis::Effect::Done)
        })
    }
}

#[tokio::test]
async fn plugin_names_cannot_replace_other_capabilities_or_loader_groups() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let installed = Arc::new(AtomicUsize::new(0));
    let mut b = builder();
    for (i, name) in [
        "Probe",
        "Probe#2",
        "Probe",
        "branch",
        "session",
        "agent",
        "AgentAnchor",
        "AgentScope",
        "trace",
    ]
    .into_iter()
    .enumerate()
    {
        let installed = installed.clone();
        b.session_plugin(move |_| NamedCapability {
            name,
            bit: 1 << i,
            installed: installed.clone(),
            pending: false,
        });
    }
    b.session_extension("trace", "Probe", |_, installed: &Arc<AtomicUsize>| {
        NamedCapability {
            name: "Probe",
            bit: 1 << 9,
            installed: installed.clone(),
            pending: false,
        }
    });
    let agent = b.build();
    agent
        .create_session_with_extension(SessionConfig::default(), "trace", installed.clone())
        .await
        .unwrap();
    assert_eq!(installed.load(Ordering::SeqCst), (1 << 10) - 1);
    agent.close().await.unwrap();
}

#[tokio::test]
async fn factory_and_extension_failures_reject_the_session_instead_of_omitting_capabilities() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for panic_at in [0, 1] {
        let mut b = builder();
        let calls = Arc::new(AtomicUsize::new(0));
        b.session_plugin(move |_| {
            assert_ne!(
                calls.fetch_add(1, Ordering::SeqCst),
                panic_at,
                "factory failure"
            );
            NamedCapability {
                name: "factory",
                bit: 1,
                installed: Arc::default(),
                pending: false,
            }
        });
        let agent = b.build();
        let error = agent
            .create_session(SessionConfig::default())
            .await
            .err()
            .expect("a required factory failure rejects creation");
        if panic_at == 1 {
            assert!(
                error.to_string().contains("plugin factory panicked"),
                "{error}"
            );
        }
        assert!(agent.sessions().is_empty());
        agent.close().await.unwrap();
    }
    let mut b = builder();
    b.session_extension("extension", "declared", |_, _: &u32| NamedCapability {
        name: "actual",
        bit: 1,
        installed: Arc::default(),
        pending: false,
    });
    let agent = b.build();
    let missing_value = agent
        .create_session_with_extension(SessionConfig::default(), "extension", "wrong value type")
        .await;
    assert!(missing_value.is_err());
    let wrong_name = agent
        .create_session_with_extension(SessionConfig::default(), "extension", 1_u32)
        .await;
    assert!(wrong_name.is_err());
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();

    let agent = builder().build();
    assert!(
        agent
            .create_session_with_extension(SessionConfig::default(), "missing", ())
            .await
            .is_err()
    );
    agent.close().await.unwrap();

    let mut b = builder();
    b.startup_timeout(Duration::from_millis(50));
    b.session_extension("pending", "pending", |_, _: &()| NamedCapability {
        name: "pending",
        bit: 1,
        installed: Arc::default(),
        pending: true,
    });
    let agent = b.build();
    assert!(
        agent
            .create_session_with_extension(SessionConfig::default(), "pending", ())
            .await
            .is_err()
    );
    assert!(agent.sessions().is_empty());
    agent.close().await.unwrap();
}

/// A Session service provided once per name, as `sessionMethods` is once per
/// namespace.
struct Named(&'static str);
impl SessionService for Named {}
impl PublicService for Named {}
struct NamedProvider(SessionScope);
impl TypedPlugin for NamedProvider {
    type Deps = ();
    fn name(&self) -> &str {
        "test.named"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            for name in ["one", "two"] {
                ctx.provide_as(
                    self.0.named_key::<Named>(name),
                    std::sync::Arc::new(Named(name)),
                )?;
            }
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for NamedProvider {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_facade_reads_a_named_session_service_by_name() {
    let mut b = builder();
    b.session_plugin(|scope| NamedProvider(*scope));
    let agent = b.build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    for name in ["one", "two"] {
        assert_eq!(
            session.session_named_service::<Named>(name).unwrap().0,
            name
        );
    }
    // A name nobody provides reads as reloading; telling an absent name from
    // a reloading provider is the caller's (methods contract §9.1).
    assert!(matches!(
        session.session_named_service::<Named>("three"),
        Err(AgentError::Reloading(_))
    ));
    session.close().await.unwrap();
    assert!(matches!(
        session.session_named_service::<Named>("one"),
        Err(AgentError::Closed(_))
    ));
    agent.close().await.unwrap();
}

struct BranchNamed(&'static str);
impl BranchService for BranchNamed {}
impl PublicService for BranchNamed {}

struct TreeCommandsProbe {
    scope: SessionScope,
    send: tokio::sync::mpsc::UnboundedSender<
        std::sync::Arc<dim_agent_capabilities::timeline_tree::TreeCommands>,
    >,
}
impl TypedPlugin for TreeCommandsProbe {
    type Deps = (InSession<dim_agent_capabilities::timeline_tree::TreeCommands>,);
    fn name(&self) -> &str {
        "test.branch-access"
    }
    fn apply<'a>(
        &'a self,
        _: &'a rutis::Ctx,
        (InSession(commands),): Self::Deps,
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            self.send.send(commands).unwrap();
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for TreeCommandsProbe {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

struct BranchNamedProvider(BranchScope);
impl TypedPlugin for BranchNamedProvider {
    type Deps = ();
    fn name(&self) -> &str {
        "test.branch-named"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): (),
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            ctx.provide_as(
                self.0.key::<BranchNamed>(),
                std::sync::Arc::new(BranchNamed("typed")),
            )?;
            for name in ["one", "two"] {
                ctx.provide_as(
                    rutis::TypeKey::keyed_dynamic::<BranchNamed>(name)
                        .with_instance(self.0.instance),
                    std::sync::Arc::new(BranchNamed(name)),
                )?;
            }
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for BranchNamedProvider {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_facade_named_reads_are_isolated_and_follow_provider_reload() {
    use std::sync::Arc;

    let root = rutis::Ctx::root().unwrap();
    let mut b = builder();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    b.session_plugin(move |scope| TreeCommandsProbe {
        scope: *scope,
        send: send.clone(),
    });
    b.timeline_plugin(|scope| BranchNamedProvider(*scope));
    let agent = b.build_in(&root);
    let one = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let tree = receive.recv().await.unwrap();
    let _two = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let other_tree = receive.recv().await.unwrap();
    let child = one
        .branch(
            "main".into(),
            one.sequence("main").unwrap(),
            BranchOptions {
                prompt: "child".into(),
                source: BranchSource::Host {
                    operation_id: "named-services".into(),
                },
            },
        )
        .await
        .unwrap();
    one.idle(&child).await.unwrap();
    let main = tree.get("main").unwrap();
    let child = tree.get(&child).unwrap();
    let other = other_tree.get("main").unwrap();
    for branch in [&main, &child, &other] {
        assert_eq!(branch.service::<BranchNamed>().unwrap().0, "typed");
        for name in ["one", "two"] {
            assert_eq!(branch.named_service::<BranchNamed>(name).unwrap().0, name);
        }
        assert!(!Arc::ptr_eq(
            &branch.named_service::<BranchNamed>("one").unwrap(),
            &branch.named_service::<BranchNamed>("two").unwrap(),
        ));
        assert!(matches!(
            branch.named_service::<BranchNamed>("absent"),
            Err(AgentError::Reloading(_))
        ));
    }
    let before = main.named_service::<BranchNamed>("one").unwrap();
    let child_before = child.named_service::<BranchNamed>("one").unwrap();
    let other_before = other.named_service::<BranchNamed>("one").unwrap();
    assert!(!Arc::ptr_eq(&before, &child_before));
    assert!(!Arc::ptr_eq(&before, &other_before));

    let provider = main
        .diagnostics()
        .plugins
        .into_iter()
        .find(|part| part.name == "test.branch-named")
        .unwrap()
        .id;
    let provider = root.view(provider).unwrap();
    provider.restart().await.unwrap();
    let after = main.named_service::<BranchNamed>("one").unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(after.0, "one");
    assert!(Arc::ptr_eq(
        &child_before,
        &child.named_service::<BranchNamed>("one").unwrap()
    ));
    assert!(Arc::ptr_eq(
        &other_before,
        &other.named_service::<BranchNamed>("one").unwrap()
    ));

    provider.dispose().await.unwrap();
    assert!(matches!(
        main.named_service::<BranchNamed>("one"),
        Err(AgentError::Reloading(_))
    ));
    assert!(matches!(
        main.service::<BranchNamed>(),
        Err(AgentError::Reloading(_))
    ));
    assert!(Arc::ptr_eq(
        &other_before,
        &other.named_service::<BranchNamed>("one").unwrap()
    ));
    agent.close().await.unwrap();
    root.shutdown().await.unwrap();
}

struct AssemblyAnchor(tokio::sync::mpsc::UnboundedSender<rutis::Ctx>);
impl TypedPlugin for AssemblyAnchor {
    type Deps = ();
    fn name(&self) -> &str {
        "test.assembly-anchor"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): (),
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            self.0.send(ctx.clone()).unwrap();
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for AssemblyAnchor {
    fn keys(&self) -> KeysOf<Self> {}
}

struct OptionalResource(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl TypedPlugin for OptionalResource {
    type Deps = ();
    fn name(&self) -> &str {
        "test.optional-resource"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): (),
    ) -> rutis::BoxFuture<'a, std::result::Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            let closed = self.0.clone();
            ctx.effect(move || {
                rutis::Effect::Disposer(Box::new(move || {
                    closed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }))
            })?;
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for OptionalResource {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn optional_loader_rows_do_not_block_create_restore_or_initial_input_and_close_with_the_scope()
 {
    use rutis_loader::{Builtins, Chain, Layer, LoaderOptions, LoaderPlugin, RuntimeResolver};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let root = rutis::Ctx::root_with(tokio::runtime::Handle::current());
    let store = Arc::new(MemoryTimelineStore::default());
    let closed = Arc::new(AtomicUsize::new(0));
    let rolled_back = Arc::new(AtomicUsize::new(0));
    let mut builtins = Builtins::new();
    // A display-name collision must not change which row the builder requires.
    let pending = register_capability(&mut builtins, || NamedCapability {
        name: "SessionScope",
        bit: 0,
        installed: Arc::default(),
        pending: true,
    });
    let failed = register_capability(&mut builtins, {
        let rolled_back = rolled_back.clone();
        move || FailingRollback {
            fail_apply: true,
            cleanups: rolled_back.clone(),
        }
    });
    let resource = register_capability(&mut builtins, {
        let closed = closed.clone();
        move || OptionalResource(closed.clone())
    });
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let anchor = register_capability(&mut builtins, move || AssemblyAnchor(send.clone()));
    let mut b = builder();
    b.timeline_store(store);
    let mut session_row = b.install(&mut builtins, &root, []).await.unwrap();
    let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let runtime = rutis_bridge::runtime::LocalRuntime::node(
        workspace.join("runtime/node_modules/@arcships/rutis-runtime"),
        workspace.join("runtime/package.json"),
    );
    let external = |prefix: &str| {
        vec![
            json!({"id":format!("{prefix}-pending"),"name":pending}),
            json!({"id":format!("{prefix}-failed"),"name":failed}),
            json!({"id":format!("{prefix}-resource"),"name":resource}),
            json!({"id":format!("{prefix}-unresolved"),"name":"missing-optional-module"}),
            json!({"id":format!("{prefix}-runtime"),"name":workspace.join("runtime/spikes/session-part.ts")}),
        ]
    };
    let parts = session_row["config"].as_array_mut().unwrap();
    let branch = parts
        .iter_mut()
        .find(|row| row["id"] == BRANCH_GROUP)
        .unwrap();
    branch["config"]
        .as_array_mut()
        .unwrap()
        .extend(external("optional-branch"));
    parts.extend(external("optional-session"));
    let resolver = RuntimeResolver::node(runtime.handle());
    let plugin = LoaderPlugin::new(
        Chain::new().with(builtins).with(resolver),
        LoaderOptions::default(),
    );
    let loader = plugin.handle();
    (&root.plugin(plugin)).await.unwrap();
    loader.reconcile(vec![Layer::new("fixture", serde_json::from_value(json!([{"insert":[
        {"id":AGENT_GROUP,"group":true,"instanced":true,"config":[{"id":anchor,"name":anchor},session_row]}
    ]}])).unwrap())],None).await.unwrap();
    let instance = loader.create_instance(&root, AGENT_GROUP).await.unwrap();
    let anchor_ctx = receive.recv().await.unwrap();
    let mut saved: Option<String> = None;
    for round in 0..2 {
        let agent = b.build_at(loader.clone(), &anchor_ctx);
        let session = match &saved {
            None => agent
                .create_session(SessionConfig::default())
                .await
                .unwrap(),
            Some(id) => agent.restore_session(id.clone()).await.unwrap(),
        };
        saved = Some(session.id().to_owned());
        let entries: Vec<_> = loader
            .entries()
            .into_iter()
            .filter(|row| row.id.starts_with("optional-") && row.instance.is_some())
            .collect();
        assert_eq!(entries.len(), 10);
        for suffix in ["pending", "runtime"] {
            assert!(
                entries
                    .iter()
                    .filter(|row| row.id.ends_with(suffix))
                    .all(|row| row.view.as_ref().unwrap().state().state
                        == rutis::FiberState::Pending)
            );
        }
        assert!(
            entries
                .iter()
                .filter(|row| row.id.ends_with("failed"))
                .all(|row| row.view.as_ref().unwrap().state().state == rutis::FiberState::Failed)
        );
        assert!(
            entries
                .iter()
                .filter(|row| row.id.ends_with("unresolved"))
                .all(|row| row.view.is_none())
        );
        session
            .submit("first input".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(
            session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(event.kind, dim_agent_types::EventKind::TurnEnd(_)))
        );
        agent.close().await.unwrap();
        assert!(
            entries
                .iter()
                .filter_map(|row| row.view.as_ref())
                .all(|view| view.state().state == rutis::FiberState::Disposed)
        );
        assert_eq!(closed.load(Ordering::SeqCst), (round + 1) * 2);
        assert_eq!(rolled_back.load(Ordering::SeqCst), (round + 1) * 4);
    }
    instance.view.shutdown().await.unwrap();
    root.shutdown().await.unwrap();
}
