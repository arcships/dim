use dim_agent_runtime::{
    AgentError, MemoryTimelineStore, SessionConfig, Timeline, TimelineKey, TimelineStore,
};
use dim_agent_types::*;
use rutis::Ctx;
use std::sync::Arc;
fn input(text: &str) -> EventDraft {
    EventKind::InputMessage(InputMessageData::text(text, None)).into()
}
#[tokio::test]
async fn before_pipeline_is_atomic_and_branch_snapshots_share_only_fixed_prefix() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let _first = timeline
        .before(&ctx, "first", &["input/message"], |mut draft| {
            if let EventKind::InputMessage(data) = &mut draft.kind {
                let ContentBlock::Text { text } =
                    &mut data.user.as_mut().expect("text input").content[0]
                else {
                    panic!("expected text input");
                };
                text.push_str(" changed");
            }
            Ok(draft)
        })
        .unwrap();
    let failing = timeline
        .before(&ctx, "second", &["input/message"], |_| {
            Err(AgentError::fault("extension", "failed"))
        })
        .unwrap();
    let original = timeline.append(input("one")).unwrap();
    assert_eq!(original.kind, input("one").kind);
    assert!(timeline.trace(1).await.unwrap().is_none());
    failing.dispose().await.unwrap();
    let changed = timeline.append(input("two")).unwrap();
    assert_eq!(changed.kind, input("two changed").kind);
    assert_eq!(timeline.trace(2).await.unwrap().unwrap().steps.len(), 1);
    let unchanged = timeline
        .before(&ctx, "identity", &["input/message"], Ok)
        .unwrap();
    let traced = timeline.append(input("trace")).unwrap();
    let trace = timeline.trace(traced.sequence).await.unwrap().unwrap();
    let handlers: Vec<_> = trace
        .steps
        .iter()
        .map(|step| step.handler.as_str())
        .collect();
    assert_eq!(handlers, ["identity", "first"]);
    assert!(trace.steps[0].result.is_none());
    unchanged.dispose().await.unwrap();
    timeline
        .append(
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Sequence { sequence: 1 },
                source: BranchSource::Host {
                    operation_id: "fork".into(),
                },
                branch: "branch".into(),
                prompt: "branch prompt".into(),
            })
            .into(),
        )
        .unwrap();
    let branch = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "branch".into(),
        },
        vec![("main".into(), 1)],
        store,
    );
    timeline.append(input("later")).unwrap();
    let local = branch.append(input("branch")).unwrap();
    let snapshot = branch.history(None).await.unwrap();
    assert_eq!(snapshot.len(), 2);
    assert!(Arc::ptr_eq(&snapshot[0], &original));
    assert_eq!(local.sequence, 1);
}
#[tokio::test]
async fn history_has_no_byte_quota_and_atomic_cuts_stay_fixed() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    );
    let large = "x".repeat(2 * 1024 * 1024);
    timeline.append(input(&large)).unwrap();
    let batch = timeline
        .append_batch_registered(vec![input("first"), input("second")], |_| Ok(()))
        .unwrap();
    assert_eq!(timeline.sequence(), 3);
    assert!(matches!(
        timeline.history(Some(batch[0].sequence)).await,
        Err(AgentError::InvalidInput(_))
    ));
    assert!(matches!(
        timeline.local(..=batch[0].sequence).await,
        Err(AgentError::InvalidInput(_))
    ));
    assert!(matches!(
        timeline.context(batch[0].sequence).await,
        Err(AgentError::InvalidInput(_))
    ));
    assert_eq!(
        timeline
            .history(Some(batch[1].sequence))
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(timeline.local(2..=3).await.unwrap().len(), 2);
    assert!(matches!(
        timeline.history(Some(4)).await,
        Err(AgentError::InvalidInput(_))
    ));
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    assert!(matches!(
        timeline.append(input("closed")),
        Err(AgentError::Closed(_))
    ));
}
// Dispatcher behavior is exercised through installed capabilities in integration
// tests; subscription ownership is released by the owning fiber on unload.
#[tokio::test]
async fn before_can_read_history_and_rejects_nested_append() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    timeline.append(input("existing")).unwrap();
    let reader = timeline.clone();
    let _hook = timeline
        .before(&ctx, "history-reader", &["input/message"], move |draft| {
            assert_eq!(reader.sequence(), 1);
            assert!(reader.append(input("recursive")).is_err());
            Ok(draft)
        })
        .unwrap();
    let event = timeline.append(input("second")).unwrap();
    assert_eq!(event.sequence, 2);
    assert_eq!(timeline.sequence(), 2);
}

#[tokio::test]
async fn failed_registration_commits_nothing() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    timeline.append(input("existing")).unwrap();
    let registered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = registered.clone();
    let error = timeline
        .append_registered(input("rejected"), |event| {
            seen.store(
                event.sequence.try_into().unwrap(),
                std::sync::atomic::Ordering::SeqCst,
            );
            Err(AgentError::InvalidInput("registration rejected".into()))
        })
        .unwrap_err();
    assert!(matches!(error, AgentError::InvalidInput(_)));
    assert_eq!(registered.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(timeline.sequence(), 1);
    // The next append reuses the sequence the rejected attempt never published.
    let event = timeline
        .append_registered(input("accepted"), |_| Ok(()))
        .unwrap();
    assert_eq!(event.sequence, 2);
    assert_eq!(timeline.sequence(), 2);
}

#[tokio::test]
async fn unknown_storage_keys_cannot_create_sessions_or_branches() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let main = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let empty_head = store.head(&main).await.unwrap();
    assert_eq!(empty_head, 0);
    let empty_range = 1..=empty_head;
    assert!(
        store
            .read(&main, empty_range.clone())
            .await
            .unwrap()
            .is_empty()
    );

    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(ctx.clone(), main.clone(), vec![], store.clone());
    timeline.append(input("known")).unwrap();
    let record = store.read(&main, 1..=1).await.unwrap();
    for key in [
        TimelineKey {
            session_id: "missing".into(),
            branch_id: "main".into(),
        },
        TimelineKey {
            session_id: "session".into(),
            branch_id: "missing".into(),
        },
    ] {
        assert!(matches!(
            store.head(&key).await,
            Err(AgentError::Storage(_))
        ));
        assert!(matches!(
            store.read(&key, 1..=1).await,
            Err(AgentError::Storage(_))
        ));
        assert!(matches!(
            store.read(&key, empty_range.clone()).await,
            Err(AgentError::Storage(_))
        ));
        assert!(matches!(
            store.append(key.clone(), record.clone(), Box::new(|_| {})),
            Err(AgentError::Storage(_))
        ));
        assert!(matches!(
            store.head(&key).await,
            Err(AgentError::Storage(_))
        ));
    }
    assert!(store.session_config("missing").await.unwrap().is_none());
    assert_eq!(store.branches("session").await.unwrap().len(), 1);
    assert_eq!(store.head(&main).await.unwrap(), 1);
}

/// Finalizers of a closing scope still commit their terminal events; the
/// unloading `before` hooks no longer run, so the original is committed.
#[tokio::test]
async fn closing_scope_commits_the_original_event_without_before() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    );
    let _hook = timeline
        .before(&ctx, "change", &["input/message"], |mut draft| {
            if let EventKind::InputMessage(data) = &mut draft.kind {
                let ContentBlock::Text { text } =
                    &mut data.user.as_mut().expect("text input").content[0]
                else {
                    panic!("expected text input");
                };
                text.push_str(" changed");
            }
            Ok(draft)
        })
        .unwrap();
    let closing = ctx.shutdown();
    let event = timeline.append(input("late")).unwrap();
    assert_eq!(event.kind, input("late").kind);
    assert!(timeline.trace(event.sequence).await.unwrap().is_none());
    closing.await.unwrap();
}

#[tokio::test]
async fn before_keys_accept_a_domain_wildcard() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let timeline = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    );
    assert!(matches!(
        timeline.before(&ctx, "bad", &["input/*/x"], Ok),
        Err(AgentError::InvalidInput(_))
    ));
    let _hook = timeline
        .before(&ctx, "domain", &["input/*"], |mut draft| {
            draft.tags = Some(vec!["seen".into()]);
            Ok(draft)
        })
        .unwrap();
    let input = timeline.append(input("tagged")).unwrap();
    assert_eq!(input.tags, Some(vec!["seen".into()]));
    let other = timeline
        .append(
            EventKind::StopInterrupt(StopInterruptData {
                reason: "other domain".into(),
                scope: InterruptScope::Turn,
            })
            .into(),
        )
        .unwrap();
    assert_eq!(other.tags, None);
}
