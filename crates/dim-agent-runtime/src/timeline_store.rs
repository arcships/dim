use crate::{BoxFuture, Result, SessionConfig, TimelineInfo, TimelineTrace};
use dim_agent_types::{AgentEvent, RecoverySnapshot};
use std::{ops::RangeInclusive, sync::Arc};

/// Storage address of one Timeline inside an Agent's shared store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimelineKey {
    pub session_id: String,
    pub branch_id: String,
}

/// One stored event and its atomic publication boundary.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub event: Arc<AgentEvent>,
    pub trace: Option<TimelineTrace>,
    pub batch_end: u64,
}

#[derive(Debug, Clone)]
pub struct StoredBranch {
    pub info: TimelineInfo,
    pub sequence: u64,
}

pub type PublishCallback = Box<dyn FnOnce(Result<()>) + Send + 'static>;

/// Agent-shared event storage. Append queues a complete atomic batch without
/// waiting for I/O; an accepted batch's callback runs once after durable commit
/// or failure.
/// Callbacks for one branch run in sequence order. A failed Session rejects
/// subsequent batches, while other Sessions continue. `head`, `read`, and
/// each `read_segments` segment require an existing Session and branch, even
/// for an empty range; missing keys return a storage error. `append` rejects
/// missing keys immediately or through its publish callback and never creates
/// a directory.
pub trait TimelineStore: Send + Sync + 'static {
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>>;
    /// Removes a newly committed Session after initialization has failed.
    /// The caller first stops its producers; completion drains prior writes and
    /// removes its directory, ownership, events, branches, and content atomically.
    /// Never used for restore failures or ordinary Session close.
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>>;
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>>;
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>>;
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>>;
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()>;
    /// Range starts may be inside a batch; the end must be a batch boundary.
    fn read<'a>(
        &'a self,
        key: &'a TimelineKey,
        range: RangeInclusive<u64>,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            let mut result = self
                .read_segments(&key.session_id, vec![(key.branch_id.clone(), range)])
                .await?;
            Ok(result.pop().expect("one range returns one segment"))
        })
    }
    /// Reads complete batches after a batch-boundary cursor at a fixed head.
    /// The event limit is a target: the final batch may exceed it.
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            if after > through {
                return Err(crate::AgentError::InvalidInput(
                    "future Timeline cursor".into(),
                ));
            }
            let records = self.read(key, after.max(1)..=through).await?;
            if after > 0
                && records
                    .first()
                    .is_some_and(|record| record.batch_end != after)
            {
                return Err(crate::AgentError::InvalidInput(
                    "cursor is inside an atomic batch".into(),
                ));
            }
            if limit == 0 {
                return Ok(vec![]);
            }
            let mut records: Vec<_> = records
                .into_iter()
                .filter(|record| record.event.sequence > after)
                .collect();
            if let Some(record) = records.get(limit - 1) {
                let end = record.batch_end;
                records.retain(|record| record.event.sequence <= end);
            }
            Ok(records)
        })
    }
    /// Decodes all segments in one read scope, sharing content across branches.
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>>;
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>>;
    /// Optional acceleration only. Save after the anchoring fact commits;
    /// failures must not roll back or poison the fact stream.
    fn save_snapshot<'a>(
        &'a self,
        _key: &'a TimelineKey,
        _snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    /// Return the latest compatible candidate without interpreting its state.
    /// The caller validates the decoded resume position before trimming history.
    fn latest_snapshot<'a>(
        &'a self,
        _key: &'a TimelineKey,
        _through: u64,
        _interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        Box::pin(async { Ok(None) })
    }
}
