use dim_agent_capabilities::{defaults, history::project_messages};
use dim_agent_runtime::*;
use dim_agent_types::*;
use std::time::Duration;
mod support;
use support::Events;

fn agent() -> Agent {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.build()
}
/// An Agent on a root the test listens on.
fn agent_in(root: &rutis::Ctx) -> Agent {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.build_in(root)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_tools_and_observable_state_follow_the_public_flow() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let agent = agent();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        session.submit("hello".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::Text(data) if data.delta == "Echo: hello")));
        assert!(matches!(events.last().unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })));
        assert_eq!(session.get::<LoopSnapshot>("main").unwrap().unwrap().phase, LoopPhase::Idle);
        session.submit("tool:echo {\"text\":\"tool-result\"}".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::ToolResult(data) if data.results[0].result.content == ToolContent::Text("tool-result".into()))));
        let messages = project_messages(&events).unwrap();
        assert!(messages.iter().any(|m| m.role == Role::ToolResult));
        session.close().await.unwrap();
        session.close().await.unwrap();
        assert!(agent.sessions().is_empty());
        agent.close().await.unwrap();
    }).await.expect("public flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_ends_the_turn_and_a_waiting_queue_input_runs_next() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = support::root();
        let agent = agent_in(&root);
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let mut events = Events::follow(&root, &session, "main", 0).await;
        session
            .submit(
                "tool:wait {\"millis\":60000}".into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        while !matches!(events.next().await.unwrap().kind, EventKind::ToolCall(_)) {}
        let queued = session
            .submit("queued".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.stop("main", "test stop".into()).await.unwrap();
        session.idle("main").await.unwrap();
        let after = session.history("main", None).await.unwrap();
        let position = |predicate: &dyn Fn(&EventKind) -> bool| {
            after
                .iter()
                .position(|event| predicate(&event.kind))
                .unwrap()
        };
        let cancelled = position(&|kind| {
            matches!(
                kind,
                EventKind::TurnEnd(TurnEndData {
                    outcome: TurnOutcome::Cancelled { .. },
                    ..
                })
            )
        });
        let applied =
            position(&|kind| matches!(kind, EventKind::InputApplied(d) if d.input_id == queued.id));
        let consumed = position(
            &|kind| matches!(kind, EventKind::InputConsumed(d) if d.input_id == queued.id),
        );
        assert!(cancelled < applied && applied < consumed);
        assert_eq!(
            after
                .iter()
                .filter(|e| matches!(e.kind, EventKind::TurnEnd(_)))
                .count(),
            2
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("stop and the waiting queue input completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_tool_reuses_capabilities_without_replaying_ancestor_inputs() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let agent = agent();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("ancestor".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let prefix = session.history("main", None).await.unwrap();
        session
            .submit(
                "tool:branch {\"prompt\":\"child\"}".into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let branches = session.branches();
        let child = branches.iter().find(|branch| branch.id != "main").unwrap();
        session.idle(&child.id).await.unwrap();
        let child_events = session.local(&child.id, ..).await.unwrap();
        assert!(matches!(
            child_events.first().unwrap().kind,
            EventKind::BranchStart(_)
        ));
        assert_eq!(
            child_events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::InputConsumed(_)))
                .count(),
            1
        );
        assert!(
            child_events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::Text(d) if d.delta.starts_with("Echo: <dim-branch-task parent=\"main\">") && d.delta.contains("delegated this task to you:\nchild")))
        );
        let history = session.history(&child.id, None).await.unwrap();
        assert!(history.starts_with(&prefix));
        assert_eq!(
            session
                .get::<LoopSnapshot>(&child.id)
                .unwrap()
                .unwrap()
                .phase,
            LoopPhase::Idle
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("branch flow completed");
}

#[tokio::test]
async fn closed_default_memory_sessions_release_history_but_shared_stores_retain_it() {
    for shared in [false, true] {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        if shared {
            builder.timeline_store(std::sync::Arc::new(MemoryTimelineStore::default()));
        }
        let agent = builder.build();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("retention policy".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let id = session.id().to_owned();
        session.close().await.unwrap();
        drop(session);
        let restored = agent.restore_session(id).await;
        if shared {
            let session = restored.unwrap();
            assert!(session.history("main", None).await.unwrap().iter().any(|event|
                matches!(&event.kind, EventKind::Text(data) if data.delta.contains("retention policy"))));
        } else {
            assert!(matches!(restored, Err(AgentError::Storage(_))));
        }
        agent.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_idle_release_checks_never_reject_a_running_turn() {
    const TURNS: usize = 60;
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = support::root();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(std::sync::Arc::new(MemoryTimelineStore::default()));
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        let agent = builder.build_in(&root);
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let mut events = Events::follow(&root, &session, "main", 0).await;
        for index in 0..=TURNS {
            let millis = if index == TURNS { 500 } else { 1 };
            session
                .submit(
                    format!("tool:wait {{\"millis\":{millis},\"text\":\"w{index}\"}}"),
                    InputMode::Queue,
                    "main",
                )
                .await
                .unwrap();
        }
        let done = tokio_util::sync::CancellationToken::new();
        let mut observed = Events::follow(&root, &session, "main", 0).await;
        let checks = tokio::spawn({
            let session = session.clone();
            let done = done.clone();
            async move {
                let mut refused = 0;
                while !done.is_cancelled() {
                    tokio::select! {
                        _ = done.cancelled() => break,
                        event = observed.next() => if event.is_none() { break },
                    }
                    assert!(!session.unload_if_idle().await.unwrap());
                    refused += 1;
                }
                refused
            }
        });
        let mut calls = 0;
        while calls <= TURNS {
            if matches!(events.next().await.unwrap().kind, EventKind::ToolCall(_)) {
                calls += 1;
            }
        }
        done.cancel();
        assert!(checks.await.unwrap() > 0);
        session.idle("main").await.unwrap();
        assert!(session.failure().is_none());
        let outcomes: Vec<_> = session
            .history("main", None)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|event| match &event.kind {
                EventKind::TurnEnd(end) => Some(end.outcome.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(outcomes.len(), TURNS + 1);
        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome, TurnOutcome::Completed { .. })),
            "{outcomes:?}"
        );
        assert!(session.unload_if_idle().await.unwrap());
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_idle_release_reopens_the_branch_for_the_running_turn() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = support::root();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(std::sync::Arc::new(MemoryTimelineStore::default()));
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        let agent = builder.build_in(&root);
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let mut events = Events::follow(&root, &session, "main", 0).await;
        session
            .submit(
                "tool:wait {\"millis\":200,\"text\":\"waited\"}".into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        while !matches!(events.next().await.unwrap().kind, EventKind::ToolCall(_)) {}
        assert!(!session.unload_if_idle().await.unwrap());
        session.idle("main").await.unwrap();
        assert!(matches!(
            session
                .history("main", None)
                .await
                .unwrap()
                .last()
                .unwrap()
                .kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        ));
        assert!(session.unload_if_idle().await.unwrap());
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}
