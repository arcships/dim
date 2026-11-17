//! SessionLifecycle follows publication and close admission (AGENTS rule 1,
//! lifecycle §3): Session parts start deferred work only for a published
//! Session and end it as soon as close starts. Its activity holds veto an idle
//! release, never a close (idle unload §5.1, R1).
mod local_support;

use dim_agent_runtime::*;
use dim_agent_types::InputMode;
use local_support::*;
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_close_during_assembly_never_activates_the_session() {
    let dirs = Dirs::new();
    let hold = Arc::new(AssemblyHold::default());
    let probe = Arc::new(LifecycleProbe::default());
    let env = env(
        &dirs,
        Options {
            hold: Some(hold.clone()),
            probe: Some(probe.clone()),
            ..Options::default()
        },
    )
    .await;
    let cwd = dirs.dir("work");
    hold.arm();
    let agent = env.agent.clone();
    let config = SessionConfig {
        cwd: cwd.to_string_lossy().into_owned(),
        tools: Some(vec!["skill".into(), "gate".into()]),
        ..SessionConfig::default()
    };
    let creating = tokio::spawn(async move { agent.create_session(config).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), hold.entered.notified())
        .await
        .expect("assembly reached the hold");
    probe.wait_for("applied", 1).await;
    // The Agent's close starts at the call; the host's close is a future.
    let closing = env.agent.agent().close();
    hold.release.notify_one();
    assert!(matches!(
        creating.await.unwrap(),
        Err(AgentError::Cancelled(_))
    ));
    closing.await.unwrap();
    probe.wait_for("inactive", 1).await;
    assert_eq!(probe.count("active"), 0);
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_close_reports_closing_to_every_session_at_once() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = env(
        &dirs,
        Options {
            probe: Some(probe.clone()),
            ..Options::default()
        },
    )
    .await;
    let cwd = dirs.dir("work");
    let _first = env.session(&cwd, "").await.unwrap();
    let _second = env.session(&cwd, "").await.unwrap();
    probe.wait_for("active", 2).await;
    probe.hold_next_close();
    let closing = env.agent.agent().close();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        probe.close_held.notified(),
    )
    .await
    .expect("a Session close reached the hold");
    probe.wait_for("closing", 2).await;
    probe.release_close.notify_one();
    closing.await.unwrap();
    env.agent.close().await.unwrap();
}

/// An Agent whose Sessions an idle release can unload: an explicit store.
async fn releasable(dirs: &Dirs, probe: &Arc<LifecycleProbe>) -> Env {
    env(
        dirs,
        Options {
            store: Some(Arc::new(MemoryTimelineStore::default())),
            probe: Some(probe.clone()),
            ..Options::default()
        },
    )
    .await
}

/// A published, idle Session and the lifecycle its parts inject.
async fn idle_session(
    env: &Env,
    dirs: &Dirs,
    probe: &LifecycleProbe,
) -> (Session, Arc<SessionLifecycle>) {
    let session = env.session(&dirs.dir("work"), "").await.unwrap();
    session.idle("main").await.unwrap();
    let lifecycle = probe.lifecycle(session.id());
    (session, lifecycle)
}

async fn within<T>(what: &str, work: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_release_waits_until_every_hold_is_dropped() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    let (session, lifecycle) = idle_session(&env, &dirs, &probe).await;
    let first = lifecycle.take_hold().unwrap();
    let second = lifecycle.take_hold().unwrap();
    within("idle", session.idle("main")).await.unwrap();
    assert!(!session.unload_if_idle().await.unwrap());
    drop(first);
    assert!(!session.unload_if_idle().await.unwrap());
    tokio::spawn(async move { drop(second) }).await.unwrap();
    assert!(session.unload_if_idle().await.unwrap());
    within("release", session.closed()).await.unwrap();
    assert!(lifecycle.take_hold().is_none());
    assert!(env.agent.sessions().is_empty());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_hold_is_taken_once_a_release_is_under_way() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    let (session, lifecycle) = idle_session(&env, &dirs, &probe).await;
    probe.hold_next_close();
    let releasing = tokio::spawn({
        let session = session.clone();
        async move { session.unload_if_idle().await }
    });
    within("release start", probe.close_held.notified()).await;
    assert!(lifecycle.take_hold().is_none());
    assert!(!session.unload_if_idle().await.unwrap());
    probe.release_close.notify_one();
    assert!(within("release", releasing).await.unwrap().unwrap());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_released_idle_pause_restores_input_and_hold_admission() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    let (session, lifecycle) = idle_session(&env, &dirs, &probe).await;
    let held = lifecycle.take_hold().unwrap();
    assert!(session.pause_if_idle().is_none());
    drop(held);
    let paused = session.pause_if_idle().expect("idle Session pauses");
    assert!(lifecycle.take_hold().is_none());
    assert!(session.pause_if_idle().is_none());
    assert!(
        session
            .submit("not admitted".into(), InputMode::Queue, "main")
            .await
            .is_err()
    );
    drop(paused);
    let held = lifecycle.take_hold().expect("rollback restores holds");
    session
        .submit("admitted after rollback".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    within("idle", session.idle("main")).await.unwrap();
    assert!(!session.unload_if_idle().await.unwrap());
    drop(held);
    assert!(session.unload_if_idle().await.unwrap());
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_the_idle_unload_waiter_does_not_resume_admission() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    let (session, lifecycle) = idle_session(&env, &dirs, &probe).await;
    probe.hold_next_close();
    let releasing = tokio::spawn({
        let session = session.clone();
        async move { session.unload_if_idle().await }
    });
    within("release start", probe.close_held.notified()).await;
    releasing.abort();
    assert!(releasing.await.unwrap_err().is_cancelled());
    assert!(lifecycle.take_hold().is_none());
    assert!(
        session
            .submit("not admitted".into(), InputMode::Queue, "main")
            .await
            .is_err()
    );
    probe.release_close.notify_one();
    within("close", session.closed()).await.unwrap();
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_ignores_holds_and_refuses_new_ones() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    let (session, lifecycle) = idle_session(&env, &dirs, &probe).await;
    let held = lifecycle.take_hold().unwrap();
    probe.hold_next_close();
    let closing = session.close();
    within("close start", probe.close_held.notified()).await;
    assert!(lifecycle.take_hold().is_none());
    assert!(!session.unload_if_idle().await.unwrap());
    probe.release_close.notify_one();
    within("close", closing).await.unwrap();
    drop(held);
    env.agent.close().await.unwrap();
}

/// The hold check and the release decision are one step: while an owner keeps
/// taking and dropping holds, a racing release never starts under a hold, and
/// once it starts no hold is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn holds_racing_an_idle_release_never_overlap_it() {
    const ROUNDS: usize = 10;
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    for _ in 0..ROUNDS {
        let (session, lifecycle) = idle_session(&env, &dirs, &probe).await;
        let start = Arc::new(tokio::sync::Barrier::new(2));
        let owner = tokio::spawn({
            let (session, start) = (session.clone(), start.clone());
            async move {
                start.wait().await;
                while let Some(hold) = lifecycle.take_hold() {
                    tokio::task::yield_now().await;
                    assert!(!session.is_unloading(), "a hold overlapped the release");
                    drop(hold);
                    tokio::task::yield_now().await;
                }
            }
        });
        start.wait().await;
        within("release decision", async {
            while !session.unload_if_idle().await.unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        within("owner", owner).await.unwrap();
        within("release", session.closed()).await.unwrap();
    }
    env.agent.close().await.unwrap();
}

/// A branch closed on its own ends only itself (lifecycle §5). While its close
/// still settles it keeps the Session from an idle release; once closed it
/// has nothing left to release, and the idle Session unloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_child_branch_no_longer_refuses_an_idle_release() {
    let dirs = Dirs::new();
    let probe = Arc::new(LifecycleProbe::default());
    let env = releasable(&dirs, &probe).await;
    let (session, _) = idle_session(&env, &dirs, &probe).await;
    env.model.push(Step::Tool("gate", serde_json::json!({})));
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "child".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .unwrap();
    within("gate", env.gate.entered.notified()).await;
    let mut closing = probe.tree(session.id()).get(&child).unwrap().close();
    assert!(!session.unload_if_idle().await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut closing)
            .await
            .is_err(),
        "the child closed before its gate opened"
    );
    env.gate.release.notify_one();
    within("child close", closing).await.unwrap();
    assert!(session.state(&child).unwrap().closed);
    assert_eq!(session.branches().len(), 2);
    within("idle", session.idle("main")).await.unwrap();
    assert!(session.unload_if_idle().await.unwrap());
    within("release", session.closed()).await.unwrap();
    env.agent.close().await.unwrap();
}
