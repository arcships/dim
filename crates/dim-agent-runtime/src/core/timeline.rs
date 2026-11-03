use super::keys::KeyFilter;
use crate::{
    AgentError, BoxFuture, BranchCursor, BranchLifecycle, BranchPage, FaultReporter, PolicyFacts,
    PolicyOrder, PolicyOrderEvent, PolicyWindow, Result, StoredEvent, TimelineKey, TimelineQuery,
    TimelineQueryDirection, TimelineQueryReply, TimelineQueryScope, TimelineStore,
};
use dim_agent_types::{
    AgentEvent, EventDraft, EventKind, InputMessageData, ProjectionCheckpoint, RecoverySnapshot,
    TimelinePosition,
};
use rutis::events::SyncCall;
#[allow(unused_imports)]
use rutis::host::{CtxHost, ErrorKind, InstanceId};
use rutis::service;
use rutis::{Ctx, Disposer, Event};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    ops::{Bound, RangeBounds},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{OnceCell, watch};
use uuid::Uuid;

/// Which turn starts a policy order read keeps.
#[derive(Debug, Clone, Copy)]
pub enum PolicyTurns<'a> {
    /// Only each segment's last turn start, next to its last compaction
    /// start: the anchors a streamed context resolves cut ids against.
    Anchors,
    /// Every turn start of every segment.
    All,
    /// Every turn start after this chain position.
    After(&'a TimelinePosition),
}

/// One draft offered to the branch's `before` middleware. Each wrapper records
/// the step it applied, so steps follow the order the transforms took effect.
/// The steps stay local: the event is dispatched only by its own branch's
/// synchronous waterfall, in process, so listeners borrow this value.
#[derive(Serialize, Deserialize)]
struct BeforeCommit {
    /// The branch's private name it is dispatched under (`before_name`).
    #[serde(skip)]
    name: String,
    draft: EventDraft,
    #[serde(skip)]
    steps: Mutex<Vec<TraceStep>>,
}
impl Event for BeforeCommit {
    const NAME: &'static str = "";
    type Value = EventDraft;
    fn name(&self) -> &str {
        &self.name
    }
}
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
    /// The branch instance that names the private before-commit channel.
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
    /// The submission ids of the inputs the branch was restored with, set
    /// before the branch accepts input.
    restored: OnceLock<HashMap<String, Submission>>,
    /// The submission ids of the inputs committed since, written under the
    /// commit lock.
    submissions: Mutex<HashMap<String, Submission>>,
    changed: watch::Sender<(u64, bool)>,
    faults: FaultReporter,
}
/// The inputs a branch holds for one submission id: one, by its sequence,
/// or several, written before the branch accepted each id once.
#[derive(Debug, Clone, Copy)]
enum Submission {
    Input(u64),
    Ambiguous,
}
fn index_submission(index: &mut HashMap<String, Submission>, event: &AgentEvent) {
    if let EventKind::InputMessage(InputMessageData {
        submission_id: Some(id),
        ..
    }) = &event.kind
    {
        index
            .entry(id.clone())
            .and_modify(|found| *found = Submission::Ambiguous)
            .or_insert(Submission::Input(event.sequence));
    }
}
/// What [`Timeline::input`] did.
pub(crate) enum Appended {
    Written(Arc<AgentEvent>),
    /// An input with the same submission id is already committed at this
    /// sequence; nothing was written.
    Existing(u64),
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
impl Data {
    /// Whether the Timeline accepts appends: it is not closing or failed and
    /// every allocated event is published.
    fn accepts_appends(&self) -> bool {
        !self.closing && self.failure.is_none() && self.allocated == self.published
    }
}
/// The reply from one reply per segment: facts in chain order and then
/// sequence, each with its position in the same order.
#[service]
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
            restored: if sequence == 0 {
                OnceLock::from(HashMap::new())
            } else {
                OnceLock::new()
            },
            submissions: Mutex::default(),
            changed: watch::channel((sequence, false)).0,
            faults,
        }))
    }
    pub(crate) fn key(&self) -> &TimelineKey {
        &self.0.key
    }
    pub fn branch_id(&self) -> String {
        self.0.key.branch_id.clone()
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
    #[service(skip)]
    pub fn append_registered(
        &self,
        original: EventDraft,
        register: impl FnOnce(&AgentEvent) -> Result<()>,
    ) -> Result<Arc<AgentEvent>> {
        let mut events =
            self.append_batch_registered(vec![original], |events| register(&events[0]))?;
        Ok(events.pop().expect("single-event batch returned one event"))
    }
    #[service(skip)]
    pub fn append_batch_registered(
        &self,
        originals: Vec<EventDraft>,
        register: impl FnOnce(&[Arc<AgentEvent>]) -> Result<()>,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let _append = AppendGuard::enter()?;
        let _commit = self.0.commit.lock().unwrap();
        self.commit(originals, register)
    }
    /// Appends `data` as one `input/message`, unless an input with its
    /// submission id is already committed on this branch: the branch accepts
    /// each id once. The check and the append hold the commit lock, so of
    /// concurrent inputs with one id exactly one is written.
    pub(crate) fn input_checked(
        &self,
        data: InputMessageData,
        admit: impl FnOnce() -> Result<()>,
    ) -> Result<Appended> {
        let _append = AppendGuard::enter()?;
        let _commit = self.0.commit.lock().unwrap();
        if let Some(id) = &data.submission_id
            && let Some(sequence) = self.submission(id)?
        {
            return Ok(Appended::Existing(sequence));
        }
        admit()?;
        let mut events = self.commit(vec![EventKind::InputMessage(data).into()], |_| Ok(()))?;
        Ok(Appended::Written(
            events.pop().expect("single-event batch returned one event"),
        ))
    }
    /// The sequence of the committed input with submission id `id`.
    pub(crate) fn submission(&self, id: &str) -> Result<Option<u64>> {
        let restored = self.0.restored.get().ok_or_else(|| {
            AgentError::fault("timeline.input", "restored submissions are not indexed")
        })?;
        let committed = self.0.submissions.lock().unwrap();
        match (restored.get(id), committed.get(id)) {
            (None, None) => Ok(None),
            (Some(Submission::Input(sequence)), None)
            | (None, Some(Submission::Input(sequence))) => Ok(Some(*sequence)),
            _ => Err(AgentError::AmbiguousSubmission(id.into())),
        }
    }
    /// Indexes the submission ids of the inputs the branch was restored
    /// with. The branch's build calls it before the branch accepts input; it
    /// reads the opening's copy of the branch's own history.
    pub(crate) async fn index_submissions(&self) -> Result<()> {
        if self.0.restored.get().is_some() {
            return Ok(());
        }
        let mut restored = HashMap::new();
        for event in self.local(1..=self.0.restored_sequence).await? {
            index_submission(&mut restored, &event);
        }
        let _ = self.0.restored.set(restored);
        Ok(())
    }
    /// Commits a batch; the caller holds the append guard and commit lock.
    fn commit(
        &self,
        originals: Vec<EventDraft>,
        register: impl FnOnce(&[Arc<AgentEvent>]) -> Result<()>,
    ) -> Result<Vec<Arc<AgentEvent>>> {
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
            let offered = Arc::new(BeforeCommit {
                name: self.before_name(),
                draft: original.clone(),
                steps: Mutex::default(),
            });
            let result = self
                .0
                .ctx
                .waterfall(offered.clone())
                .sync(|offered| Ok(Some(offered.draft.clone())));
            let (original, draft, steps) = match result {
                Ok(Some(draft)) => {
                    let steps = std::mem::take(&mut *offered.steps.lock().unwrap());
                    (original, draft, steps)
                }
                Ok(None) | Err(_) => (original.clone(), original, Vec::new()),
            };
            let event_id = draft.id.clone().or_else(|| original.id.clone());
            let event = Arc::new(AgentEvent {
                id: event_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
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
        {
            let mut submissions = self.0.submissions.lock().unwrap();
            for event in &events {
                index_submission(&mut submissions, event);
            }
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
    #[service(skip)]
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
            let mut stored = self.stored(start, end.min(published)).await?;
            stored.extend(hot);
            return Ok(stored);
        }
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
    /// The event a context checkpoint is anchored at, out of the events of
    /// `branch` up to `cut`. The point read decodes one row. A row inside an
    /// atomic batch cannot end a range, so an invalid point read falls back to
    /// the segment read this check made before the point read existed.
    async fn anchor(
        &self,
        branch: &str,
        sequence: u64,
        cut: u64,
    ) -> Result<Option<Arc<AgentEvent>>> {
        match self.records(branch, sequence, sequence).await {
            Ok(records) => Ok(records.into_iter().next()),
            Err(AgentError::InvalidInput(_)) => Ok(self
                .records(branch, sequence, cut)
                .await?
                .into_iter()
                .next()),
            Err(error) => Err(error),
        }
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
    /// One bounded read over this branch or over its chain ([ADR 019](../../../../docs/adr/019-event-query.md),
    /// [plugin contract §5.4](../../../../docs/design/plugin-contract.md#54-the-event-query)):
    /// the direction, kinds, flags, event id, predicate, limit and offset
    /// select the facts, and the reply is up to `limit` of them in chain order
    /// and then sequence, in the query's direction, each with its position. A
    /// chain read is each ancestor to its cut and then this branch, root first
    /// ascending and this branch first descending; the offset the earlier
    /// segment did not use moves to the next one, so paging is skipping and a
    /// page shorter than the limit means the selection is exhausted. The read
    /// never returns a fact above a branch's coverage: an ancestor to its cut,
    /// this branch to its published head.
    pub async fn query(&self, query: TimelineQuery) -> Result<TimelineQueryReply> {
        use TimelineQueryDirection::Descending;

        query.validate()?;
        let published = self.sequence();
        let clips: Vec<(String, u64)> = match query.scope {
            TimelineQueryScope::Branch => vec![(self.0.key.branch_id.clone(), published)],
            TimelineQueryScope::Chain => self
                .0
                .ancestors
                .iter()
                .filter(|(_, cut)| *cut > 0)
                .cloned()
                .chain(std::iter::once((self.0.key.branch_id.clone(), published)))
                .collect(),
        };
        let anchor = match (&query.from_event, query.from_sequence) {
            (None, None) => None,
            (None, Some(sequence)) => Some((
                match query.scope {
                    TimelineQueryScope::Branch => None,
                    TimelineQueryScope::Chain => Some(self.0.key.branch_id.clone()),
                },
                sequence,
            )),
            (Some(event_id), None) => {
                let mut found = None;
                for (branch, clip) in &clips {
                    let key = TimelineKey {
                        session_id: self.0.key.session_id.clone(),
                        branch_id: branch.clone(),
                    };
                    let probe = TimelineQuery {
                        scope: TimelineQueryScope::Branch,
                        from_event: None,
                        from_sequence: None,
                        order_by: query.order_by,
                        direction: TimelineQueryDirection::Ascending,
                        kinds: Vec::new(),
                        flags: Vec::new(),
                        event_id: Some(event_id.clone()),
                        predicate: None,
                        limit: std::num::NonZeroU64::new(1).expect("one is nonzero"),
                        offset: 0,
                    };
                    let page = self.0.store.query(&key, &probe, 1, *clip).await?;
                    if let Some(position) = page.positions.first() {
                        found = Some((Some(branch.clone()), position.sequence));
                        break;
                    }
                }
                found
            }
            (Some(_), Some(_)) => unreachable!("validate rejects two anchors"),
        };
        if query.from_event.is_some() && anchor.is_none() {
            return Ok(TimelineQueryReply {
                events: Vec::new(),
                positions: Vec::new(),
            });
        }
        let anchor_index = match &anchor {
            Some((Some(branch), _)) => Some(
                clips
                    .iter()
                    .position(|(name, _)| name == branch)
                    .ok_or_else(|| {
                        AgentError::InvalidInput(format!(
                            "query start names branch {branch}, which the scope does not cover"
                        ))
                    })?,
            ),
            _ => None,
        };
        let order: Vec<usize> = match query.direction {
            TimelineQueryDirection::Ascending => (0..clips.len()).collect(),
            Descending => (0..clips.len()).rev().collect(),
        };
        let limit = usize::try_from(query.limit.get()).unwrap_or(usize::MAX);
        let mut skip = query.offset;
        let mut events = Vec::new();
        let mut positions = Vec::new();
        for index in order {
            if events.len() >= limit {
                break;
            }
            let (branch, clip) = &clips[index];
            let from = match (&anchor, anchor_index, query.direction) {
                (None, _, _) => 1,
                (
                    Some((Some(_), sequence)),
                    Some(start_index),
                    TimelineQueryDirection::Ascending,
                ) => {
                    if index < start_index {
                        continue;
                    }
                    if index == start_index { *sequence } else { 1 }
                }
                (Some((Some(_), _)), Some(start_index), TimelineQueryDirection::Descending) => {
                    if index > start_index {
                        continue;
                    }
                    1
                }
                (Some((None, sequence)), _, TimelineQueryDirection::Ascending) => *sequence,
                (Some((None, _)), _, TimelineQueryDirection::Descending) => 1,
                (Some(_), None, _) => 1,
            };
            let bound = match (&anchor, anchor_index, query.direction) {
                (
                    Some((Some(_), sequence)),
                    Some(start_index),
                    TimelineQueryDirection::Descending,
                ) if index == start_index => (*clip).min(*sequence),
                (Some((None, sequence)), _, TimelineQueryDirection::Descending) => {
                    (*clip).min(*sequence)
                }
                _ => *clip,
            };
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            let page = self
                .0
                .store
                .query(
                    &key,
                    &TimelineQuery {
                        scope: TimelineQueryScope::Branch,
                        from_event: None,
                        from_sequence: None,
                        order_by: query.order_by,
                        direction: query.direction,
                        kinds: query.kinds.clone(),
                        flags: query.flags.clone(),
                        event_id: query.event_id.clone(),
                        predicate: query.predicate.clone(),
                        limit: std::num::NonZeroU64::new(
                            u64::try_from(limit - events.len())
                                .unwrap_or(u64::MAX)
                                .max(1),
                        )
                        .expect("a read keeps one event of its limit"),
                        offset: skip,
                    },
                    from,
                    bound,
                )
                .await?;
            let reached = page.skipped + page.events.len() as u64;
            skip = skip.saturating_sub(reached);
            events.extend(page.events);
            positions.extend(page.positions);
        }
        Ok(TimelineQueryReply { events, positions })
    }
    /// The first page of a backward, kind-filtered read of the branch chain
    /// at a published `through`.
    #[service(skip)]
    pub fn page_cursor(&self, through: u64) -> Result<BranchCursor> {
        let segments = self.segments(through)?;
        let segment = segments.len().checked_sub(1);
        Ok(BranchCursor {
            through,
            segment,
            before: segment.map_or(0, |index| segments[index].1),
        })
    }
    /// One page of that read: the newest `limit` stored rows of `kinds` at or
    /// below the cursor's sequence, oldest first. The next page continues the
    /// same segment or moves to the ancestor before it.
    #[service(skip)]
    pub async fn page(
        &self,
        cursor: BranchCursor,
        limit: usize,
        kinds: &[&str],
    ) -> Result<BranchPage> {
        let segments = self.segments(cursor.through)?;
        let exhausted = BranchPage {
            events: vec![],
            next: None,
            own: false,
        };
        if limit == 0 || kinds.is_empty() {
            return Ok(exhausted);
        }
        let (mut segment, mut before) = (cursor.segment, cursor.before);
        loop {
            let Some(index) = segment else {
                return Ok(exhausted);
            };
            let Some((branch, _)) = segments.get(index) else {
                return Ok(exhausted);
            };
            if before == 0 {
                segment = index.checked_sub(1);
                before = segment.map_or(0, |prev| segments[prev].1);
                continue;
            }
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            let records = self
                .0
                .store
                .read_kind_page(&key, kinds, before, limit)
                .await?;
            let Some(first) = records.first().map(|record| record.event.sequence) else {
                segment = index.checked_sub(1);
                before = segment.map_or(0, |prev| segments[prev].1);
                continue;
            };
            let next = if first > 1 {
                Some(BranchCursor {
                    through: cursor.through,
                    segment: Some(index),
                    before: first - 1,
                })
            } else {
                index.checked_sub(1).map(|prev| BranchCursor {
                    through: cursor.through,
                    segment: Some(prev),
                    before: segments[prev].1,
                })
            };
            return Ok(BranchPage {
                events: records.into_iter().map(|record| record.event).collect(),
                next,
                own: cursor.through > 0 && Some(index) == segments.len().checked_sub(1),
            });
        }
    }
    #[service(skip)]
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
    /// Reads complete batches after `after`, targeting `limit` events and
    /// never ending inside an atomic batch. Projection reads do not use
    /// stored requests, so `context/prepare` and `model/call` records carry
    /// an empty request.
    #[service(skip)]
    pub async fn stored_page(
        &self,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let through = self.through(Some(through))?;
        Ok(self
            .0
            .store
            .read_page_without_request_bodies(&self.0.key, after, limit, through)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }

    /// The branch chain the context covers, root first: each ancestor to its
    /// fixed cut and the local branch to `through`.
    #[service(skip)]
    pub fn segments(&self, through: u64) -> Result<Vec<(String, u64)>> {
        let through = self.through(Some(through))?;
        Ok(self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), through)))
            .filter(|(_, cut)| *cut > 0)
            .collect())
    }

    /// The ancestor events of one kind, root first.
    #[service(skip)]
    pub async fn ancestor_events_of_kind(&self, kind: &str) -> Result<Vec<Arc<AgentEvent>>> {
        let mut events = Vec::new();
        for (branch, cut) in &self.0.ancestors {
            if *cut == 0 {
                continue;
            }
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            events.extend(self.0.store.events_of_kind(&key, kind, *cut).await?);
        }
        Ok(events)
    }

    /// The event with this id on the local branch.
    #[service(skip)]
    pub async fn event_by_id(&self, id: &str) -> Result<Option<Arc<AgentEvent>>> {
        self.0.store.event_by_id(&self.0.key, id).await
    }

    /// The position of one event anywhere in the branch chain.
    #[service(skip)]
    pub async fn event_position(&self, id: &str) -> Result<Option<TimelinePosition>> {
        for (branch, _) in self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), 0)))
        {
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            if let Some(event) = self.0.store.event_by_id(&key, id).await? {
                return Ok(Some(TimelinePosition {
                    branch_id: branch,
                    sequence: event.sequence,
                }));
            }
        }
        Ok(None)
    }

    /// The cut of the latest completed compaction at or below `through`, when
    /// it is one of this branch's own events before the fact that names it. A
    /// completed compaction's cut is a validated context boundary, so a read
    /// may start there. A stored reference that names another branch's event or
    /// a later one is ignored: a read then replays from the segment start
    /// instead of folding a tail that never covered `through`.
    #[service(skip)]
    pub async fn compaction_cut(&self, through: u64) -> Result<Option<u64>> {
        let through = self.through(Some(through))?;
        let Some(cut) = self
            .0
            .store
            .last_compaction_cut(&self.0.key, through)
            .await?
        else {
            return Ok(None);
        };
        let Some(position) = self.event_position(&cut.retain_from).await? else {
            return Ok(None);
        };
        let usable = position.branch_id == self.0.key.branch_id && position.sequence < cut.sequence;
        Ok(usable.then_some(position.sequence))
    }

    /// One filtered page of `branch`, which may be an ancestor.
    #[service(skip)]
    pub async fn stored_context_page_in(
        &self,
        branch: &str,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let key = TimelineKey {
            session_id: self.0.key.session_id.clone(),
            branch_id: branch.to_owned(),
        };
        Ok(self
            .0
            .store
            .read_context_page(&key, after, limit, through)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }

    /// Reads the rows this projection consumes after an event cursor; a cursor
    /// inside an atomic batch reads that row and filters by sequence. The event
    /// limit is a target: the final batch may exceed it.
    pub async fn stored_context_page(
        &self,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let through = self.through(Some(through))?;
        if after > through {
            return Err(AgentError::InvalidInput("future Timeline cursor".into()));
        }
        Ok(self
            .0
            .store
            .read_context_page(&self.0.key, after, limit, through)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }
    /// A local event range read from the store, bypassing the hot cache.
    #[service(skip)]
    pub async fn stored_local<R: RangeBounds<u64> + Send>(
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
        Ok(self
            .0
            .store
            .read(&self.0.key, start..=end)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }

    /// Full fact replay; no interpreter or snapshot is required.
    #[service(skip)]
    pub async fn context(&self, through: u64) -> Result<ContextHistory> {
        self.context_from(through, None).await
    }
    /// The policy-relevant events at `through`, filtered by the store.
    #[service(skip)]
    pub async fn policy_facts(&self, through: u64, turn_id: &str) -> Result<PolicyFacts> {
        let published = self.sequence();
        if through > published {
            return Err(AgentError::InvalidInput(format!(
                "future timeline sequence {through}"
            )));
        }
        let mut events = Vec::new();
        for (branch, cut) in self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), through)))
        {
            if cut == 0 {
                continue;
            }
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch,
            };
            events.extend(self.0.store.policy_facts(&key, cut, turn_id).await?.events);
        }
        Ok(PolicyFacts { through, events })
    }
    /// The last event at or before a fixed cut, even inside its atomic batch.
    #[service(skip)]
    async fn boundary_event(&self, key: &TimelineKey, cut: u64) -> Result<Option<Arc<AgentEvent>>> {
        self.0.store.last_event(key, cut).await
    }

    /// The event order for legal compaction cuts: each segment's boundary, its
    /// last turn start and its last compaction start at its cut, and the turn
    /// starts `turns` keeps.
    #[service(skip)]
    pub async fn policy_order(&self, through: u64, turns: PolicyTurns<'_>) -> Result<PolicyOrder> {
        let through = self.through(Some(through))?;
        let segments: Vec<_> = self
            .0
            .ancestors
            .iter()
            .cloned()
            .chain(std::iter::once((self.0.key.branch_id.clone(), through)))
            .collect();
        let rank = |branch: &str| segments.iter().position(|(name, _)| name == branch);
        let window = match turns {
            PolicyTurns::Anchors => None,
            PolicyTurns::All => Some(None),
            PolicyTurns::After(at) => Some(rank(&at.branch_id).map(|index| (index, at.sequence))),
        };
        let mut events = Vec::new();
        let mut branches = Vec::new();
        for (index, (branch, cut)) in segments.into_iter().enumerate() {
            if cut == 0 {
                continue;
            }
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            branches.push(branch.clone());
            let turns = match window {
                None => cut.saturating_add(1)..=cut,
                Some(None) => 1..=cut,
                Some(Some((rank, _))) if index > rank => 1..=cut,
                Some(Some((rank, sequence))) if index == rank => sequence.saturating_add(1)..=cut,
                Some(Some(_)) => cut.saturating_add(1)..=cut,
            };
            events.extend(self.0.store.policy_order(&key, turns, cut).await?.events);
            if let Some(boundary) = self.boundary_event(&key, cut).await? {
                events.push(PolicyOrderEvent {
                    id: boundary.id.clone(),
                    branch_id: branch,
                    sequence: boundary.sequence,
                    turn_start: matches!(boundary.kind, EventKind::TurnStart(_)),
                    compaction_start: None,
                    boundary: true,
                });
            }
        }
        let rank = |branch: &str| branches.iter().position(|name| name == branch);
        events.sort_by_key(|event| (rank(&event.branch_id), event.sequence));
        Ok(PolicyOrder {
            through,
            branches,
            events,
        })
    }

    /// The anchor and turn window for a known compaction cut.
    #[service(skip)]
    pub async fn policy_window(
        &self,
        through: u64,
        turn_id: &str,
        cut: &str,
    ) -> Result<PolicyWindow> {
        let through = self.through(Some(through))?;
        let mut window = self
            .0
            .store
            .policy_window(&self.0.key, through, turn_id, cut)
            .await?;
        let current = std::mem::take(&mut window.tool_results);
        for (branch, segment_through) in &self.0.ancestors {
            if *segment_through == 0 {
                continue;
            }
            let key = TimelineKey {
                session_id: self.0.key.session_id.clone(),
                branch_id: branch.clone(),
            };
            let ancestor = self
                .0
                .store
                .policy_window(&key, *segment_through, turn_id, cut)
                .await?;
            if window.cut_position.is_none() {
                window.cut_position = ancestor.cut_position;
            }
            if !window.events.iter().any(|event| event.id == cut) {
                window.events.extend(
                    ancestor
                        .events
                        .iter()
                        .filter(|event| event.id == cut)
                        .cloned(),
                );
            }
            for result in ancestor.tool_results {
                if !window
                    .tool_results
                    .iter()
                    .any(|seen| seen.event.id == result.event.id)
                {
                    window.tool_results.push(result);
                }
            }
        }
        for result in current {
            if !window
                .tool_results
                .iter()
                .any(|seen| seen.event.id == result.event.id)
            {
                window.tool_results.push(result);
            }
        }
        Ok(window)
    }
    /// The latest interpreter-compatible checkpoint at `through`, if one is
    /// valid for this branch and its fixed ancestor chain.
    #[service(skip)]
    pub async fn checkpoint(
        &self,
        through: u64,
        interpreter: &str,
        decode: fn(&RecoverySnapshot, &AgentEvent) -> Option<ProjectionCheckpoint>,
    ) -> Result<Option<ProjectionCheckpoint>> {
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
            let Some(anchor) = self.anchor(branch, snapshot.sequence, *cut).await? else {
                continue;
            };
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
            return Ok(Some(checkpoint));
        }
        Ok(None)
    }
    /// Select an interpreter-compatible cache before decoding or trimming facts.
    /// Invalid caches are disposable; failures reading required facts are not.
    #[service(skip)]
    pub async fn context_with_checkpoint(
        &self,
        through: u64,
        interpreter: &str,
        decode: fn(&RecoverySnapshot, &AgentEvent) -> Option<ProjectionCheckpoint>,
    ) -> Result<ContextHistory> {
        let checkpoint = self.checkpoint(through, interpreter, decode).await?;
        self.context_from(through, checkpoint).await
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
    #[service(skip)]
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
    #[service(skip)]
    pub fn watch(&self) -> watch::Receiver<(u64, bool)> {
        self.0.changed.subscribe()
    }
    /// The published tail when the Timeline accepts appends: it is not
    /// closing or failed and every allocated event is published. A read that
    /// holds no lock past it.
    pub(crate) fn open_tail(&self) -> Option<u64> {
        let data = self.0.data.lock().unwrap();
        data.accepts_appends().then_some(data.published)
    }
    /// Freezes append immediately; completion waits for all accepted batches.
    /// Holds the commit lock while nothing is pending, so no append can start
    /// until the returned lock is dropped or frozen. Used by idle release,
    /// which checks every branch before freezing any of them.
    pub(crate) fn lock_tail(&self) -> Option<TailLock<'_>> {
        let commit = self.0.commit.lock().unwrap();
        let data = self.0.data.lock().unwrap();
        if !data.accepts_appends() {
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
    #[service(skip)]
    pub fn before(
        &self,
        ctx: &Ctx,
        name: impl Into<String>,
        keys: &[&str],
        handler: impl Fn(EventDraft) -> Result<EventDraft> + Send + Sync + 'static,
    ) -> Result<Disposer> {
        let name = name.into();
        let keys = KeyFilter::new(keys)?;
        let middleware = move |offered: Arc<BeforeCommit>, next: SyncCall<'_, BeforeCommit>| {
            let draft = next.next()?.ok_or_else(|| {
                rutis::Error::from(ErrorKind::PluginFailed(
                    "the before chain returned no draft".into(),
                ))
            })?;
            let key = draft.kind.key();
            if !keys.matches(key) {
                return Ok(Some(draft));
            }
            let result = handler(draft.clone())?;
            if result.kind.key() != key {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    format!("{name} changed the event kind").into(),
                )));
            }
            offered.steps.lock().unwrap().push(TraceStep {
                handler: name.clone(),
                result: (result != draft).then(|| result.clone()),
            });
            Ok(Some(result))
        };
        Ok(ctx
            .listen::<BeforeCommit>()
            .name(self.before_name())
            .on_sync(middleware)?)
    }
    /// The branch's private channel: the bus is flat, so the name carries the
    /// branch instance. Only the Timeline builds it.
    fn before_name(&self) -> String {
        format!("dim/timeline/{}/before", self.0.instance)
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
