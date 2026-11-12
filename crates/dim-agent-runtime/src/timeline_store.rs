use crate::{BoxFuture, BranchRecord, Result, SessionConfig, TimelineTrace};
use dim_agent_types::{AgentEvent, RecoverySnapshot};
use std::{ops::RangeInclusive, sync::Arc};

/// Storage address of one Timeline inside an Agent's shared store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimelineKey {
    pub session_id: String,
    pub branch_id: String,
}

/// A complete logical scope, captured by the Timeline before storage I/O.
#[derive(Debug, Clone)]
pub struct TimelineReadScope {
    pub key: TimelineKey,
    pub through: u64,
    pub segments: Vec<TimelineReadSegment>,
}

impl TimelineReadScope {
    /// Binds continuation data to logical facts, independently of tail persistence progress.
    pub fn query_binding(&self, query: &crate::QuerySpec) -> Result<String> {
        let intervals: Vec<_> = self
            .segments
            .iter()
            .map(|segment| (&segment.branch_id, segment.from, segment.through))
            .collect();
        serde_json::to_string(&(
            &self.key.session_id,
            &self.key.branch_id,
            self.through,
            intervals,
            query,
        ))
        .map_err(|error| crate::AgentError::InvalidInput(error.to_string()))
    }
}

/// One inclusive branch interval. Stored events stop at `stored_through`;
/// the tail supplies the remaining committed events without overlap.
#[derive(Debug, Clone)]
pub struct TimelineReadSegment {
    pub branch_id: String,
    pub from: u64,
    pub through: u64,
    pub stored_through: u64,
    pub tail: Vec<Arc<AgentEvent>>,
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
    pub info: BranchRecord,
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
/// Each Session id has at most one live load per store; callers serialize
/// closing and restoring that id, including when Agents share a store.
pub trait TimelineStore: Send + Sync + 'static {
    /// Computes a validated query over the complete fixed scope and its disjoint tail.
    fn query<'a>(
        &'a self,
        scope: &'a TimelineReadScope,
        query: &'a crate::QuerySpec,
        page: Option<&'a crate::QueryPage>,
    ) -> BoxFuture<'a, Result<crate::QueryResult>>;

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
    /// Releases runtime admission state after all Session producers stop.
    /// A running store drains prior writes and clears this load's failure state
    /// without deleting stored history, so a later restore can write again.
    /// A closing or stopped store treats release as already settled; shutdown
    /// remains responsible for pending writes and faults. Append failures remain
    /// observable through their own results.
    /// Idempotent: a Session with no admission state is already released.
    fn release_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>>;
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>>;
    /// Returns the Session's branch directory without reading event history.
    /// Persisted directory metadata is checked here; event and content decoding
    /// errors are reported by the operations that read them.
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>>;
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>>;
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()>;
    /// Checks that `sequence` ends an atomic batch, without reading the event:
    /// whether a sequence closes a batch is the stored row's structure, not
    /// event content (ADR 020 D4). Zero, before the branch's first event, ends
    /// a batch. An interior sequence is invalid input; a sequence the branch
    /// does not store is a storage error.
    fn batch_end<'a>(&'a self, key: &'a TimelineKey, sequence: u64) -> BoxFuture<'a, Result<()>>;
    /// Returns the end of the batch containing `sequence`, without decoding event content.
    /// Zero denotes the position before the first event.
    fn batch_end_at<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let mut end = sequence;
            loop {
                match self.batch_end(key, end).await {
                    Ok(()) => return Ok(end),
                    Err(crate::AgentError::InvalidInput(_)) => {
                        end = end.checked_add(1).ok_or_else(|| {
                            crate::AgentError::Storage("batch sequence overflow".into())
                        })?
                    }
                    Err(error) => return Err(error),
                }
            }
        })
    }
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
            self.batch_end(key, after).await?;
            self.batch_end(key, through).await?;
            if limit == 0 || after == through {
                return Ok(vec![]);
            }
            let target = after
                .saturating_add(u64::try_from(limit).unwrap_or(u64::MAX))
                .min(through);
            let end = self.batch_end_at(key, target).await?;
            let records = self.read(key, after + 1..=end).await?;
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
