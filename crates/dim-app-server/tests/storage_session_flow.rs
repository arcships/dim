use dim_agent_capabilities::{defaults, timeline_tree::TreeCommands};
use dim_agent_runtime::Names;
use dim_agent_runtime::testing::branch_events;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, BranchOptions, Dispatcher, Feature, SessionConfig,
    Timeline, TimelineIdentity, TimelineKey, TimelineStore, testing::MinimalHost,
};
use dim_agent_types::*;
use dim_app_server::storage::{SqliteStorage, SqliteTimelineStore};
use rusqlite::Connection;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

struct CaptureTimeline {
    timelines: Arc<Mutex<BTreeMap<String, Timeline>>>,
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
}
impl CaptureTimeline {
    fn new(
        names: &Names<'_>,
        timelines: Arc<Mutex<BTreeMap<String, Timeline>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            timelines,
            timeline: names.key()?,
            identity: names.key()?,
        })
    }
}

impl DynPlugin for CaptureTimeline {
    fn name(&self) -> &str {
        "storage-session.capture-timeline"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
            self.timelines
                .lock()
                .unwrap()
                .insert(identity.branch_id.clone(), timeline.as_ref().clone());
            Ok(Effect::Done)
        })
    }
}

struct GenericCleanupFault {
    tree: Arc<Mutex<Option<Arc<TreeCommands>>>>,
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
    dispatcher: Key<Dispatcher>,
    commands: Key<TreeCommands>,
}
impl GenericCleanupFault {
    fn new(names: &Names<'_>, tree: Arc<Mutex<Option<Arc<TreeCommands>>>>) -> rutis::Result<Self> {
        Ok(Self {
            tree,
            timeline: names.key()?,
            identity: names.key()?,
            dispatcher: names.key()?,
            commands: names.key()?,
        })
    }
}
impl DynPlugin for GenericCleanupFault {
    fn name(&self) -> &str {
        "storage-session.generic-cleanup"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.identity.erase(),
            self.dispatcher.erase(),
            self.commands.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let tree = ctx.require_at(&self.commands)?;
            if identity.branch_id != "main" {
                return Ok(Effect::Done);
            }
            *self.tree.lock().unwrap() = Some(tree);
            dispatcher.subscribe(ctx, self.name(), &["input/message"], |event| {
                Box::pin(async move {
                    if matches!(&event.kind, EventKind::InputMessage(data) if matches!(
                        data.user.as_ref().map(|user| user.content.as_slice()),
                        Some([ContentBlock::Text { text }]) if text == "generic cleanup fault"
                    )) {
                        return Err(AgentError::fault(
                            "cleanup fixture",
                            "first generic failure",
                        ));
                    }
                    Ok(())
                })
            })?;
            ctx.effect(|| async move {
                timeline.append(EventDraft {
                    id: None,
                    tags: Some(vec!["cleanup".into()]),
                    parent_call_id: None,
                    kind: EventKind::PermissionChanged(PermissionChangedData {
                        policy: PermissionPolicy::default(),
                    }),
                })?;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_failure_during_generic_cleanup_supersedes_the_session_fault() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/storage-session-flow").join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("dimcode.sqlite");
        let db = SqliteStorage::open(path.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let timelines = Arc::new(Mutex::new(BTreeMap::new()));
        let captured = timelines.clone();
        let tree = Arc::new(Mutex::new(None));
        let captured_tree = tree.clone();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        builder
            .timeline_store(store.clone())
            .require_feature(Feature::new("storage-session.capture-timeline").branch(
                "storage-session.capture-timeline",
                move |names| Ok(Box::new(CaptureTimeline::new(names, captured.clone())?)),
            ))
            .require_feature(Feature::new("storage-session.generic-cleanup").branch(
                "storage-session.generic-cleanup",
                move |names| Ok(Box::new(GenericCleanupFault::new(names, captured_tree.clone())?)),
            ));
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        let idle = session.branch("main".into(), 0, BranchOptions {
            prompt: "idle sibling".into(), source: BranchSource::Host { operation_id: "idle sibling".into() },
        }).await.unwrap();
        session.idle(&idle).await.unwrap();
        let idle_handle = tree.lock().unwrap().as_ref().unwrap().get(&idle).unwrap();
        idle_handle.close().await.unwrap();
        assert!(idle_handle.state().unwrap().closed);
        session.idle("main").await.unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_cleanup BEFORE INSERT ON timeline_events WHEN NEW.kind = 'permission/changed' BEGIN SELECT RAISE(ABORT, 'cleanup storage failure'); END").unwrap();
        session.submit("generic cleanup fault".into(), InputMode::Queue, "main").await.unwrap();
        assert!(session.closed().await.is_err());
        for branch in ["main", idle.as_str()] {
            let timeline = timelines.lock().unwrap().get(branch).unwrap().clone();
            assert!(matches!(timeline.failure(), Some(AgentError::Storage(message)) if message.contains("cleanup storage failure")));
            assert!(matches!(timeline.append(EventKind::StopInterrupt(StopInterruptData { reason: "filler".into(), scope: InterruptScope::Turn }).into()), Err(AgentError::Storage(message)) if message.contains("cleanup storage failure")));
            assert!(matches!(session.submit("late input".into(), InputMode::Queue, branch).await, Err(AgentError::Storage(message)) if message.contains("cleanup storage failure")));
        }
        assert!(matches!(session.failure(), Some(AgentError::Storage(message)) if message.contains("cleanup storage failure")));
        assert!(!branch_events(&session, "main", ..).await.unwrap().iter().any(|event| event.tags.as_deref() == Some(&["cleanup".into()])));
        let _ = agent.close().await;
        drop(connection);
        timelines.lock().unwrap().clear();
        store.close().await.unwrap();
        db.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }).await.expect("generic cleanup with a later storage fault completed");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_session_failure_rejects_writes_on_an_idle_sibling_timeline() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let path = std::env::temp_dir().join(format!("dimagent-storage-siblings-{}.sqlite", uuid::Uuid::new_v4()));
        let db = SqliteStorage::open(path.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let timelines = Arc::new(Mutex::new(BTreeMap::new()));
        let captured = timelines.clone();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        builder
            .timeline_store(store.clone())
            .require_feature(Feature::new("storage-session.capture-timeline").branch(
                "storage-session.capture-timeline",
                move |names| Ok(Box::new(CaptureTimeline::new(names, captured.clone())?)),
            ));
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        let mut siblings = Vec::new();
        for name in ["failed sibling", "idle sibling"] {
            let branch = session.branch("main".into(), 0, BranchOptions {
                prompt: name.into(),
                source: BranchSource::Host { operation_id: name.into() },
            }).await.unwrap();
            session.idle(&branch).await.unwrap();
            siblings.push(branch);
        }
        let failed = &siblings[0];
        let idle = &siblings[1];
        let idle_through = session.sequence(idle).unwrap();
        let failed_through = session.sequence(failed).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE injected_branch(session_id TEXT, branch_id TEXT); CREATE TRIGGER fail_sibling_output BEFORE INSERT ON timeline_events WHEN NEW.kind = 'step/text' AND NEW.session_id = (SELECT session_id FROM injected_branch) AND NEW.branch_id = (SELECT branch_id FROM injected_branch) BEGIN SELECT RAISE(ABORT, 'injected sibling output failure'); END").unwrap();
        connection.execute("INSERT INTO injected_branch VALUES (?1, ?2)", [session.id(), failed]).unwrap();
        session.submit("fail this output".into(), InputMode::Queue, failed).await.unwrap();
        assert!(session.closed().await.unwrap_err().to_string().contains("injected sibling output failure"));
        for branch in [failed, idle] {
            let timeline = timelines.lock().unwrap().get(branch).unwrap().clone();
            assert!(matches!(timeline.failure(), Some(AgentError::Storage(message)) if message.contains("injected sibling output failure")));
            assert!(matches!(timeline.append(EventKind::InputMessage(InputMessageData::text(
                "late direct write", Some(InputMode::Queue),
            )).into()), Err(AgentError::Storage(message)) if message.contains("injected sibling output failure")));
            assert!(matches!(session.submit("late input".into(), InputMode::Queue, branch).await,
                Err(AgentError::Storage(message)) if message.contains("injected sibling output failure")));
        }
        assert_eq!(session.sequence(idle).unwrap(), idle_through);
        assert!(!branch_events(&session, failed, (failed_through + 1)..).await.unwrap().iter().any(|event|
            matches!(event.kind, EventKind::Text(_) | EventKind::ModelOutputEnd(_) | EventKind::TurnEnd(_))));
        assert!(!branch_events(&session, idle, (idle_through + 1)..).await.unwrap().iter().any(|event|
            matches!(event.kind, EventKind::ModelOutputEnd(_) | EventKind::TurnEnd(_))));
        let _ = agent.close().await;
        drop(connection);
        drop(session);
        drop(builder);
        timelines.lock().unwrap().clear();
        store.close().await.unwrap();
        db.close().await.unwrap();
        std::fs::remove_file(path).unwrap();
    }).await.expect("SQLite sibling failure and cleanup completed");
}

struct RecordCleanup {
    timeline: Key<Timeline>,
    _generation: Arc<()>,
}
impl RecordCleanup {
    fn new(names: &Names<'_>, generation: Arc<()>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
            _generation: generation,
        })
    }
}
impl DynPlugin for RecordCleanup {
    fn name(&self) -> &str {
        "storage-session.record-cleanup"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            ctx.effect(|| async move {
                timeline.append(EventDraft {
                    id: None,
                    tags: Some(vec!["finalizer".into()]),
                    parent_call_id: None,
                    kind: EventKind::PermissionChanged(PermissionChangedData {
                        policy: PermissionPolicy::default(),
                    }),
                })?;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn session_close_and_idle_unload_persist_finalizers_and_release_branch_parts() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for unloading in [false, true] {
            let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target/storage-session-flow")
                .join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("dimcode.sqlite");
            let db = SqliteStorage::open(path.clone()).unwrap();
            let store = SqliteTimelineStore::new(db.clone());
            let mut builder = AgentBuilder::new(AgentConfig::default());
            defaults::mock_hosts(&mut builder);
            defaults::install(&mut builder);
            let generation = Arc::new(Mutex::new(Weak::new()));
            let mounted_generation = generation.clone();
            builder.timeline_store(store.clone()).require_feature(
                Feature::new("storage-session.record-cleanup").branch(
                    "storage-session.record-cleanup",
                    move |names| {
                        let marker = Arc::new(());
                        *mounted_generation.lock().unwrap() = Arc::downgrade(&marker);
                        Ok(Box::new(RecordCleanup::new(names, marker)?))
                    },
                ),
            );
            let agent = MinimalHost::start(&builder).await.unwrap();
            let session = agent
                .create_session(SessionConfig::default())
                .await
                .unwrap();
            session
                .submit("settle before cleanup".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let generation = generation.lock().unwrap().clone();
            assert!(generation.upgrade().is_some(), "branch part must be live");
            let key = TimelineKey {
                session_id: session.id().into(),
                branch_id: "main".into(),
            };
            if unloading {
                assert!(session.unload_if_idle().await.unwrap());
            } else {
                session.close().await.unwrap();
                session.close().await.unwrap();
            }
            drop(session);
            assert!(
                generation.upgrade().is_none(),
                "branch part must be dropped after Session teardown (idle unload: {unloading})"
            );
            let head = store.head(&key).await.unwrap();
            let records = store.read(&key, 1..=head).await.unwrap();
            assert_eq!(
                records
                    .iter()
                    .filter(|r| r.event.tags.as_deref() == Some(&["finalizer".into()]))
                    .count(),
                1
            );
            let terminal = records.last().unwrap().event.clone();
            assert_eq!(terminal.tags, Some(vec!["finalizer".into()]));
            agent.close().await.unwrap();
            store.close().await.unwrap();
            db.close().await.unwrap();
            drop(builder);
            drop(store);
            drop(db);
            let reopened = SqliteStorage::open(path).unwrap();
            let store = SqliteTimelineStore::new(reopened.clone());
            assert_eq!(store.head(&key).await.unwrap(), head);
            assert_eq!(
                store.read(&key, head..=head).await.unwrap()[0].event,
                terminal
            );
            store.close().await.unwrap();
            reopened.close().await.unwrap();
            std::fs::remove_dir_all(directory).unwrap();
        }
    })
    .await
    .expect("session cleanup and durable publication completed");
}
