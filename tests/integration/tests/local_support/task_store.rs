use dim_agent_runtime::*;
use dim_agent_types::*;
use std::{ops::RangeInclusive, sync::Mutex};
use tokio::sync::{Notify, oneshot};

#[derive(Default)]
pub struct TaskStore {
    memory: MemoryTimelineStore,
    next_query: Mutex<Option<oneshot::Receiver<Result<()>>>>,
    pub entered: Notify,
}

impl TaskStore {
    pub fn hold_next_query(&self) -> oneshot::Sender<Result<()>> {
        let (release, held) = oneshot::channel();
        assert!(self.next_query.lock().unwrap().replace(held).is_none());
        release
    }
}

impl TimelineStore for TaskStore {
    fn query<'a>(
        &'a self,
        scope: &'a TimelineReadScope,
        query: &'a QuerySpec,
        page: Option<&'a QueryPage>,
    ) -> BoxFuture<'a, Result<QueryResult>> {
        let held = self.next_query.lock().unwrap().take();
        Box::pin(async move {
            if let Some(held) = held {
                self.entered.notify_one();
                held.await
                    .expect("the test releases or cancels its query")?;
            }
            self.memory.query(scope, query, page).await
        })
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
