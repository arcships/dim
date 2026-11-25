use dim_agent_capabilities::defaults;
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

async fn soon<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("public flow completed")
}

struct PanicFlowProbe {
    delivered: Arc<AtomicUsize>,
    dispatcher: Arc<OnceLock<Arc<Dispatcher>>>,
    dispatcher_key: Key<Dispatcher>,
}
impl PanicFlowProbe {
    fn new(
        names: &Names<'_>,
        delivered: Arc<AtomicUsize>,
        dispatcher: Arc<OnceLock<Arc<Dispatcher>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            delivered,
            dispatcher,
            dispatcher_key: names.key()?,
        })
    }
}
impl DynPlugin for PanicFlowProbe {
    fn name(&self) -> &str {
        "test.panic-flow-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher_key.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        let counter = self.delivered.clone();
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher_key)?;
            dispatcher.subscribe(
                ctx,
                "test.counting-subscriber",
                &["input/message"],
                move |_| {
                    let counter = counter.clone();
                    Box::pin(async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                },
            )?;
            dispatcher.subscribe(ctx, "test.panic-subscriber", &["input/message"], |_| {
                Box::pin(async { panic!("subscription handler exploded") })
            })?;
            assert!(self.dispatcher.set(dispatcher).is_ok());
            Ok(Effect::Done)
        })
    }
}

/// Both listeners must be registered in one `apply`; independent plugins have
/// no initialization order without an injects dependency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_panic_is_attributed_and_freezes_the_dispatch_cursor() {
    let delivered = Arc::new(AtomicUsize::new(0));
    let dispatcher = Arc::new(OnceLock::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let counted = delivered.clone();
    let captured = dispatcher.clone();
    builder.require_feature(Feature::new("test.panic-flow-probe").branch(
        "test.panic-flow-probe",
        move |names| {
            Ok(Box::new(PanicFlowProbe::new(
                names,
                counted.clone(),
                captured.clone(),
            )?))
        },
    ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default(), Default::default()))
        .await
        .unwrap();
    let input = soon(session.submit("hello".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    let error = soon(session.idle("main")).await.unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("test.panic-subscriber"),
        "failure attributed to the subscriber: {message}"
    );
    assert!(
        message.contains("subscription handler exploded"),
        "panic payload preserved: {message}"
    );
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
    assert_eq!(dispatcher.get().unwrap().sequence(), input.sequence - 1);
    let rejected = session
        .submit("second".into(), InputMode::Queue, "main")
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("test.panic-subscriber"));
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
    match soon(agent.close()).await {
        Ok(()) => {}
        Err(error) => assert!(error.to_string().contains("test.panic-subscriber")),
    }
    assert_eq!(dispatcher.get().unwrap().sequence(), input.sequence - 1);
}

struct DomainProbe {
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    dispatcher: Key<Dispatcher>,
}
impl DomainProbe {
    fn new(names: &Names<'_>, seen: Arc<std::sync::Mutex<Vec<String>>>) -> rutis::Result<Self> {
        Ok(Self {
            seen,
            dispatcher: names.key()?,
        })
    }
}
impl DynPlugin for DomainProbe {
    fn name(&self) -> &str {
        "test.domain-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        let seen = self.seen.clone();
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let rejected = dispatcher.subscribe(ctx, "test.bad-wildcard", &["input*"], |_| {
                Box::pin(async { Ok(()) })
            });
            assert!(matches!(rejected, Err(AgentError::InvalidInput(_))));
            dispatcher.subscribe(ctx, "test.input-domain", &["input/*"], move |event| {
                seen.lock().unwrap().push(event.kind.key().to_owned());
                Box::pin(async { Ok(()) })
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn domain_wildcard_subscribes_to_every_key_in_that_domain() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let recorded = seen.clone();
    builder.require_feature(
        Feature::new("test.domain-probe").branch("test.domain-probe", move |names| {
            Ok(Box::new(DomainProbe::new(names, recorded.clone())?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default(), Default::default()))
        .await
        .unwrap();
    soon(session.submit("hello".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        ["input/message", "input/applied", "input/consumed"]
    );
    soon(agent.close()).await.unwrap();
}

/// Reads Loop's published state while handling each event after Loop did.
struct PhaseProbe {
    seen: Arc<std::sync::Mutex<Vec<(String, LoopPhase)>>>,
    dispatcher: Key<Dispatcher>,
    state: Key<StateCell<LoopSnapshot>>,
}
impl DynPlugin for PhaseProbe {
    fn name(&self) -> &str {
        "test.phase-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase(), self.state.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        let seen = self.seen.clone();
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let state = ctx.require_at(&self.state)?;
            dispatcher.subscribe(ctx, "test.phase-probe", &["*"], move |event| {
                seen.lock()
                    .unwrap()
                    .push((event.kind.key().to_owned(), state.get().phase));
                Box::pin(async { Ok(()) })
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_publishes_running_only_once_turn_start_is_dispatched() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let recorded = seen.clone();
    builder.require_feature(Feature::new("test.phase-probe").branch(
        "test.phase-probe",
        move |names| {
            Ok(Box::new(PhaseProbe {
                seen: recorded.clone(),
                dispatcher: names.key()?,
                state: names.state_cell()?,
            }))
        },
    ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default(), Default::default()))
        .await
        .unwrap();
    soon(session.submit("hello".into(), InputMode::Queue, "main"))
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let seen = seen.lock().unwrap().clone();
    let start = seen
        .iter()
        .position(|(key, _)| key == "turn/start")
        .unwrap();
    assert!(
        seen[..start]
            .iter()
            .all(|(_, phase)| *phase == LoopPhase::Idle)
    );
    assert_eq!(seen[start].1, LoopPhase::Running);
    let end = seen.iter().position(|(key, _)| key == "turn/end").unwrap();
    assert_eq!(seen[end].1, LoopPhase::Idle);
    soon(agent.close()).await.unwrap();
}
