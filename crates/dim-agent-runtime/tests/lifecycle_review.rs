//! Independent public regressions for construction ownership and close failures.
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, Feature, SessionConfig, testing::MinimalHost,
    wait_active,
};
use rutis::host::{CtxHost, ErrorKind};
use rutis::{Ctx, Plugin};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct SlowInitialization {
    entered: Arc<Notify>,
    cleaning: Arc<Notify>,
    release: Arc<Notify>,
}
impl Plugin for SlowInitialization {
    type Deps = ();
    fn name(&self) -> &str {
        "review.slow-initialization"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let cleaning = self.cleaning.clone();
        let release = self.release.clone();
        ctx.effect(|| async move {
            cleaning.notify_one();
            release.notified().await;
            Err(rutis::Error::from(ErrorKind::ServiceNotFound(
                "rollback resource failure".into(),
            )))
        })?;
        self.entered.notify_one();
        ctx.cancelled().await;
        Err(rutis::Error::from(ErrorKind::ServiceNotFound(
            "initialization cancelled".into(),
        )))
    }
}
fn framework_contains(error: &ErrorKind, message: &str) -> bool {
    match error {
        ErrorKind::ServiceNotFound(value) => value == message,
        ErrorKind::Aggregate { errors } => errors.iter().any(|e| framework_contains(e, message)),
        _ => false,
    }
}
fn contains(error: &AgentError, message: &str) -> bool {
    match error {
        AgentError::Framework(error) => framework_contains(error, message),
        AgentError::Multiple(errors) => errors.iter().any(|e| contains(e, message)),
        _ => false,
    }
}
fn cancelled(error: &AgentError) -> bool {
    match error {
        AgentError::Cancelled(_) => true,
        AgentError::Multiple(errors) => errors.iter().any(cancelled),
        _ => false,
    }
}
#[tokio::test]
async fn agent_close_joins_cancelled_construction_and_retains_async_rollback_failure() {
    let entered = Arc::new(Notify::new());
    let cleaning = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let setup = (entered.clone(), cleaning.clone(), release.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.require_feature(Feature::new("review.slow-initialization").session(
        "review.slow-initialization",
        move |_| {
            Ok(Box::new(SlowInitialization {
                entered: setup.0.clone(),
                cleaning: setup.1.clone(),
                release: setup.2.clone(),
            }))
        },
    ));
    let host = MinimalHost::start(&builder).await.unwrap();
    let agent = host.agent().clone();
    let owner = agent.clone();
    let create = tokio::spawn(async move { owner.create_session(SessionConfig::default()).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let mut close = tokio::spawn(agent.close());
    tokio::time::timeout(Duration::from_secs(2), cleaning.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut close)
            .await
            .is_err()
    );
    create.abort();
    release.notify_one();
    let error = tokio::time::timeout(Duration::from_secs(2), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(contains(&error, "rollback resource failure"), "{error:?}");
    assert!(
        !cancelled(&error),
        "the close reports no cancellation: {error:?}"
    );
    assert!(agent.sessions().is_empty());
    let repeated = agent.close().await.unwrap_err();
    assert!(contains(&repeated, "rollback resource failure"));
}

#[test]
fn agent_close_freezes_build_ownership_before_the_close_task_can_be_polled() {
    let owner_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let construction_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let entered = Arc::new(Notify::new());
    let cleaning = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let setup = (entered.clone(), cleaning.clone(), release.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.require_feature(Feature::new("review.slow-initialization").session(
        "review.slow-initialization",
        move |_| {
            Ok(Box::new(SlowInitialization {
                entered: setup.0.clone(),
                cleaning: setup.1.clone(),
                release: setup.2.clone(),
            }))
        },
    ));
    let host = construction_runtime
        .block_on(MinimalHost::start(&builder))
        .unwrap();
    let agent = host.agent().clone();
    let creator = agent.clone();
    let create = construction_runtime
        .spawn(async move { creator.create_session(SessionConfig::default()).await });
    construction_runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
    });
    let close = agent.close();
    construction_runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), cleaning.notified())
            .await
            .unwrap();
        release.notify_one();
        let error = tokio::time::timeout(Duration::from_secs(2), create)
            .await
            .unwrap()
            .unwrap()
            .err()
            .expect("cancelled initialization must fail");
        assert!(contains(&error, "rollback resource failure"), "{error:?}");
    });
    assert!(agent.sessions().is_empty());
    let error = owner_runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), close)
            .await
            .unwrap()
            .unwrap_err()
    });
    assert!(contains(&error, "rollback resource failure"), "{error:?}");
}

/// A service no row provides.
struct Unprovided;
/// Stays Pending: it injects `Unprovided`.
struct Dependent;
impl Plugin for Dependent {
    type Deps = Arc<Unprovided>;
    fn name(&self) -> &str {
        "review.dependent"
    }
    async fn apply(&self, _: &Ctx, _: Self::Deps) -> rutis::Result {
        Ok(())
    }
}
struct Failing;
impl Plugin for Failing {
    type Deps = ();
    fn name(&self) -> &str {
        "review.failing"
    }
    async fn apply(&self, _: &Ctx, (): Self::Deps) -> rutis::Result {
        Err(rutis::Error::from(ErrorKind::ServiceNotFound(
            "required row failed".into(),
        )))
    }
}

#[tokio::test]
async fn a_failed_row_ends_the_wait_while_an_earlier_row_stays_pending() {
    let root = Ctx::root().unwrap();
    let views = vec![root.plugin(Dependent), root.plugin(Failing)];
    let started = std::time::Instant::now();
    let error = wait_active(
        &root,
        &views,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(contains(&error, "required row failed"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    root.shutdown().await.unwrap();
}
