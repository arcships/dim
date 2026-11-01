use dim_agent_runtime::{
    AgentError, BoxFuture, MemoryTimelineStore, PublishCallback, Result, SessionConfig,
    StoredBranch, StoredEvent, Timeline, TimelineKey, TimelineStore, TimelineTrace,
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

/// Counts every segment read from the store, in call order.
#[derive(Default)]
struct Reads {
    memory: MemoryTimelineStore,
    segments: Mutex<Vec<(String, RangeInclusive<u64>)>>,
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

/// Decodes a checkpoint whose resume is the sequence held in the snapshot
/// state, on `main`.
fn checkpoint_resume(
    snapshot: &RecoverySnapshot,
    _anchor: &AgentEvent,
) -> Option<ProjectionCheckpoint> {
    Some(ProjectionCheckpoint {
        resume: TimelinePosition {
            branch_id: "main".into(),
            sequence: snapshot.state.parse().ok()?,
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
    assert_eq!(timeline.local(..).await.unwrap().len(), 2);
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

/// A checkpointed read reads the anchor as one row and never a range that
/// spans it to the cut, so the tail is not decoded twice.
#[tokio::test]
async fn a_checkpointed_read_decodes_one_anchor_event() {
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
    let context = reader
        .context_with_checkpoint(through, "test/anchor", checkpoint_resume)
        .await
        .unwrap();
    assert!(context.checkpoint.is_some());
    assert_eq!(context.events.len(), (through - 39) as usize);
    let segments = store.segments.lock().unwrap();
    assert_eq!(
        segments
            .first()
            .map(|(branch, range)| (branch.as_str(), range.clone())),
        Some(("main", 51..=51)),
        "the anchor check reads the anchor row"
    );
    assert!(
        !segments
            .iter()
            .any(|(_, range)| *range.start() >= 51 && *range.end() >= through),
        "no read runs from the anchor row to the cut"
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
    let context = branch
        .context_with_checkpoint(through, "test/anchor", checkpoint_resume)
        .await
        .unwrap();
    assert!(context.checkpoint.is_some(), "the checkpoint is used");
    assert_eq!(context.events.len(), (through - 39) as usize);
    assert_eq!(
        *store.segments.lock().unwrap(),
        [("main".to_string(), 40..=through)],
        "the anchor comes from the hot window; only the context read reaches the store"
    );
}

/// An anchor row inside an atomic batch keeps its checkpoint: the point read
/// cannot end there, so the segment read finds the same anchor as before.
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
    let context = reader
        .context_with_checkpoint(52, "test/anchor", checkpoint_resume)
        .await
        .unwrap();
    assert!(
        context.checkpoint.is_some(),
        "a mid-batch anchor still selects its checkpoint"
    );
    assert_eq!(context.events.len(), 13);
}

#[tokio::test]
async fn last_event_reads_a_cut_inside_an_atomic_batch() {
    use dim_agent_runtime::{StoredEvent, TimelineStore};
    use dim_agent_types::{AgentEvent, EventKind, InputMessageData};

    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let event = |id: &str, sequence: u64| AgentEvent {
        id: id.into(),
        sequence,
        create_at: 0,
        tags: None,
        kind: EventKind::InputMessage(InputMessageData::text(id, None)),
    };
    store
        .append(
            key.clone(),
            vec![
                StoredEvent {
                    event: Arc::new(event("a", 1)),
                    batch_end: 2,
                    trace: None,
                },
                StoredEvent {
                    event: Arc::new(event("b", 2)),
                    batch_end: 2,
                    trace: None,
                },
            ],
            Box::new(|_| {}),
        )
        .unwrap();
    let last = store.last_event(&key, 1).await.unwrap().unwrap();
    assert_eq!(last.sequence, 1);
    let last = store.last_event(&key, 2).await.unwrap().unwrap();
    assert_eq!(last.sequence, 2);
}
