use super::keys::KeyFilter;
use crate::{
    AgentError, BoxFuture, BranchLifecycle, FaultReporter, Result, StoredEvent, TimelineKey,
    TimelineStore,
};
use dim_agent_types::{
    AgentEvent, EventDraft, ProjectionCheckpoint, RecoverySnapshot, TimelinePosition,
};
use rutis::{CordisError, Ctx, Disposer, Event, EventKey, InstanceId, SyncEvent, SyncNext};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    ops::{Bound, RangeBounds},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{OnceCell, watch};
use uuid::Uuid;

/// One draft offered to the branch's `before` middleware. Each wrapper records
/// the step it applied, so steps follow the order the transforms took effect.
struct BeforeCommit {
    draft: EventDraft,
    steps: Mutex<Vec<TraceStep>>,
}
impl Event for BeforeCommit {
    const NAME: &'static str = "timeline.before";
    type Value = EventDraft;
}
impl SyncEvent for BeforeCommit {}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    pub handler: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<EventDraft>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineTrace {
    pub event_id: String,
    pub original: EventDraft,
    pub steps: Vec<TraceStep>,
}

/// A checkpoint and the ordered events replayed after its saved projection state.
#[derive(Debug, Clone)]
pub struct ContextHistory {
    pub checkpoint: Option<ProjectionCheckpoint>,
    pub ancestry: Vec<String>,
    pub events: Vec<Arc<AgentEvent>>,
    /// Source addresses corresponding one-to-one with `events`.
    pub positions: Vec<TimelinePosition>,
}

#[derive(Clone)]
pub struct Timeline(Arc<Inner>);
struct Inner {
    ctx: Ctx,
    /// The branch instance whose subtree hears before-commit listeners.
    instance: InstanceId,
    key: TimelineKey,
    ancestors: Vec<(String, u64)>,
    store: Arc<dyn TimelineStore>,
    restored_sequence: u64,
    /// The branch opening, alive while its lifecycle holds it; see
    /// [`Opening`].
    opening: OnceLock<Weak<Opening>>,
    data: Mutex<Data>,
    commit: Mutex<()>,
    changed: watch::Sender<(u64, bool)>,
    faults: FaultReporter,
}
#[derive(Default)]
struct Data {
    hot: VecDeque<StoredEvent>,
    allocated: u64,
    published: u64,
    closing: bool,
    closed: bool,
    failure: Option<AgentError>,
    reader_active: bool,
}
impl Timeline {
    /// A writable Timeline outside the branch tree, for tests that craft
    /// histories: it bypasses the branch's own commit lock and sequence, so
    /// it is not part of the normal API.
    #[cfg(feature = "testing")]
    pub fn new(
        ctx: Ctx,
        key: TimelineKey,
        ancestors: Vec<(String, u64)>,
        store: Arc<dyn TimelineStore>,
    ) -> Self {
        Self::branch(
            ctx.instance(),
            ctx,
            key,
            ancestors,
            store,
            0,
            FaultReporter::default(),
        )
    }
    /// Constructs a Timeline over validated, previously committed storage;
    /// like [`Self::new`], only for tests.
    #[cfg(feature = "testing")]
    pub async fn restore(
        ctx: Ctx,
        key: TimelineKey,
        ancestors: Vec<(String, u64)>,
        store: Arc<dyn TimelineStore>,
    ) -> Result<Self> {
        let sequence = store.head(&key).await?;
        Ok(Self::branch(
            ctx.instance(),
            ctx,
            key,
            ancestors,
            store,
            sequence,
            FaultReporter::default(),
        ))
    }
    /// The Timeline of the branch scope `instance`, restored at `sequence`.
    pub(crate) fn branch(
        instance: InstanceId,
        ctx: Ctx,
        key: TimelineKey,
        ancestors: Vec<(String, u64)>,
        store: Arc<dyn TimelineStore>,
        sequence: u64,
        faults: FaultReporter,
    ) -> Self {
        Self(Arc::new(Inner {
            ctx,
            instance,
            key,
            ancestors,
            store,
            restored_sequence: sequence,
            opening: OnceLock::new(),
            data: Mutex::new(Data {
                allocated: sequence,
                published: sequence,
                ..Default::default()
            }),
            commit: Mutex::new(()),
            changed: watch::channel((sequence, false)).0,
            faults,
        }))
    }
    pub(crate) fn key(&self) -> &TimelineKey {
        &self.0.key
    }
    pub fn ancestors(&self) -> Vec<(String, u64)> {
        self.0.ancestors.clone()
    }
    pub(crate) fn restored_sequence(&self) -> u64 {
        self.0.restored_sequence
    }
    /// Appends one event; it is published once its batch commits. Parts in
    /// other languages write through it like native capabilities (ADR 001).
    pub fn append(&self, original: EventDraft) -> std::result::Result<Arc<AgentEvent>, AgentError> {
        self.append_registered(original, |_| Ok(()))
    }
    /// Registers producer state before queueing; publication follows store commit.
    pub fn append_registered(
        &self,
        original: EventDraft,
        register: impl FnOnce(&AgentEvent) -> Result<()>,
    ) -> Result<Arc<AgentEvent>> {
        let mut events =
            self.append_batch_registered(vec![original], |events| register(&events[0]))?;
        Ok(events.pop().expect("single-event batch returned one event"))
    }
    pub fn append_batch_registered(
        &self,
        originals: Vec<EventDraft>,
        register: impl FnOnce(&[Arc<AgentEvent>]) -> Result<()>,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let _append = AppendGuard::enter()?;
        let _commit = self.0.commit.lock().unwrap();
        if let Some(error @ AgentError::Storage(_)) = self.0.faults.failure() {
            return Err(error);
        }
        let first_sequence = {
            let data = self.0.data.lock().unwrap();
            if let Some(error) = &data.failure {
                return Err(error.clone());
            }
            if data.closing {
                return Err(AgentError::Closed(self.0.key.branch_id.clone()));
            }
            data.allocated + 1
        };
        let mut records = Vec::new();
        let mut events = Vec::new();
        for original in originals {
            let offered = BeforeCommit {
                draft: original.clone(),
                steps: Mutex::default(),
            };
            // Any failure, including a closing branch scope whose hooks
            // are unloading, commits the original without a trace.
            let (original, draft, steps) = match self.0.ctx.events().waterfall_sync(
                &self.0.ctx,
                &self.before_key(),
                &offered,
                |_, offered| Ok(offered.draft.clone()),
            ) {
                Ok(draft) => (original, draft, offered.steps.into_inner().unwrap()),
                Err(_) => (original.clone(), original, Vec::new()),
            };
            let event = Arc::new(AgentEvent {
                id: Uuid::new_v4().to_string(),
                sequence: first_sequence + events.len() as u64,
                create_at: unix_millis(),
                tags: draft.tags,
                kind: draft.kind,
            });
            let trace = (!steps.is_empty()).then(|| TimelineTrace {
                event_id: event.id.clone(),
                original,
                steps,
            });
            events.push(event.clone());
            records.push(StoredEvent {
                event,
                trace,
                batch_end: 0,
            });
        }
        let last_sequence = events
            .last()
            .ok_or_else(|| AgentError::InvalidInput("timeline batch must not be empty".into()))?
            .sequence;
        for record in &mut records {
            record.batch_end = last_sequence;
        }
        catch_unwind(AssertUnwindSafe(|| register(&events))).map_err(|_| {
            AgentError::fault("timeline.registration", "registration callback panicked")
        })??;
        {
            let mut data = self.0.data.lock().unwrap();
            if let Some(error) = &data.failure {
                return Err(error.clone());
            }
            if data.closing {
                return Err(AgentError::Closed(self.0.key.branch_id.clone()));
            }
            data.allocated = last_sequence;
            data.hot.extend(records.iter().cloned());
        }
        let weak = Arc::downgrade(&self.0);
        let result = self.0.store.append(
            self.0.key.clone(),
            records,
            Box::new(move |result| {
                if let Some(inner) = weak.upgrade() {
                    Timeline(inner).publish(last_sequence, result);
                }
            }),
        );
        if let Err(error) = result {
            self.publish(last_sequence, Err(error.clone()));
            return Err(error);
        }
        Ok(events)
    }
    fn publish(&self, sequence: u64, result: Result<()>) {
        let mut data = self.0.data.lock().unwrap();
        if data.failure.is_some() {
            return;
        }
        if let Err(error) = result {
            data.failure = Some(error.clone());
            let published = data.published;
            let reader_active = data.reader_active;
            data.hot
                .retain(|r| reader_active && r.event.sequence <= published);
            data.closed = true;
            self.0.changed.send_replace((published, true));
            drop(data);
            self.0.faults.report(error);
            return;
        }
        data.published = sequence;
        data.closed = data.closing && data.published == data.allocated;
        if data.closed && !data.reader_active {
            data.hot.clear();
        }
        self.0.changed.send_replace((data.published, data.closed));
    }
    /// Published sequence: what is durable and visible to the Dispatcher
    /// and to readers outside the branch.
    pub fn sequence(&self) -> u64 {
        self.0.data.lock().unwrap().published
    }
    /// Last sequence assigned on this bus, published or not. An event is a
    /// fact once it has its sequence; the reads below cover it.
    pub fn committed(&self) -> u64 {
        self.0.data.lock().unwrap().allocated
    }
    /// Pending events participate in idle/close accounting before publication.
    pub fn has_pending(&self) -> bool {
        let data = self.0.data.lock().unwrap();
        data.failure.is_none() && data.allocated > data.published
    }
    pub fn failure(&self) -> Option<AgentError> {
        self.0.data.lock().unwrap().failure.clone().or_else(|| {
            self.0
                .faults
                .failure()
                .filter(|error| matches!(error, AgentError::Storage(_)))
        })
    }
    fn through(&self, through: Option<u64>) -> Result<u64> {
        let committed = self.committed();
        let through = through.unwrap_or(committed);
        if through > committed {
            return Err(AgentError::InvalidInput(format!(
                "future timeline sequence {through}"
            )));
        }
        Ok(through)
    }
    /// Events of `branch` in `start..=end`. For this branch the hot window
    /// holds every event not yet dispatched, which includes every event not
    /// yet published; the store holds everything published.
    async fn records(&self, branch: &str, start: u64, end: u64) -> Result<Vec<Arc<AgentEvent>>> {
        if end == 0 || start > end {
            return Ok(vec![]);
        }
        if branch == self.0.key.branch_id {
            let (published, hot_start, hot) = {
                let data = self.0.data.lock().unwrap();
                if data
                    .hot
                    .iter()
                    .any(|r| r.event.sequence == end && r.batch_end > end)
                {
                    return Err(AgentError::InvalidInput(format!(
                        "sequence {end} is inside an atomic batch"
                    )));
                }
                let hot_start = data.hot.front().map_or(u64::MAX, |r| r.event.sequence);
                // From the hot window alone when it covers `start`; otherwise
                // only what the store does not have yet.
                let from = if start >= hot_start {
                    start
                } else {
                    start.max(data.published + 1)
                };
                let hot: Vec<_> = data
                    .hot
                    .iter()
                    .filter(|r| r.event.sequence >= from && r.event.sequence <= end)
                    .map(|r| r.event.clone())
                    .collect();
                (data.published, hot_start, hot)
            };
            if start >= hot_start {
                return Ok(hot);
            }
            // The hot window may start inside a batch; the published sequence
            // is always a batch boundary and everything up to it is stored.
            let mut stored = self.stored(start, end.min(published)).await?;
            stored.extend(hot);
            return Ok(stored);
        }
        // An ancestor's events, up to its cut.
        let segment = [(branch.to_owned(), start, end)];
        if let Some(mut shared) = self.shared_ancestry(&segment).await? {
            return Ok(shared.pop().unwrap_or_default());
        }
        Ok(events(
            self.0
                .store
                .read(
                    &TimelineKey {
                        session_id: self.0.key.session_id.clone(),
                        branch_id: branch.into(),
                    },
                    start..=end,
                )
                .await?,
        ))
    }
    /// This branch's stored events in `start..=end`, `end` at most the
    /// published sequence. While the branch opens, the part up to the head
    /// the Timeline was built at is served from the opening's copy and only
    /// the rest is read from the store.
    async fn stored(&self, start: u64, end: u64) -> Result<Vec<Arc<AgentEvent>>> {
        let Some((mut shared, head)) = self.shared_own(start, end).await? else {
            return Ok(events(self.0.store.read(&self.0.key, start..=end).await?));
        };
        if end > head {
            shared.extend(events(
                self.0.store.read(&self.0.key, head + 1..=end).await?,
            ));
        }
        Ok(shared)
    }
    /// While the branch opens, its events in `start..=end` up to the head,
    /// sliced from the opening's copy, with that head; `None` when they are
    /// read from the store. Only a read of the whole part fills the copy: a
    /// read of a part uses it once it exists and reads alone before, so it
    /// never decodes the events before its start.
    async fn shared_own(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Option<(Vec<Arc<AgentEvent>>, u64)>> {
        let Some(opening) = self.opening() else {
            return Ok(None);
        };
        let head = opening.head;
        if start > end || start > head {
            return Ok(None);
        }
        let own = if start == 1 && end >= head {
            let fill = async {
                let records = self.0.store.read(&self.0.key, 1..=head).await?;
                Ok::<_, AgentError>(entries(records))
            };
            Some(opening.own.get_or_try_init(|| fill).await?)
        } else {
            opening.own.get()
        };
        own.map(|own| Ok((slice(own, start, end.min(head))?, head)))
            .transpose()
    }
    /// While the branch opens, the events of ancestor `segments`, each
    /// `(branch, start, end)`, sliced from the opening's copy of its
    /// ancestry, one list per segment; `None` when they are read from the
    /// store. As for the own part, only a read of the whole ancestry (every
    /// ancestor from 1 to its cut) fills the copy, and a read of a part uses
    /// it once it exists and reads alone before.
    async fn shared_ancestry(
        &self,
        segments: &[(String, u64, u64)],
    ) -> Result<Option<Vec<Vec<Arc<AgentEvent>>>>> {
        let Some(opening) = self.opening() else {
            return Ok(None);
        };
        if opening.ancestry.is_empty() {
            return Ok(None);
        }
        let whole = segments.len() == opening.ancestry.len()
            && segments.iter().zip(&opening.ancestry).all(
                |((branch, start, end), (ancestor, cut))| {
                    branch == ancestor && *start == 1 && end == cut
                },
            );
        let ancestors = if whole {
            let fill = async {
                let ranges = opening
                    .ancestry
                    .iter()
                    .map(|(ancestor, cut)| (ancestor.clone(), 1..=*cut))
                    .collect();
                let read = self
                    .0
                    .store
                    .read_segments(&self.0.key.session_id, ranges)
                    .await?;
                Ok::<_, AgentError>(read.into_iter().map(entries).collect::<Vec<_>>())
            };
            Some(opening.ancestors.get_or_try_init(|| fill).await?)
        } else {
            opening.ancestors.get()
        };
        let Some(ancestors) = ancestors else {
            return Ok(None);
        };
        let mut shared = Vec::with_capacity(segments.len());
        for (branch, start, end) in segments {
            // Only ranges inside the ancestry the branch was built with.
            let Some(index) = opening
                .ancestry
                .iter()
                .position(|(ancestor, cut)| ancestor == branch && end <= cut)
            else {
                return Ok(None);
            };
            if start > end {
                return Ok(None);
            }
            shared.push(slice(&ancestors[index], *start, *end)?);
        }
        Ok(Some(shared))
    }
    async fn history_records(
        &self,
        segments: Vec<(String, u64, u64)>,
    ) -> Result<Vec<Vec<Arc<AgentEvent>>>> {
        if let [(branch, start, end)] = &segments[..] {
            return Ok(vec![self.records(branch, *start, *end).await?]);
        }
        // Ancestor prefixes come from the store in one read; this branch's
        // own segment also covers events not yet published.
        let own = segments
            .last()
            .filter(|(branch, _, _)| *branch == self.0.key.branch_id)
            .cloned();
        let ancestors: Vec<_> = segments
            .into_iter()
            .take_while(|(branch, _, _)| *branch != self.0.key.branch_id)
            .collect();
        let mut records = if ancestors.is_empty() {
            Vec::new()
        } else if let Some(shared) = self.shared_ancestry(&ancestors).await? {
            shared
        } else {
            self.0
                .store
                .read_segments(
                    &self.0.key.session_id,
                    ancestors
                        .into_iter()
                        .map(|(branch, start, end)| (branch, start..=end))
                        .collect(),
                )
                .await?
                .into_iter()
                .map(events)
                .collect()
        };
        if let Some((branch, start, end)) = own {
            records.push(self.records(&branch, start, end).await?);
        }
        Ok(records)
    }
    /// Starts the branch opening: while `lifecycle` is `Loading`, reads of
    /// the history this Timeline was built with share one decoded copy (see
    /// [`Opening`]). The lifecycle holds the opening and drops it when the
    /// branch leaves `Loading`; reads after that go to the store, and a
    /// reader that already holds the copy finishes with it. Called once, by
    /// the branch scope that builds both; `count` counts the opening while it
    /// is alive.
    pub(crate) fn begin_opening(&self, lifecycle: &BranchLifecycle, count: &OpeningCount) {
        let head = self.0.restored_sequence;
        let ancestry: Vec<_> = self
            .0
            .ancestors
            .iter()
            .filter(|(_, cut)| *cut > 0)
            .cloned()
            .collect();
        // A new branch without ancestry has nothing to share.
        if head == 0 && ancestry.is_empty() {
            return;
        }
        let opening = Arc::new(Opening {
            head,
            own: OnceCell::new(),
            ancestry,
            ancestors: OnceCell::new(),
            count: count.opened(),
        });
        if self.0.opening.set(Arc::downgrade(&opening)).is_ok() {
            lifecycle.hold(opening);
        }
    }
    fn opening(&self) -> Option<Arc<Opening>> {
        self.0.opening.get().and_then(Weak::upgrade)
    }
    pub async fn history(&self, through: Option<u64>) -> Result<Vec<Arc<AgentEvent>>> {
        let through = self.through(through)?;
        let segments = self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), through)))
            .filter(|(_, cut)| *cut > 0)
            .map(|(branch, cut)| (branch, 1, cut))
            .collect();
        Ok(self
            .history_records(segments)
            .await?
            .into_iter()
            .flatten()
            .collect())
    }
    pub async fn local<R: RangeBounds<u64> + Send>(
        &self,
        range: R,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let start = match range.start_bound() {
            Bound::Included(n) => *n,
            Bound::Excluded(n) => n.saturating_add(1),
            Bound::Unbounded => 1,
        }
        .max(1);
        let end = match range.end_bound() {
            Bound::Included(n) => *n,
            Bound::Excluded(n) => n.saturating_sub(1),
            Bound::Unbounded => self.committed(),
        };
        let end = self.through(Some(end))?;
        self.records(&self.0.key.branch_id, start, end).await
    }
    /// Full fact replay; no interpreter or snapshot is required.
    pub async fn context(&self, through: u64) -> Result<ContextHistory> {
        self.context_from(through, None).await
    }
    /// Select an interpreter-compatible cache before decoding or trimming facts.
    /// Invalid caches are disposable; failures reading required facts are not.
    pub async fn context_with_checkpoint(
        &self,
        through: u64,
        interpreter: &str,
        decode: fn(&RecoverySnapshot, &AgentEvent) -> Option<ProjectionCheckpoint>,
    ) -> Result<ContextHistory> {
        let through = self.through(Some(through))?;
        let segments: Vec<_> = self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), through)))
            .collect();
        for (index, (branch, cut)) in segments.iter().enumerate().rev() {
            if *cut == 0 {
                continue;
            }
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            let Some(snapshot) = self
                .0
                .store
                .latest_snapshot(&key, *cut, interpreter)
                .await?
            else {
                continue;
            };
            if snapshot.interpreter != interpreter
                || snapshot.sequence == 0
                || snapshot.sequence > *cut
            {
                continue;
            }
            // The anchor is a fact, not copied cache metadata. Reading to the
            // legal branch cut also supports an anchor inside an atomic batch.
            // Only structure is checked here; whether the anchor can carry
            // the state is the interpreter's decision.
            let anchor = self
                .records(branch, snapshot.sequence, *cut)
                .await?
                .into_iter()
                .next();
            let Some(anchor) = anchor else { continue };
            if anchor.id != snapshot.event_id || anchor.sequence != snapshot.sequence {
                continue;
            }
            let Some(checkpoint) = decode(&snapshot, &anchor) else {
                continue;
            };
            let Some(resume_index) = segments[..=index]
                .iter()
                .position(|(id, _)| *id == checkpoint.resume.branch_id)
            else {
                continue;
            };
            if checkpoint.resume.sequence == 0
                || checkpoint.resume.sequence > segments[resume_index].1
                || (resume_index == index && checkpoint.resume.sequence >= snapshot.sequence)
            {
                continue;
            }
            return self.context_from(through, Some(checkpoint)).await;
        }
        self.context(through).await
    }
    async fn context_from(
        &self,
        through: u64,
        checkpoint: Option<ProjectionCheckpoint>,
    ) -> Result<ContextHistory> {
        let through = self.through(Some(through))?;
        let segments = self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), through)));
        let resume = checkpoint.as_ref().map(|checkpoint| &checkpoint.resume);
        let mut active = resume.is_none();
        let ranges: Vec<_> = segments
            .filter_map(|(branch, cut)| {
                let start = match &resume {
                    Some(position) if !active && position.branch_id == branch => {
                        active = true;
                        position.sequence
                    }
                    _ => 1,
                };
                (active && cut > 0).then_some((branch, start, cut))
            })
            .collect();
        let records = self.history_records(ranges.clone()).await?;
        let mut events = Vec::new();
        let mut positions = Vec::new();
        for ((branch, _, _), records) in ranges.into_iter().zip(records) {
            for event in records {
                positions.push(TimelinePosition {
                    branch_id: branch.clone(),
                    sequence: event.sequence,
                });
                events.push(event);
            }
        }
        Ok(ContextHistory {
            checkpoint,
            ancestry: self
                .0
                .ancestors
                .iter()
                .map(|(branch, _)| branch.clone())
                .collect(),
            events,
            positions,
        })
    }
    /// Waits for the anchor's durable publication, then writes only the cache.
    /// The owner tracks this future along with its other preparation work.
    pub async fn save_snapshot(&self, snapshot: RecoverySnapshot) -> Result<()> {
        self.through(Some(snapshot.sequence))?;
        let mut changed = self.watch();
        loop {
            if let Some(error) = self.failure() {
                return Err(error);
            }
            if self.sequence() >= snapshot.sequence {
                break;
            }
            changed
                .changed()
                .await
                .map_err(|_| AgentError::Closed(self.0.key.branch_id.clone()))?;
        }
        self.0.store.save_snapshot(&self.0.key, snapshot).await
    }
    pub async fn trace(&self, sequence: u64) -> Result<Option<TimelineTrace>> {
        self.through(Some(sequence))?;
        {
            let data = self.0.data.lock().unwrap();
            if let Some(record) = data.hot.iter().find(|r| r.event.sequence == sequence) {
                return Ok(record.trace.clone());
            }
        }
        self.0.store.trace(&self.0.key, sequence).await
    }
    pub(crate) fn read(&self, after: u64) -> EventReader {
        self.0.data.lock().unwrap().reader_active = true;
        EventReader {
            timeline: self.clone(),
            next: after.saturating_add(1),
            changed: self.0.changed.subscribe(),
        }
    }
    /// Releases the Timeline's ownership after every handler has returned.
    pub(crate) fn dispatched(&self, sequence: u64) {
        let mut data = self.0.data.lock().unwrap();
        while data
            .hot
            .front()
            .is_some_and(|r| r.event.sequence <= sequence)
        {
            data.hot.pop_front();
        }
    }
    pub fn watch(&self) -> watch::Receiver<(u64, bool)> {
        self.0.changed.subscribe()
    }
    /// Freezes append immediately; completion waits for all accepted batches.
    /// Holds the commit lock while nothing is pending, so no append can start
    /// until the returned lock is dropped or frozen. Used by idle release,
    /// which checks every branch before freezing any of them.
    pub(crate) fn lock_tail(&self) -> Option<TailLock<'_>> {
        let commit = self.0.commit.lock().unwrap();
        let data = self.0.data.lock().unwrap();
        if data.closing || data.failure.is_some() || data.allocated != data.published {
            return None;
        }
        let sequence = data.published;
        drop(data);
        Some(TailLock {
            timeline: self,
            sequence,
            _commit: commit,
        })
    }
    /// Reopens append after idle release has stopped dispatch, for finalizers.
    pub(crate) fn thaw(&self) {
        let _commit = self.0.commit.lock().unwrap();
        self.0.data.lock().unwrap().closing = false;
    }
    /// Closes this Timeline. Only its owner calls it: a branch's Timeline is
    /// settled and closed by its TimelineTree (principles §2), so it is not a
    /// public method and is not exported to other languages.
    pub(crate) fn close(&self) -> BoxFuture<'static, Result<()>> {
        let mut changed = self.watch();
        {
            let _commit = self.0.commit.lock().unwrap();
            let mut data = self.0.data.lock().unwrap();
            data.closing = true;
            data.closed = data.failure.is_some() || data.published == data.allocated;
            if data.closed && !data.reader_active {
                data.hot.clear();
            }
            self.0.changed.send_replace((data.published, data.closed));
        }
        let this = self.clone();
        Box::pin(async move {
            loop {
                if let Some(error) = this.failure() {
                    return Err(error);
                }
                if changed.borrow().1 {
                    return Ok(());
                }
                changed.changed().await.map_err(|_| {
                    AgentError::fault("timeline.close", "publication channel ended")
                })?;
            }
        })
    }
    /// Registers a trusted transform owned by `ctx`, which must be inside the
    /// branch; it unloads with that plugin. `keys` follow the Dispatcher rules. Middleware registered earlier
    /// wraps later ones, so later registrations transform first. The wrapper
    /// always calls downstream: a handler cannot intercept the chain.
    pub fn before(
        &self,
        ctx: &Ctx,
        name: impl Into<String>,
        keys: &[&str],
        handler: impl Fn(EventDraft) -> Result<EventDraft> + Send + Sync + 'static,
    ) -> Result<Disposer> {
        let name = name.into();
        let keys = KeyFilter::new(keys)?;
        let middleware =
            move |_: &Ctx, offered: &BeforeCommit, next: SyncNext<'_, BeforeCommit>| {
                let draft = next.call()?;
                let key = draft.kind.key();
                if !keys.matches(key) {
                    return Ok(draft);
                }
                let result = handler(draft.clone())
                    .map_err(|error| CordisError::PluginFailed(Box::new(error)))?;
                if result.kind.key() != key {
                    return Err(CordisError::PluginFailed(
                        format!("{name} changed the event kind").into(),
                    ));
                }
                offered.steps.lock().unwrap().push(TraceStep {
                    handler: name.clone(),
                    result: (result != draft).then(|| result.clone()),
                });
                Ok(result)
            };
        Ok(ctx
            .events()
            .on_waterfall_sync(ctx, &self.before_key(), middleware)?)
    }
    fn before_key(&self) -> EventKey<BeforeCommit> {
        EventKey::of().instance(self.0.instance)
    }
}

pub(crate) struct EventReader {
    timeline: Timeline,
    next: u64,
    changed: watch::Receiver<(u64, bool)>,
}
impl Drop for EventReader {
    fn drop(&mut self) {
        let mut data = self.timeline.0.data.lock().unwrap();
        data.reader_active = false;
        let published = data.published;
        data.hot.retain(|record| record.event.sequence > published);
    }
}
impl EventReader {
    pub async fn next(&mut self) -> Result<Option<Arc<AgentEvent>>> {
        loop {
            {
                let data = self.timeline.0.data.lock().unwrap();
                if let Some(error) = &data.failure {
                    return Err(error.clone());
                }
                if self.next <= data.published {
                    let event = data
                        .hot
                        .iter()
                        .find(|r| r.event.sequence == self.next)
                        .expect("dispatcher event remains in hot window")
                        .event
                        .clone();
                    self.next += 1;
                    return Ok(Some(event));
                }
                if data.closed {
                    return Ok(None);
                }
            }
            if self.changed.changed().await.is_err() {
                return Ok(None);
            }
        }
    }
}
/// The events of a store read; reads of committed history return events
/// only, traces have their own read.
fn events(records: Vec<StoredEvent>) -> Vec<Arc<AgentEvent>> {
    records.into_iter().map(|record| record.event).collect()
}

/// The history a branch was built with, shared by its readers while the
/// branch opens (its lifecycle's `Loading` phase): its own events up to
/// `head`, and its ancestry, each ancestor up to its cut. Every rebuilding
/// capability reads these prefixes, so the first read that needs all of a
/// part decodes it once; readers that need it meanwhile wait for that read,
/// which holds no lock, and a failed read is not kept. Events are facts once
/// sequenced, so a slice of the copy, after the store's end check, is what a
/// store read of that range returns. Events after `head` are never kept:
/// they are read as outside an opening.
pub(crate) struct Opening {
    head: u64,
    own: OnceCell<Vec<Entry>>,
    /// The ancestors with a cut above 0, in ancestry order.
    ancestry: Vec<(String, u64)>,
    /// One list per ancestor of `ancestry`.
    ancestors: OnceCell<Vec<Vec<Entry>>>,
    count: OpeningCount,
}
impl Drop for Opening {
    fn drop(&mut self) {
        self.count.closed();
    }
}
/// How many of an Agent's branch openings are alive, with or without their
/// copy. Only the `testing` feature counts, for tests of their release.
#[derive(Clone, Default)]
pub(crate) struct OpeningCount(#[cfg(feature = "testing")] Arc<std::sync::atomic::AtomicUsize>);
impl OpeningCount {
    /// Counts one more opening; the opening uncounts itself when dropped.
    fn opened(&self) -> Self {
        #[cfg(feature = "testing")]
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.clone()
    }
    fn closed(&self) {
        #[cfg(feature = "testing")]
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(feature = "testing")]
    pub(crate) fn alive(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
/// One event of the copy and the end of the atomic batch it was committed
/// in, as the fill read them; the trace is not kept.
struct Entry {
    event: Arc<AgentEvent>,
    batch_end: u64,
}
fn entries(records: Vec<StoredEvent>) -> Vec<Entry> {
    records
        .into_iter()
        .map(|record| Entry {
            event: record.event,
            batch_end: record.batch_end,
        })
        .collect()
}
/// The events of `entries`, in sequence order, with sequences in
/// `start..=end`. As in a store read, the start may be inside an atomic
/// batch and the end must be a batch boundary.
fn slice(entries: &[Entry], start: u64, end: u64) -> Result<Vec<Arc<AgentEvent>>> {
    let from = entries.partition_point(|entry| entry.event.sequence < start);
    let to = entries.partition_point(|entry| entry.event.sequence <= end);
    let entries = &entries[from..to];
    if entries.last().is_some_and(|entry| entry.batch_end > end) {
        return Err(AgentError::InvalidInput(format!(
            "sequence {end} is inside an atomic batch"
        )));
    }
    Ok(entries.iter().map(|entry| entry.event.clone()).collect())
}
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before Unix epoch")
        .as_millis() as u64
}

thread_local! { static APPENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
struct AppendGuard;
impl AppendGuard {
    fn enter() -> Result<Self> {
        APPENDING.with(|active| {
            if active.replace(true) {
                Err(AgentError::fault(
                    "timeline.before",
                    "nested append is not allowed",
                ))
            } else {
                Ok(Self)
            }
        })
    }
}
impl Drop for AppendGuard {
    fn drop(&mut self) {
        APPENDING.with(|active| active.set(false));
    }
}

pub(crate) struct TailLock<'a> {
    timeline: &'a Timeline,
    sequence: u64,
    _commit: std::sync::MutexGuard<'a, ()>,
}
impl TailLock<'_> {
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Stops accepting appends; the tail stays exactly at [`Self::sequence`].
    pub(crate) fn freeze(self) {
        self.timeline.0.data.lock().unwrap().closing = true;
    }
}

/// Closes a Timeline a test built directly with `Timeline::new`. A free
/// function rather than a method, so the bridge generator never exports it.
#[cfg(feature = "testing")]
pub fn close_timeline(timeline: &Timeline) -> BoxFuture<'static, Result<()>> {
    timeline.close()
}
