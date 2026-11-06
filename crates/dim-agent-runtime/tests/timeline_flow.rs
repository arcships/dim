use dim_agent_runtime::testing::timeline_events;
use dim_agent_runtime::{
    AgentError, BoxFuture, MemoryTimelineStore, PublishCallback, Result, SessionConfig,
    StoredBranch, StoredEvent, Timeline, TimelineKey, TimelineQueryScope, TimelineStore,
    TimelineTrace,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::CtxHost;
use std::{
    ops::RangeInclusive,
    sync::{Arc, Mutex},
};
fn input(text: &str) -> EventDraft {
    EventKind::InputMessage(InputMessageData::text(text, None)).into()
}

/// Counts every segment read and every query from the store, in call order.
#[derive(Default)]
struct Reads {
    memory: MemoryTimelineStore,
    segments: Mutex<Vec<(String, RangeInclusive<u64>)>>,
    queries: Mutex<Vec<(String, u64, u64)>>,
}
impl TimelineStore for Reads {
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(id)
    }
    fn branches<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn batch_end<'a>(&'a self, key: &'a TimelineKey, sequence: u64) -> BoxFuture<'a, Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        self.memory.append(key, events, publish)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        Box::pin(async move {
            for (branch, range) in &segments {
                self.segments
                    .lock()
                    .unwrap()
                    .push((branch.clone(), range.clone()));
            }
            self.memory.read_segments(session_id, segments).await
        })
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        self.memory.latest_snapshot(key, through, interpreter)
    }
    fn query<'a>(
        &'a self,
        key: &'a TimelineKey,
        query: &'a dim_agent_runtime::TimelineQuery,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<dim_agent_runtime::TimelineQueryPage>> {
        self.queries
            .lock()
            .unwrap()
            .push((key.branch_id.clone(), from, through));
        self.memory.query(key, query, from, through)
    }
}

/// Decodes a checkpoint whose resume is the sequence held in the snapshot
/// state, on `main`, with an event id the store does not hold: the read keeps
/// the recorded position.
fn checkpoint_resume(
    snapshot: &RecoverySnapshot,
    _anchor: &AgentEvent,
) -> Option<ProjectionCheckpoint> {
    Some(ProjectionCheckpoint {
        resume: ResumePoint {
            branch_id: "main".into(),
            sequence: snapshot.state.parse().ok()?,
            event_id: "missing".into(),
        },
        state: ProjectionCheckpointState::default(),
    })
}
/// Decodes a checkpoint whose resume names the event id held in the snapshot
/// state, before a colon, and a position that need not be that event's.
fn checkpoint_resume_by_id(
    snapshot: &RecoverySnapshot,
    _anchor: &AgentEvent,
) -> Option<ProjectionCheckpoint> {
    let (event_id, sequence) = snapshot.state.split_once(':')?;
    Some(ProjectionCheckpoint {
        resume: ResumePoint {
            branch_id: "main".into(),
            sequence: sequence.parse().ok()?,
            event_id: event_id.into(),
        },
        state: ProjectionCheckpointState::default(),
    })
}
#[tokio::test]
async fn producer_preassigned_event_id_is_the_first_delta_part_id() {
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
    let _rewrite = timeline
        .before(&ctx, "rewrite", &["input/message"], |draft| {
            Ok(EventDraft::new(draft.kind))
        })
        .unwrap();
    let event = timeline
        .append(input("first part").with_id("part-1"))
        .unwrap();
    assert_eq!(event.id, "part-1");
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
    let main = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let original = timeline.append(input("one")).unwrap();
    assert_eq!(original.kind, input("one").kind);
    assert!(store.trace(&main, 1).await.unwrap().is_none());
    let boundary = timeline_events(&timeline, TimelineQueryScope::Chain, ..=original.sequence)
        .await
        .unwrap();
    assert_eq!(boundary.len(), 1);
    assert_eq!(boundary[0].kind, input("one").kind);
    failing.dispose().await.unwrap();
    let changed = timeline.append(input("two")).unwrap();
    assert_eq!(changed.kind, input("two changed").kind);
    assert_eq!(store.trace(&main, 2).await.unwrap().unwrap().steps.len(), 1);
    let unchanged = timeline
        .before(&ctx, "identity", &["input/message"], Ok)
        .unwrap();
    let traced = timeline.append(input("trace")).unwrap();
    let trace = store.trace(&main, traced.sequence).await.unwrap().unwrap();
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
    let snapshot = timeline_events(&branch, TimelineQueryScope::Chain, ..)
        .await
        .unwrap();
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
        timeline.batch_end(batch[0].sequence).await,
        Err(AgentError::InvalidInput(_))
    ));
    assert_eq!(
        timeline_events(&timeline, TimelineQueryScope::Branch, ..)
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(timeline.batch_end(batch[1].sequence).await.is_ok());
    assert_eq!(
        timeline_events(&timeline, TimelineQueryScope::Branch, 2..=3)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(matches!(
        timeline.bound(4),
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
    let event = timeline
        .append_registered(input("accepted"), |_| Ok(()))
        .unwrap();
    assert_eq!(event.sequence, 2);
    assert_eq!(timeline.sequence(), 2);
}

#[tokio::test]
async fn panicking_before_keeps_the_original_fact_and_the_branch_usable() {
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
        .before(&ctx, "panicking-before", &["input/message"], |_| {
            panic!("injected before panic")
        })
        .unwrap();
    let original = input("original");
    let first = timeline.append(original.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&first.kind).unwrap(),
        serde_json::to_value(original.kind).unwrap()
    );
    assert_eq!(first.sequence, 1);
    let next = input("after panic");
    let second = timeline.append(next.clone()).unwrap();
    assert_eq!(second.sequence, 2);
    assert_eq!(
        serde_json::to_value(second.kind.clone()).unwrap(),
        serde_json::to_value(next.kind).unwrap()
    );
    assert_eq!(
        timeline_events(&timeline, TimelineQueryScope::Branch, ..)
            .await
            .unwrap()
            .len(),
        2
    );
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
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
        store.clone(),
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
    assert!(
        store
            .trace(
                &TimelineKey {
                    session_id: "session".into(),
                    branch_id: "main".into(),
                },
                event.sequence,
            )
            .await
            .unwrap()
            .is_none()
    );
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

/// A checkpoint selection reads the anchor as one event and never a range
/// that spans it to the cut, so the tail is not decoded twice.
#[tokio::test]
async fn a_checkpoint_selection_decodes_one_anchor_event() {
    let store = Arc::new(Reads::default());
    store
        .memory
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let writer = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    writer
        .append_batch_registered(
            (1..=50)
                .map(|index| input(&format!("prefix {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let anchor = writer.append(input("anchor")).unwrap();
    writer
        .save_snapshot(RecoverySnapshot {
            sequence: anchor.sequence,
            event_id: anchor.id.clone(),
            interpreter: "test/anchor".into(),
            state: "40".into(),
        })
        .await
        .unwrap();
    writer
        .append_batch_registered(
            (1..=1000)
                .map(|index| input(&format!("tail {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let reader = Timeline::restore(Ctx::root().unwrap(), key, vec![], store.clone())
        .await
        .unwrap();
    let through = reader.committed();
    assert_eq!(through, 1051);
    store.segments.lock().unwrap().clear();
    store.queries.lock().unwrap().clear();
    let checkpoint = reader
        .checkpoint(through, "test/anchor", checkpoint_resume)
        .await
        .unwrap();
    assert!(checkpoint.is_some());
    assert_eq!(
        store.queries.lock().unwrap().first().cloned(),
        Some(("main".into(), 51, 51)),
        "the anchor check reads the anchor event"
    );
    assert_eq!(
        *store.segments.lock().unwrap(),
        [],
        "the anchor check reads no segment"
    );
}

/// A live branch serves the anchor from its hot window: the anchor check makes
/// no store read, and the checkpoint is used.
#[tokio::test]
async fn a_checkpoint_anchored_in_the_hot_window_is_used() {
    let store = Arc::new(Reads::default());
    store
        .memory
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let writer = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    writer
        .append_batch_registered(
            (1..=50)
                .map(|index| input(&format!("prefix {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let branch = Timeline::restore(Ctx::root().unwrap(), key, vec![], store.clone())
        .await
        .unwrap();
    let anchor = branch.append(input("anchor")).unwrap();
    branch
        .save_snapshot(RecoverySnapshot {
            sequence: anchor.sequence,
            event_id: anchor.id.clone(),
            interpreter: "test/anchor".into(),
            state: "40".into(),
        })
        .await
        .unwrap();
    branch
        .append_batch_registered(
            (1..=1000)
                .map(|index| input(&format!("tail {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let through = branch.committed();
    assert_eq!(through, 1051);
    store.segments.lock().unwrap().clear();
    store.queries.lock().unwrap().clear();
    let checkpoint = branch
        .checkpoint(through, "test/anchor", checkpoint_resume)
        .await
        .unwrap();
    assert!(checkpoint.is_some(), "the checkpoint is used");
    assert_eq!(
        *store.segments.lock().unwrap(),
        [],
        "the anchor comes from the hot window: the selection reads no segment"
    );
    assert!(
        store
            .queries
            .lock()
            .unwrap()
            .iter()
            .all(|(_, from, _)| *from != anchor.sequence),
        "the anchor comes from the hot window: no query reads it"
    );
}

/// An anchor row inside an atomic batch keeps its checkpoint: the anchor is
/// one event, so it is read at its sequence like any other.
#[tokio::test]
async fn a_checkpoint_anchored_inside_an_atomic_batch_is_still_used() {
    let store = Arc::new(Reads::default());
    store
        .memory
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let writer = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    writer
        .append_batch_registered(
            (1..=50)
                .map(|index| input(&format!("prefix {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let batch = writer
        .append_batch_registered(vec![input("anchor"), input("after")], |_| Ok(()))
        .unwrap();
    writer
        .save_snapshot(RecoverySnapshot {
            sequence: batch[0].sequence,
            event_id: batch[0].id.clone(),
            interpreter: "test/anchor".into(),
            state: "40".into(),
        })
        .await
        .unwrap();
    let reader = Timeline::restore(Ctx::root().unwrap(), key, vec![], store.clone())
        .await
        .unwrap();
    store.segments.lock().unwrap().clear();
    store.queries.lock().unwrap().clear();
    let checkpoint = reader
        .checkpoint(52, "test/anchor", checkpoint_resume)
        .await
        .unwrap()
        .expect("a mid-batch anchor still selects its checkpoint");
    assert_eq!(checkpoint.resume.sequence, 40);
    assert_eq!(
        store.queries.lock().unwrap().first().cloned(),
        Some(("main".into(), 51, 51)),
        "the anchor check reads the anchor event alone"
    );
    assert_eq!(
        *store.segments.lock().unwrap(),
        [],
        "the anchor check reads no segment"
    );
}

/// A child branch whose first event is inside a multi-event batch anchors a
/// checkpoint at it: the restore reads that event alone, never the branch's
/// prefix to its head.
#[tokio::test]
async fn a_first_event_anchor_inside_an_atomic_batch_is_read_alone() {
    let store = Arc::new(Reads::default());
    store
        .memory
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let main = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    main.append_batch_registered(
        (1..=50)
            .map(|index| input(&format!("prefix {index}")))
            .collect(),
        |_| Ok(()),
    )
    .unwrap();
    main.append(branch_create("child", 50)).unwrap();
    let child_key = TimelineKey {
        session_id: "session".into(),
        branch_id: "child".into(),
    };
    let child = Timeline::new(
        Ctx::root().unwrap(),
        child_key.clone(),
        vec![("main".into(), 50)],
        store.clone(),
    );
    let batch = child
        .append_batch_registered(vec![input("anchor"), input("after")], |_| Ok(()))
        .unwrap();
    child
        .save_snapshot(RecoverySnapshot {
            sequence: batch[0].sequence,
            event_id: batch[0].id.clone(),
            interpreter: "test/anchor".into(),
            state: "1".into(),
        })
        .await
        .unwrap();
    child
        .append_batch_registered(
            (1..=1000)
                .map(|index| input(&format!("tail {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let reader = Timeline::restore(
        Ctx::root().unwrap(),
        child_key,
        vec![("main".into(), 50)],
        store.clone(),
    )
    .await
    .unwrap();
    let through = reader.committed();
    assert_eq!((batch[0].sequence, through), (1, 1002));
    store.segments.lock().unwrap().clear();
    store.queries.lock().unwrap().clear();
    let checkpoint = reader
        .checkpoint(through, "test/anchor", checkpoint_resume)
        .await
        .unwrap()
        .expect("a first-event anchor still selects its checkpoint");
    assert_eq!(
        checkpoint.resume.sequence, 1,
        "the resume is the ancestor's"
    );
    assert_eq!(
        store.queries.lock().unwrap().first().cloned(),
        Some(("child".into(), 1, 1)),
        "the anchor check reads the anchor event alone"
    );
    assert_eq!(
        *store.segments.lock().unwrap(),
        [],
        "the anchor check reads no segment"
    );
}

/// The resume event the checkpoint names by id is the position the read
/// begins at; the position stored beside it is only used when the store holds
/// no such id.
#[tokio::test]
async fn a_checkpoint_resumes_at_the_event_its_id_names() {
    let store = Arc::new(Reads::default());
    store
        .memory
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let writer = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    let prefix = writer
        .append_batch_registered(
            (1..=50)
                .map(|index| input(&format!("prefix {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let resume = prefix[39].clone();
    let anchor = writer.append(input("anchor")).unwrap();
    writer
        .save_snapshot(RecoverySnapshot {
            sequence: anchor.sequence,
            event_id: anchor.id.clone(),
            interpreter: "test/anchor".into(),
            state: format!("{}:1", resume.id),
        })
        .await
        .unwrap();
    writer
        .append_batch_registered(
            (1..=1000)
                .map(|index| input(&format!("tail {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let reader = Timeline::restore(Ctx::root().unwrap(), key, vec![], store.clone())
        .await
        .unwrap();
    let through = reader.committed();
    let checkpoint = reader
        .checkpoint(through, "test/anchor", checkpoint_resume_by_id)
        .await
        .unwrap()
        .expect("the checkpoint is usable");
    assert_eq!(
        (
            checkpoint.resume.event_id.as_str(),
            checkpoint.resume.sequence
        ),
        (resume.id.as_str(), resume.sequence),
        "the resume event the id names is resolved, not the stored position"
    );
}

/// A resume id no stored event carries leaves the recorded position in use.
#[tokio::test]
async fn a_resume_id_the_store_does_not_hold_keeps_the_recorded_position() {
    let store = Arc::new(Reads::default());
    store
        .memory
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let writer = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    writer
        .append_batch_registered(
            (1..=50)
                .map(|index| input(&format!("prefix {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let anchor = writer.append(input("anchor")).unwrap();
    writer
        .save_snapshot(RecoverySnapshot {
            sequence: anchor.sequence,
            event_id: anchor.id.clone(),
            interpreter: "test/anchor".into(),
            state: "missing:40".into(),
        })
        .await
        .unwrap();
    writer
        .append_batch_registered(
            (1..=1000)
                .map(|index| input(&format!("tail {index}")))
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let reader = Timeline::restore(Ctx::root().unwrap(), key, vec![], store.clone())
        .await
        .unwrap();
    let through = reader.committed();
    let checkpoint = reader
        .checkpoint(through, "test/anchor", checkpoint_resume_by_id)
        .await
        .unwrap()
        .expect("the checkpoint is usable");
    assert_eq!(checkpoint.resume.sequence, 40);
}

/// A query over `scope` at its head, accepting every fact.
fn branch_query(scope: dim_agent_runtime::TimelineQueryScope) -> dim_agent_runtime::TimelineQuery {
    dim_agent_runtime::TimelineQuery {
        scope,
        include_publishing: false,
        from_event: None,
        from_sequence: None,
        order_by: Default::default(),
        direction: Default::default(),
        kinds: vec![],
        flags: vec![],
        event_id: None,
        predicate: None,
        predicates: Vec::new(),
        text: None,
        limit: std::num::NonZeroU64::new(16).unwrap(),
        offset: 0,
    }
}

fn queried(events: &[Arc<AgentEvent>]) -> Vec<String> {
    events.iter().map(|event| event.id.clone()).collect()
}

fn query_pairs(reply: &dim_agent_runtime::TimelineQueryReply) -> Vec<(&str, u64)> {
    reply
        .positions
        .iter()
        .map(|position| (position.branch_id.as_str(), position.sequence))
        .collect()
}

fn branch_create(branch: &str, at: u64) -> EventDraft {
    EventKind::BranchCreate(BranchCreateData {
        at: BranchCreateAt::Sequence { sequence: at },
        source: BranchSource::Host {
            operation_id: "test".into(),
        },
        branch: branch.into(),
        prompt: "continue".into(),
    })
    .into()
}

/// `main` with facts `main-1`..`main-<main_head>`, the `child` branch create
/// at `cut`, and `child` cut there with facts `child-1`..`child-<n>`. The
/// create is a fact of `main` at sequence `cut`, so an ancestor reads it.
async fn chain_created_at(main_head: u64, cut: u64, child_facts: u64) -> (Timeline, Timeline) {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let ctx = Ctx::root().unwrap();
    let main = Timeline::new(
        ctx.clone(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    for sequence in 1..=main_head {
        let id = format!("main-{sequence}");
        if sequence == cut {
            main.append(branch_create("child", sequence).with_id(id))
                .unwrap();
        } else {
            main.append(input(&id).with_id(id)).unwrap();
        }
    }
    let child = Timeline::new(
        ctx,
        TimelineKey {
            session_id: "session".into(),
            branch_id: "child".into(),
        },
        vec![("main".into(), cut)],
        store,
    );
    for sequence in 1..=child_facts {
        let id = format!("child-{sequence}");
        child.append(input(&id).with_id(id)).unwrap();
    }
    (main, child)
}

async fn chain_with_a_tall_ancestor(main_head: u64, child_facts: u64) -> (Timeline, Timeline) {
    chain_created_at(main_head, main_head, child_facts).await
}

/// Reads `scope` in pages, skipping the facts each page returned, until a page
/// shorter than `limit` ends the selection.
async fn paged(
    timeline: &Timeline,
    scope: dim_agent_runtime::TimelineQueryScope,
    direction: dim_agent_runtime::TimelineQueryDirection,
    limit: u64,
) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut offset = 0;
    loop {
        let reply = timeline
            .query(dim_agent_runtime::TimelineQuery {
                direction,
                predicates: Vec::new(),
                text: None,
                limit: std::num::NonZeroU64::new(limit).unwrap(),
                offset,
                ..branch_query(scope)
            })
            .await
            .unwrap();
        let ids = queried(&reply.events);
        let short = ids.len() as u64 != limit;
        offset += ids.len() as u64;
        pages.push(ids);
        if short {
            return pages;
        }
    }
}

/// `main` with two single facts, one atomic batch of three and two more
/// singles: `main-1`..`main-7` at sequences 1..7.
async fn batched_timeline() -> Timeline {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    );
    for id in ["main-1", "main-2"] {
        timeline.append(input(id).with_id(id)).unwrap();
    }
    timeline
        .append_batch_registered(
            vec![
                input("a").with_id("main-3"),
                input("b").with_id("main-4"),
                input("c").with_id("main-5"),
            ],
            |_| Ok(()),
        )
        .unwrap();
    for id in ["main-6", "main-7"] {
        timeline.append(input(id).with_id(id)).unwrap();
    }
    timeline
}

#[tokio::test]
async fn a_query_pages_by_skipping_and_its_pages_compose_to_the_selection() {
    use dim_agent_runtime::TimelineQueryDirection::Ascending;
    use dim_agent_runtime::TimelineQueryScope::Branch;

    let timeline = batched_timeline().await;
    let whole = timeline.query(branch_query(Branch)).await.unwrap();
    assert_eq!(
        queried(&whole.events),
        [
            "main-1", "main-2", "main-3", "main-4", "main-5", "main-6", "main-7"
        ]
    );
    assert_eq!(
        query_pairs(&whole),
        [
            ("main", 1),
            ("main", 2),
            ("main", 3),
            ("main", 4),
            ("main", 5),
            ("main", 6),
            ("main", 7)
        ]
    );

    let pages = paged(&timeline, Branch, Ascending, 2).await;
    assert_eq!(
        pages,
        vec![
            vec!["main-1".to_owned(), "main-2".to_owned()],
            vec!["main-3".to_owned(), "main-4".to_owned()],
            vec!["main-5".to_owned(), "main-6".to_owned()],
            vec!["main-7".to_owned()],
        ],
        "a page may end inside an atomic batch, and the last one is short"
    );
    assert_eq!(
        pages.concat(),
        queried(&whole.events),
        "the pages compose to the single-page read, every fact exactly once"
    );

    let spanning = timeline
        .query(dim_agent_runtime::TimelineQuery {
            offset: 1,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(5).unwrap(),
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(
        queried(&spanning.events),
        ["main-2", "main-3", "main-4", "main-5", "main-6"],
        "a page starts after the facts it skips"
    );

    let at_the_end = timeline
        .query(dim_agent_runtime::TimelineQuery {
            offset: 7,
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert!(
        at_the_end.events.is_empty() && at_the_end.positions.is_empty(),
        "skipping the whole selection returns no fact"
    );
    let past_the_end = timeline
        .query(dim_agent_runtime::TimelineQuery {
            offset: 99,
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert!(
        past_the_end.events.is_empty(),
        "a skip past the selection is not an error"
    );
}

#[tokio::test]
async fn a_descending_query_takes_the_newest_facts_in_descending_order() {
    use dim_agent_runtime::TimelineQueryDirection::{Ascending, Descending};
    use dim_agent_runtime::TimelineQueryScope::Branch;

    let timeline = batched_timeline().await;
    let newest = timeline
        .query(dim_agent_runtime::TimelineQuery {
            direction: Descending,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(2).unwrap(),
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(queried(&newest.events), ["main-7", "main-6"]);
    assert_eq!(
        query_pairs(&newest),
        [("main", 7), ("main", 6)],
        "a descending page is ordered by sequence descending"
    );

    let mut reversed = queried(&timeline.query(branch_query(Branch)).await.unwrap().events);
    reversed.reverse();
    let pages = paged(&timeline, Branch, Descending, 2).await;
    assert_eq!(
        pages.concat(),
        reversed,
        "the descending pages compose to the ascending read reversed"
    );

    let ascending = timeline
        .query(dim_agent_runtime::TimelineQuery {
            direction: Ascending,
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(
        queried(&ascending.events),
        [
            "main-1", "main-2", "main-3", "main-4", "main-5", "main-6", "main-7"
        ]
    );
}

#[tokio::test]
async fn a_chain_query_orders_by_chain_position_then_sequence() {
    use dim_agent_runtime::TimelineQueryScope::{Branch, Chain};

    let (_, child) = chain_created_at(3, 3, 2).await;
    let chain = child.query(branch_query(Chain)).await.unwrap();
    assert_eq!(
        queried(&chain.events),
        ["main-1", "main-2", "main-3", "child-1", "child-2"],
        "the ancestor comes first, and it reads to its cut"
    );
    assert_eq!(
        query_pairs(&chain),
        [
            ("main", 1),
            ("main", 2),
            ("main", 3),
            ("child", 1),
            ("child", 2)
        ],
        "a chain read reports the ancestor's branch for an ancestor's fact"
    );

    let local = child.query(branch_query(Branch)).await.unwrap();
    assert_eq!(
        queried(&local.events),
        ["child-1", "child-2"],
        "a branch read covers this branch alone"
    );
}

#[tokio::test]
async fn a_query_selects_kinds_and_the_flags_it_names() {
    use dim_agent_runtime::TimelineQueryScope::Branch;

    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    );
    timeline.append(input("message").with_id("input")).unwrap();
    timeline
        .append(
            EventDraft::new(EventKind::Text(DeltaData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                delta: "text".into(),
            }))
            .with_id("text"),
        )
        .unwrap();
    timeline
        .append(
            EventDraft::new(EventKind::StopInterrupt(StopInterruptData {
                reason: "stop".into(),
                scope: InterruptScope::Turn,
            }))
            .with_id("stop"),
        )
        .unwrap();

    let by_kind = timeline
        .query(dim_agent_runtime::TimelineQuery {
            kinds: vec!["input/message".into()],
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(queried(&by_kind.events), ["input"]);

    let render = timeline
        .query(dim_agent_runtime::TimelineQuery {
            flags: vec![dim_agent_runtime::TimelineQueryFlag::Render],
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(queried(&render.events), ["text"]);

    let state = timeline
        .query(dim_agent_runtime::TimelineQuery {
            flags: vec![dim_agent_runtime::TimelineQueryFlag::State],
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(queried(&state.events), ["input", "text"]);

    let union = timeline
        .query(dim_agent_runtime::TimelineQuery {
            flags: vec![
                dim_agent_runtime::TimelineQueryFlag::Render,
                dim_agent_runtime::TimelineQueryFlag::State,
            ],
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert_eq!(
        queried(&union.events),
        ["input", "text"],
        "naming both flags selects the union: a row carrying one matches"
    );

    let any = timeline.query(branch_query(Branch)).await.unwrap();
    assert_eq!(queried(&any.events), ["input", "text", "stop"]);

    let unknown = timeline
        .query(dim_agent_runtime::TimelineQuery {
            kinds: vec!["test/absent".into()],
            ..branch_query(Branch)
        })
        .await
        .unwrap();
    assert!(unknown.events.is_empty(), "an unknown kind selects no rows");
}

#[tokio::test]
async fn a_query_predicate_matches_one_dotted_path_and_an_unknown_path_matches_nothing() {
    use dim_agent_runtime::{PredicateOp, TimelineQueryPredicate, TimelineQueryScope::Branch};

    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "session".into(),
            branch_id: "main".into(),
        },
        vec![],
        store,
    );
    let marked = |id: &str, tool: &str| {
        EventDraft::new(EventKind::Extension(
            ExtensionEvent::new(
                "test",
                "marked",
                serde_json::json!({"tool": tool, "nested": {"level": 3}}),
            )
            .unwrap(),
        ))
        .with_id(id)
    };
    timeline.append(marked("search", "search")).unwrap();
    timeline.append(marked("read", "read")).unwrap();

    let matching = |path: &str, value: serde_json::Value| dim_agent_runtime::TimelineQuery {
        predicate: Some(TimelineQueryPredicate {
            op: PredicateOp::Eq,
            path: path.into(),
            value,
        }),
        ..branch_query(Branch)
    };
    let reply = timeline
        .query(matching("tool", serde_json::json!("search")))
        .await
        .unwrap();
    assert_eq!(queried(&reply.events), ["search"]);
    let reply = timeline
        .query(matching("nested.level", serde_json::json!(3)))
        .await
        .unwrap();
    assert_eq!(queried(&reply.events), ["search", "read"]);
    let reply = timeline
        .query(matching("nested.absent", serde_json::json!(3)))
        .await
        .unwrap();
    assert!(reply.events.is_empty(), "an unknown path selects no rows");
    let reply = timeline
        .query(matching("tool", serde_json::json!("absent")))
        .await
        .unwrap();
    assert!(reply.events.is_empty());
}

#[tokio::test]
async fn a_query_event_id_selects_one_fact_and_reports_its_branch() {
    use dim_agent_runtime::TimelineQueryScope::Chain;

    let (_, child) = chain_created_at(3, 3, 1).await;
    let ancestor = child
        .query(dim_agent_runtime::TimelineQuery {
            event_id: Some("main-2".into()),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert_eq!(queried(&ancestor.events), ["main-2"]);
    assert_eq!(
        ancestor.positions,
        vec![TimelinePosition {
            branch_id: "main".into(),
            sequence: 2
        }],
        "a by-id lookup reports the branch the event lives on"
    );

    let local = child
        .query(dim_agent_runtime::TimelineQuery {
            event_id: Some("child-1".into()),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert_eq!(queried(&local.events), ["child-1"]);
    assert_eq!(local.positions[0].branch_id, "child");

    let absent = child
        .query(dim_agent_runtime::TimelineQuery {
            event_id: Some("absent".into()),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert!(
        absent.events.is_empty() && absent.positions.is_empty(),
        "an id outside the scope selects nothing"
    );

    let filtered = child
        .query(dim_agent_runtime::TimelineQuery {
            event_id: Some("main-3".into()),
            kinds: vec!["input/message".into()],
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert!(
        filtered.events.is_empty(),
        "a by-id lookup also applies the kinds: main-3 is the branch create"
    );

    let skipped = child
        .query(dim_agent_runtime::TimelineQuery {
            event_id: Some("main-2".into()),
            offset: 1,
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert!(
        skipped.events.is_empty(),
        "an offset past the one match returns no fact"
    );
}

#[tokio::test]
async fn a_chain_page_carries_its_offset_across_segments() {
    use dim_agent_runtime::TimelineQueryDirection::Ascending;
    use dim_agent_runtime::TimelineQueryScope::Chain;

    let (_, child) = chain_with_a_tall_ancestor(5, 3).await;
    let whole = child.query(branch_query(Chain)).await.unwrap();
    assert_eq!(
        query_pairs(&whole),
        [
            ("main", 1),
            ("main", 2),
            ("main", 3),
            ("main", 4),
            ("main", 5),
            ("child", 1),
            ("child", 2),
            ("child", 3)
        ]
    );

    let spanning = child
        .query(dim_agent_runtime::TimelineQuery {
            offset: 4,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(3).unwrap(),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert_eq!(
        query_pairs(&spanning),
        [("main", 5), ("child", 1), ("child", 2)],
        "the page starts in the ancestor and continues into the read branch"
    );

    let tail = child
        .query(dim_agent_runtime::TimelineQuery {
            offset: 7,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(3).unwrap(),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert_eq!(query_pairs(&tail), [("child", 3)]);
    let past = child
        .query(dim_agent_runtime::TimelineQuery {
            offset: 8,
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert!(
        past.events.is_empty(),
        "skipping the whole chain returns no fact"
    );

    let pages = paged(&child, Chain, Ascending, 3).await;
    assert_eq!(
        pages.concat(),
        queried(&whole.events),
        "the pages compose to the single-page read, every fact exactly once"
    );
}

#[tokio::test]
async fn a_descending_chain_page_starts_in_the_branch_and_continues_into_the_ancestor() {
    use dim_agent_runtime::TimelineQueryDirection::Descending;
    use dim_agent_runtime::TimelineQueryScope::Chain;

    let (_, child) = chain_with_a_tall_ancestor(5, 3).await;
    let newest = child
        .query(dim_agent_runtime::TimelineQuery {
            direction: Descending,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(2).unwrap(),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert_eq!(
        query_pairs(&newest),
        [("child", 3), ("child", 2)],
        "a descending chain read starts at the read branch's newest fact"
    );

    let spanning = child
        .query(dim_agent_runtime::TimelineQuery {
            direction: Descending,
            offset: 2,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(4).unwrap(),
            ..branch_query(Chain)
        })
        .await
        .unwrap();
    assert_eq!(
        query_pairs(&spanning),
        [("child", 1), ("main", 5), ("main", 4), ("main", 3)],
        "the page continues from the read branch into its ancestor"
    );

    let mut reversed: Vec<String> =
        queried(&child.query(branch_query(Chain)).await.unwrap().events);
    reversed.reverse();
    let pages = paged(&child, Chain, Descending, 3).await;
    assert_eq!(
        pages.concat(),
        reversed,
        "the descending pages compose to the ascending read reversed"
    );
}

#[tokio::test]
async fn a_chain_read_on_an_empty_branch_returns_its_ancestor_facts() {
    use dim_agent_runtime::TimelineQueryScope::Chain;

    let (_, child) = chain_with_a_tall_ancestor(5, 0).await;
    let reply = child.query(branch_query(Chain)).await.unwrap();
    assert_eq!(
        query_pairs(&reply),
        [
            ("main", 1),
            ("main", 2),
            ("main", 3),
            ("main", 4),
            ("main", 5)
        ],
        "a read branch with no facts still reaches its ancestors"
    );
}

/// A read begins where the caller last was, named by a sequence or by an event
/// id, and the event it names is part of the read.
#[tokio::test]
async fn a_read_begins_at_the_sequence_or_event_it_names() {
    use dim_agent_runtime::TimelineQueryScope::{Branch, Chain};

    let (main, child) = chain_created_at(5, 5, 3).await;

    let mut by_sequence = branch_query(Branch);
    by_sequence.from_sequence = Some(4);
    let page = main.query(by_sequence).await.unwrap();
    assert_eq!(
        queried(&page.events),
        ["main-4", "main-5"],
        "a sequence start includes the event at it"
    );

    let mut by_event = branch_query(Branch);
    by_event.from_event = Some("main-4".into());
    let page = main.query(by_event).await.unwrap();
    assert_eq!(
        queried(&page.events),
        ["main-4", "main-5"],
        "an event start resolves to its position"
    );

    let mut chain = branch_query(Chain);
    chain.from_event = Some("main-4".into());
    let page = child.query(chain).await.unwrap();
    assert_eq!(
        query_pairs(&page),
        [
            ("main", 4),
            ("main", 5),
            ("child", 1),
            ("child", 2),
            ("child", 3),
        ],
        "a chain read begins at the event it names and carries on"
    );

    let mut descending = branch_query(Branch);
    descending.direction = dim_agent_runtime::TimelineQueryDirection::Descending;
    descending.from_sequence = Some(5);
    let page = main.query(descending).await.unwrap();
    assert_eq!(
        queried(&page.events),
        ["main-5", "main-4", "main-3", "main-2", "main-1"],
        "a descending read begins at the sequence and goes back"
    );

    let mut absent = branch_query(Branch);
    absent.from_event = Some("absent".into());
    let page = main.query(absent).await.unwrap();
    assert!(
        page.events.is_empty(),
        "a read that begins at no event selects nothing"
    );

    let mut both = branch_query(Branch);
    both.from_event = Some("main-4".into());
    both.from_sequence = Some(4);
    assert!(
        matches!(main.query(both).await, Err(AgentError::InvalidInput(_))),
        "a read begins at one event, not two"
    );
}

#[test]
fn the_query_crosses_languages_with_its_serde_shape() {
    use dim_agent_runtime::{PredicateOp, TimelineQueryPredicate, TimelineQueryScope};

    let query = dim_agent_runtime::TimelineQuery {
        scope: TimelineQueryScope::Chain,
        include_publishing: false,
        from_event: None,
        from_sequence: None,
        order_by: Default::default(),
        direction: dim_agent_runtime::TimelineQueryDirection::Descending,
        kinds: vec!["input/message".into()],
        flags: vec![
            dim_agent_runtime::TimelineQueryFlag::Render,
            dim_agent_runtime::TimelineQueryFlag::State,
        ],
        event_id: Some("event-1".into()),
        predicate: Some(TimelineQueryPredicate {
            op: PredicateOp::Eq,
            path: "nested.level".into(),
            value: serde_json::json!(3),
        }),
        predicates: Vec::new(),
        text: None,
        limit: std::num::NonZeroU64::new(4).unwrap(),
        offset: 2,
    };
    let wire = serde_json::to_value(&query).unwrap();
    assert_eq!(wire["scope"], "chain");
    assert_eq!(wire["include_publishing"], false);
    assert_eq!(wire["direction"], "descending");
    assert_eq!(wire["flags"], serde_json::json!(["render", "state"]));
    assert_eq!(wire["event_id"], "event-1");
    assert_eq!(wire["limit"], 4);
    assert_eq!(wire["offset"], 2);
    assert_eq!(wire["predicate"]["path"], "nested.level");
    assert_eq!(
        serde_json::from_value::<dim_agent_runtime::TimelineQuery>(wire).unwrap(),
        query
    );
    let defaulted: dim_agent_runtime::TimelineQuery = serde_json::from_value(serde_json::json!({
        "scope": "branch",
        "limit": 2
    }))
    .unwrap();
    assert_eq!(
        defaulted.direction,
        dim_agent_runtime::TimelineQueryDirection::Ascending,
        "ascending is the default"
    );
    assert!(!defaulted.include_publishing);
    assert_eq!(defaulted.offset, 0);
    assert_eq!(defaulted.kinds, Vec::<String>::new());
    assert_eq!(defaulted.flags, Vec::new());
    assert_eq!(defaulted.event_id, None);
    assert_eq!(defaulted.predicate, None);
    assert!(
        serde_json::from_value::<dim_agent_runtime::TimelineQuery>(serde_json::json!({
            "scope": "branch",
            "limit": 0
        }))
        .is_err(),
        "a zero limit is not a query"
    );
    assert!(
        serde_json::from_value::<dim_agent_runtime::TimelineQuery>(serde_json::json!({
            "scope": "branch",
            "position": {"kind": "range", "start": 1},
            "limit": 2
        }))
        .is_err(),
        "a position is not part of the query"
    );
}
