use crate::{
    BoxFuture, BranchRecord, Result, SessionConfig, TimelineQuery, TimelineQueryDirection,
    TimelineQueryOrderBy, TimelineQueryPage, TimelineQueryScope, TimelineTrace,
};
use dim_agent_types::{AgentEvent, EventKind, RecoverySnapshot};
use std::{num::NonZeroU64, ops::RangeInclusive, sync::Arc};

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
    pub info: BranchRecord,
    pub sequence: u64,
}

pub type PublishCallback = Box<dyn FnOnce(Result<()>) + Send + 'static>;

/// One indexed `input/message` submission id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSubmission {
    pub submission_id: String,
    pub event_id: String,
    pub sequence: u64,
}

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
    /// Returns the Session's branch directory after checking the stored
    /// Timeline. An implementation may reuse a check it already ran for this
    /// Session in the same store while the stored directory and every branch
    /// head are unchanged. The first check of a Session in a process reads the
    /// whole Timeline.
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
    /// One per-branch read for [`TimelineQuery`] (ADR 019, plugin contract
    /// §5.4): applies the read point, direction, kinds, flags, event id,
    /// predicate, limit and offset to `key`'s branch and returns up to `limit`
    /// matching facts, ordered by sequence in the query's direction, each with
    /// its position.
    ///
    /// `query.scope` must be `TimelineQueryScope::Branch`. The Timeline
    /// service composes the chain by reading each ancestor and then the
    /// queried branch with this method, carrying the offset the earlier
    /// segment did not use. Paging is skipping: `offset` matching facts are
    /// passed over before the page, so a page shorter than the limit means the
    /// selection is exhausted, and a page may end inside an atomic batch.
    /// `skipped` is the number of matching facts the read passed over, at most
    /// `offset`, so a caller can carry the rest into its next segment. The
    /// query's `bound` is the service's: the cut arrives as `from` and
    /// `through`.
    ///
    /// A read point past the branch's published head and any other scope are
    /// invalid input. A kind, flag, id or data path the store does not know
    /// selects no rows; it is not an error.
    fn query<'a>(
        &'a self,
        key: &'a TimelineKey,
        query: &'a TimelineQuery,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<TimelineQueryPage>>;
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
    /// The submission ids of this branch's `input/message` facts through
    /// `through`. A store may answer from an index; the default scans the
    /// kind in bounded pages.
    fn submission_index<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredSubmission>>> {
        Box::pin(async move {
            let mut rows = Vec::new();
            let mut from = 1;
            while from <= through {
                let page = self
                    .query(
                        key,
                        &TimelineQuery {
                            scope: TimelineQueryScope::Branch,
                            include_publishing: false,
                            from_event: None,
                            from_sequence: None,
                            order_by: TimelineQueryOrderBy::Sequence,
                            direction: TimelineQueryDirection::Ascending,
                            kinds: vec!["input/message".into()],
                            flags: Vec::new(),
                            event_id: None,
                            predicate: None,
                            limit: NonZeroU64::new(256).expect("a page keeps one input"),
                            offset: 0,
                        },
                        from,
                        through,
                    )
                    .await?;
                let last = page.positions.last().map(|position| position.sequence);
                for event in page.events {
                    if let EventKind::InputMessage(data) = &event.kind
                        && let Some(submission_id) = &data.submission_id
                    {
                        rows.push(StoredSubmission {
                            submission_id: submission_id.clone(),
                            event_id: event.id.clone(),
                            sequence: event.sequence,
                        });
                    }
                }
                match last {
                    Some(last) if last < through => from = last + 1,
                    _ => break,
                }
            }
            Ok(rows)
        })
    }
}
