use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, BRANCH_GROUP, Dispatcher, Feature, Names, SessionConfig, Timeline,
    names, testing::MinimalHost,
};
use dim_agent_types::{ContentBlock, EventKind, InputMode};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[tokio::test]
async fn interrupt_commits_before_later_input_without_waiting_for_dispatch() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    let interrupt = session.interrupt("main", "reload".into()).unwrap();
    let input = session
        .submit("new work".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    assert!(interrupt < input.sequence);
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(events.iter().any(|event| event.sequence == interrupt
        && matches!(&event.kind, EventKind::StopInterrupt(data) if data.reason == "reload")));
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::InputConsumed(data) if data.input_id == input.id)));
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::TurnEnd(data) if matches!(data.outcome, dim_agent_types::TurnOutcome::Completed { .. }))));
    agent.close().await.unwrap();
    assert!(session.interrupt("main", "too late".into()).is_err());
}

struct Registered;
#[derive(Clone)]
struct ExternalWork {
    entered: Arc<Notify>,
    cancelling: Arc<Notify>,
    release: Arc<Notify>,
}
struct ExternalWorkPlugin {
    probe: ExternalWork,
    dispatcher: Key<Dispatcher>,
    registered: Key<Registered>,
}
impl ExternalWorkPlugin {
    fn new(names: &Names<'_>, probe: ExternalWork) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            dispatcher: names.key()?,
            registered: names.key()?,
        })
    }
}
impl DynPlugin for ExternalWorkPlugin {
    fn name(&self) -> &str {
        "ExternalWork"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let tasks = TaskTracker::new();
            let cancel = CancellationToken::new();
            let cleanup_tasks = tasks.clone();
            let cleanup_cancel = cancel.clone();
            ctx.effect(|| async move {
                cleanup_cancel.cancel();
                cleanup_tasks.close();
                cleanup_tasks.wait().await;
                Ok(())
            })?;
            let once = AtomicBool::new(false);
            let probe = self.probe.clone();
            let handle = ctx.handle().clone();
            dispatcher.subscribe(
                ctx,
                "ExternalWork",
                &["input/message", "stop/interrupt"],
                move |event| {
                    match &event.kind {
                        EventKind::InputMessage(_) if !once.swap(true, Ordering::AcqRel) => {
                            let probe = probe.clone();
                            let cancel = cancel.clone();
                            tasks.spawn_on(
                                async move {
                                    probe.entered.notify_one();
                                    cancel.cancelled().await;
                                    probe.cancelling.notify_one();
                                    probe.release.notified().await;
                                },
                                &handle,
                            );
                        }
                        EventKind::StopInterrupt(_) => cancel.cancel(),
                        _ => {}
                    }
                    Box::pin(async { Ok(()) })
                },
            )?;
            ctx.provide_at(self.registered.clone(), Arc::new(Registered))?;
            Ok(Effect::Done)
        })
    }
}
struct LaterSubscriber {
    seen: watch::Sender<Option<u64>>,
    dispatcher: Key<Dispatcher>,
    registered: Key<Registered>,
}
impl LaterSubscriber {
    fn new(names: &Names<'_>, seen: watch::Sender<Option<u64>>) -> rutis::Result<Self> {
        Ok(Self {
            seen,
            dispatcher: names.key()?,
            registered: names.key()?,
        })
    }
}
impl DynPlugin for LaterSubscriber {
    fn name(&self) -> &str {
        "LaterSubscriber"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase(), self.registered.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let seen = self.seen.clone();
            dispatcher.subscribe(ctx, "LaterSubscriber", &["stop/interrupt"], move |event| {
                seen.send_replace(Some(event.sequence));
                Box::pin(async { Ok(()) })
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_capability_receives_interrupt_and_owns_cleanup_outside_the_stop() {
    let probe = ExternalWork {
        entered: Arc::new(Notify::new()),
        cancelling: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let (send, mut observed) = watch::channel(None);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    names::register::<Registered>(builder.catalog_mut(), Some(BRANCH_GROUP));
    let factory = probe.clone();
    builder
        .require_feature(
            Feature::new("ExternalWork").branch("ExternalWork", move |names| {
                Ok(Box::new(ExternalWorkPlugin::new(names, factory.clone())?))
            }),
        )
        .require_feature(
            Feature::new("LaterSubscriber").branch("LaterSubscriber", move |names| {
                Ok(Box::new(LaterSubscriber::new(names, send.clone())?))
            }),
        );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    session
        .submit("hello".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), probe.entered.notified())
        .await
        .unwrap();
    let owner = session.clone();
    let first = tokio::spawn(async move { owner.stop("main", "user interrupt".into()).await });
    tokio::time::timeout(Duration::from_secs(2), observed.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), probe.cancelling.notified())
        .await
        .unwrap();
    let sequence = observed.borrow().unwrap();
    let snapshot = chain_events(&session, "main", ..).await.unwrap();
    assert!(snapshot.iter().any(|event| event.sequence == sequence && matches!(&event.kind, EventKind::StopInterrupt(data) if data.reason == "user interrupt")));
    tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!session.state("main").unwrap().stopping);
    probe.release.notify_one();
    assert_eq!(
        chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
            .count(),
        1
    );
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    assert!(chain_events(&session, "main", ..).await.unwrap().iter().any(
        |event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("continue"))
    ));
    agent.close().await.unwrap();
}

struct OversizedInterrupt {
    cleaned: Arc<AtomicBool>,
    timeline: Key<Timeline>,
}
impl OversizedInterrupt {
    fn new(names: &Names<'_>, cleaned: Arc<AtomicBool>) -> rutis::Result<Self> {
        Ok(Self {
            cleaned,
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for OversizedInterrupt {
    fn name(&self) -> &str {
        "OversizedInterrupt"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let cleaned = self.cleaned.clone();
            ctx.effect(|| async move {
                cleaned.store(true, Ordering::Release);
                Ok(())
            })?;
            timeline.before(
                ctx,
                "OversizedInterrupt",
                &["stop/interrupt"],
                |mut draft| {
                    draft.tags = Some(vec!["x".repeat(32 * 1024)]);
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_interrupt_and_later_input_need_no_byte_budget() {
    let cleaned = Arc::new(AtomicBool::new(false));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    let disposed = cleaned.clone();
    builder.require_feature(
        Feature::new("OversizedInterrupt").branch("OversizedInterrupt", move |names| {
            Ok(Box::new(OversizedInterrupt::new(names, disposed.clone())?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    session.stop("main", "pause".into()).await.unwrap();
    session
        .submit("resume input".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(events.iter().any(
        |event| matches!(&event.kind, EventKind::InputMessage(data) if matches!(
            data.user.as_ref().map(|user| user.content.as_slice()),
            Some([ContentBlock::Text { text }]) if text == "resume input"
        ))
    ));
    assert!(events.iter().any(|event| {
        matches!(event.kind, EventKind::StopInterrupt(_))
            && event
                .tags
                .as_ref()
                .is_some_and(|tags| tags[0].len() == 32 * 1024)
    }));
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::InputApplied(_)))
    );
    session.close().await.unwrap();
    assert!(cleaned.load(Ordering::Acquire));
    agent.close().await.unwrap();
}
