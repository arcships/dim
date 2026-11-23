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

#[path = "timeline/property_queries.rs"]
mod property_queries;
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
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        self.queries.lock().unwrap().extend(
            scope
                .segments
                .iter()
                .map(|segment| (segment.branch_id.clone(), segment.from, segment.through)),
        );
        self.memory.query(scope, query, page)
    }

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
}

#[tokio::test]
async fn store_pages_read_only_the_requested_complete_batches() {
    let store = Arc::new(Reads::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let writer = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    writer
        .append_batch_registered(vec![input("one"), input("two"), input("three")], |_| Ok(()))
        .unwrap();
    for n in 4..=1024 {
        writer.append(input(&format!("message {n}"))).unwrap();
    }
    assert_eq!(store.batch_end_at(&key, 1).await.unwrap(), 3);
    assert_eq!(store.batch_end_at(&key, 0).await.unwrap(), 0);
    assert_eq!(store.read_page(&key, 0, 2, 1024).await.unwrap().len(), 3);
    assert_eq!(store.read_page(&key, 3, 2, 1024).await.unwrap().len(), 2);
    assert_eq!(
        *store.segments.lock().unwrap(),
        [("main".into(), 1..=3), ("main".into(), 4..=5)],
        "reading five events must not copy the thousand-event suffix"
    );
    assert!(store.read_page(&key, 1, 2, 1024).await.is_err());
    assert!(store.read_page(&key, 0, 2, 1).await.is_err());
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

/// A store that accepts the first batch and answers it as failed without
/// writing, as a writer whose commit fails after admission would.
#[derive(Default)]
struct FailedAppend {
    memory: MemoryTimelineStore,
    held: Mutex<Option<PublishCallback>>,
}
impl TimelineStore for FailedAppend {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        self.memory.query(scope, query, page)
    }
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
        let mut held = self.held.lock().unwrap();
        if held.is_none() {
            *held = Some(publish);
            return Ok(());
        }
        drop(held);
        self.memory.append(key, events, publish)
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(id, segments)
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
}

#[tokio::test]
async fn a_failed_append_cannot_shift_the_stored_sequence_of_the_next_batch() {
    let store = Arc::new(FailedAppend::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let timeline = Timeline::new(Ctx::root().unwrap(), key.clone(), vec![], store.clone());
    timeline
        .append(input("failed"))
        .expect("the store accepted the batch");
    let later = timeline.append(input("later"));
    let head = store.head(&key).await.unwrap();
    if head > 0 {
        let error = store.read(&key, 1..=head).await.unwrap_err();
        panic!(
            "the rejected sequence moved to a lower position: reading 1..={head} failed with {error}"
        );
    }
    assert!(
        matches!(&later, Err(AgentError::Storage(message)) if message == "Timeline sequence is not contiguous"),
        "{later:?}"
    );
    let held = store.held.lock().unwrap().take().expect("the held batch");
    held(Err(AgentError::Storage("injected append failure".into())));
    assert!(
        timeline.failure().is_some(),
        "the failed batch faults the timeline"
    );
}

#[path = "timeline/position_queries.rs"]
mod position_queries;
