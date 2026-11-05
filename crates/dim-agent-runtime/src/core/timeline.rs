use super::keys::KeyFilter;
use crate::{
    AgentError, BoxFuture, FaultReporter, Result, StoredEvent, TimelineKey, TimelineQuery,
    TimelineQueryDirection, TimelineQueryOrderBy, TimelineQueryPage, TimelineQueryReply,
    TimelineQueryScope, TimelineStore,
};
use dim_agent_types::{
    AgentEvent, EventDraft, EventKind, InputMessageData, ProjectionCheckpoint, RecoverySnapshot,
    ResumePoint, TimelinePosition,
};
use rutis::events::SyncCall;
#[allow(unused_imports)]
use rutis::host::{CtxHost, ErrorKind, InstanceId};
use rutis::service;
use rutis::{Ctx, Disposer, Event};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
use uuid::Uuid;

/// Inputs one page of the restored submission index keeps.
const SUBMISSION_PAGE: u64 = 256;

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
/// The committed input one submission id names.
#[derive(Debug, Clone)]
pub(crate) struct SubmissionInput {
    pub id: String,
    pub sequence: u64,
}
/// The inputs a branch holds for one submission id: one, by its event id and
/// sequence, or several, written before the branch accepted each id once.
#[derive(Debug, Clone)]
enum Submission {
    Input(SubmissionInput),
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
            .or_insert_with(|| {
                Submission::Input(SubmissionInput {
                    id: event.id.clone(),
                    sequence: event.sequence,
                })
            });
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
            && let Some(input) = self.submission(id)?
        {
            return Ok(Appended::Existing(input.sequence));
        }
        admit()?;
        let mut events = self.commit(vec![EventKind::InputMessage(data).into()], |_| Ok(()))?;
        Ok(Appended::Written(
            events.pop().expect("single-event batch returned one event"),
        ))
    }
    /// The committed input with submission id `id`: its event id and sequence.
    pub(crate) fn submission(&self, id: &str) -> Result<Option<SubmissionInput>> {
        let restored = self.0.restored.get().ok_or_else(|| {
            AgentError::fault("timeline.input", "restored submissions are not indexed")
        })?;
        let committed = self.0.submissions.lock().unwrap();
        match (restored.get(id), committed.get(id)) {
            (None, None) => Ok(None),
            (Some(Submission::Input(input)), None) | (None, Some(Submission::Input(input))) => {
                Ok(Some(input.clone()))
            }
            _ => Err(AgentError::AmbiguousSubmission(id.into())),
        }
    }
    /// Indexes the submission ids of the inputs the branch was restored
    /// with. The branch's build calls it before the branch accepts input, so
    /// the published head is the restored head and nothing of this branch is
    /// written and not yet published.
    pub(crate) async fn index_submissions(&self) -> Result<()> {
        if self.0.restored.get().is_some() {
            return Ok(());
        }
        let mut restored = HashMap::new();
        let mut from = 1;
        loop {
            let reply = self
                .query(TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    include_publishing: false,
                    from_event: None,
                    from_sequence: Some(from),
                    order_by: TimelineQueryOrderBy::Sequence,
                    direction: TimelineQueryDirection::Ascending,
                    kinds: vec!["input/message".into()],
                    flags: Vec::new(),
                    event_id: None,
                    predicate: None,
                    predicates: Vec::new(),
                    text: None,
                    limit: NonZeroU64::new(SUBMISSION_PAGE)
                        .expect("a page keeps at least one input"),
                    offset: 0,
                })
                .await?;
            let short = (reply.events.len() as u64) < SUBMISSION_PAGE;
            let last = reply.events.last().map(|event| event.sequence);
            for event in &reply.events {
                index_submission(&mut restored, event);
            }
            match last {
                Some(last) if !short => from = last + 1,
                _ => break,
            }
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
        through.map_or_else(|| Ok(self.committed()), |through| self.bound(through))
    }
    /// The last sequence a read may take: `through`, which may not pass the
    /// sequence this branch has assigned.
    #[service(skip)]
    pub fn bound(&self, through: u64) -> Result<u64> {
        if through > self.committed() {
            return Err(AgentError::InvalidInput(format!(
                "future timeline sequence {through}"
            )));
        }
        Ok(through)
    }
    /// The event a context checkpoint is anchored at. It is one fact, so it is
    /// read as one event and never as a range that reaches the cut: this
    /// branch's hot window while it holds the event, otherwise a one-event
    /// query of the store. A point read is not a range, so a row inside an
    /// atomic batch is read like any other.
    async fn anchor(&self, branch: &str, sequence: u64) -> Result<Option<Arc<AgentEvent>>> {
        if branch == self.0.key.branch_id {
            let hot = self.0.data.lock().unwrap();
            if let Some(record) = hot
                .hot
                .iter()
                .find(|record| record.event.sequence == sequence)
            {
                return Ok(Some(record.event.clone()));
            }
        }
        let key = TimelineKey {
            session_id: self.0.key.session_id.clone(),
            branch_id: branch.into(),
        };
        let point = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from_event: None,
            from_sequence: None,
            order_by: TimelineQueryOrderBy::Sequence,
            direction: TimelineQueryDirection::Ascending,
            include_publishing: false,
            kinds: Vec::new(),
            flags: Vec::new(),
            event_id: None,
            predicate: None,
            predicates: Vec::new(),
            text: None,
            limit: NonZeroU64::new(1).expect("one is nonzero"),
            offset: 0,
        };
        Ok(self
            .0
            .store
            .query(&key, &point, sequence, sequence)
            .await?
            .events
            .into_iter()
            .next())
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
    /// this branch to the published head, or to the committed head when the
    /// query asks for it, in which case this branch's own facts in
    /// `published+1..=through` are part of the same page.
    ///
    /// The reply's `through` is the cutoff the read took on this branch: the
    /// published head, or the committed head the query asked for, which is
    /// read when the call starts, so a write during the call stays out of the
    /// page. The published part and the tail are disjoint in sequence, so each
    /// fact is returned once. A caller that holds live events releases the ones
    /// above the cutoff ([ADR 020 D3](../../../../docs/adr/020-timeline-read-convergence.md)).
    pub async fn query(&self, query: TimelineQuery) -> Result<TimelineQueryReply> {
        let (published, through, tail) = {
            let data = self.0.data.lock().unwrap();
            let through = if query.include_publishing {
                data.allocated
            } else {
                data.published
            };
            let tail = (through > data.published).then(|| {
                data.hot
                    .iter()
                    .filter(|record| {
                        record.event.sequence > data.published && record.event.sequence <= through
                    })
                    .map(|record| record.event.clone())
                    .collect::<Vec<_>>()
            });
            (data.published, through, tail)
        };
        self.query_page(query, published, through, tail).await
    }
    /// [`Self::query`] with this branch clipped at `published` and, when
    /// `through` names a committed sequence above it, the branch's own facts in
    /// `published+1..=through` merged in. The query's start, selection and page
    /// apply to the merged result: the tail continues the published page when
    /// ascending and precedes it when descending, and a page shorter than the
    /// limit means the selection is exhausted up to the tail's end.
    async fn query_page(
        &self,
        query: TimelineQuery,
        published: u64,
        through: u64,
        tail: Option<Vec<Arc<AgentEvent>>>,
    ) -> Result<TimelineQueryReply> {
        use TimelineQueryDirection::Descending;

        query.validate()?;
        let tail = tail.map(|events| (events, through));
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
                        include_publishing: false,
                        kinds: Vec::new(),
                        flags: Vec::new(),
                        event_id: Some(event_id.clone()),
                        predicate: None,
                        predicates: Vec::new(),
                        text: None,
                        limit: std::num::NonZeroU64::new(1).expect("one is nonzero"),
                        offset: 0,
                    };
                    let page = self.0.store.query(&key, &probe, 1, *clip).await?;
                    if let Some(position) = page.positions.first() {
                        found = Some((Some(branch.clone()), position.sequence));
                        break;
                    }
                }
                if found.is_none() {
                    found = tail.as_ref().and_then(|(events, _)| {
                        events
                            .iter()
                            .find(|event| &event.id == event_id)
                            .map(|event| (Some(self.0.key.branch_id.clone()), event.sequence))
                    });
                }
                found
            }
            (Some(_), Some(_)) => unreachable!("validate rejects two anchors"),
        };
        if query.from_event.is_some() && anchor.is_none() {
            return Ok(TimelineQueryReply {
                events: Vec::new(),
                positions: Vec::new(),
                through,
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
            let tail_bounds = match (&tail, *branch == self.0.key.branch_id) {
                (Some((_, through)), true) => Some(match query.direction {
                    TimelineQueryDirection::Ascending => (from, *through),
                    Descending => (
                        1,
                        match (&anchor, anchor_index) {
                            (Some((Some(_), sequence)), Some(start_index))
                                if index == start_index =>
                            {
                                *sequence
                            }
                            (Some((None, sequence)), _) => *sequence,
                            _ => *through,
                        }
                        .min(*through),
                    ),
                }),
                _ => None,
            };
            let parts: [bool; 2] = match query.direction {
                TimelineQueryDirection::Ascending => [false, true],
                Descending => [true, false],
            };
            for is_tail in parts {
                if events.len() >= limit {
                    break;
                }
                let page = if is_tail {
                    let (Some((tail_events, _)), Some((from, bound))) = (&tail, tail_bounds) else {
                        continue;
                    };
                    self.tail_page(tail_events, &query, from, bound, limit - events.len(), skip)
                } else {
                    let mut request = query.clone();
                    request.scope = TimelineQueryScope::Branch;
                    request.from_event = None;
                    request.from_sequence = None;
                    request.limit = std::num::NonZeroU64::new(
                        u64::try_from(limit - events.len())
                            .unwrap_or(u64::MAX)
                            .max(1),
                    )
                    .expect("a read keeps one event of its limit");
                    request.offset = skip;
                    self.0.store.query(&key, &request, from, bound).await?
                };
                let reached = page.skipped + page.events.len() as u64;
                skip = skip.saturating_sub(reached);
                events.extend(page.events);
                positions.extend(page.positions);
            }
        }
        Ok(TimelineQueryReply {
            events,
            positions,
            through,
        })
    }
    /// The part of one page the tail holds: the facts `query` accepts in
    /// `from..=bound`, in the query's direction, with the read's offset
    /// applied.
    fn tail_page(
        &self,
        tail: &[Arc<AgentEvent>],
        query: &TimelineQuery,
        from: u64,
        bound: u64,
        limit: usize,
        offset: u64,
    ) -> TimelineQueryPage {
        let mut events: Vec<Arc<AgentEvent>> = tail
            .iter()
            .filter(|event| (from..=bound).contains(&event.sequence) && query.accepts(event))
            .cloned()
            .collect();
        if query.direction == TimelineQueryDirection::Descending {
            events.reverse();
        }
        let skipped = events
            .len()
            .min(usize::try_from(offset).unwrap_or(usize::MAX));
        let events: Vec<Arc<AgentEvent>> = events.into_iter().skip(skipped).take(limit).collect();
        TimelineQueryPage {
            positions: events
                .iter()
                .map(|event| TimelinePosition {
                    branch_id: self.0.key.branch_id.clone(),
                    sequence: event.sequence,
                })
                .collect(),
            events,
            skipped: skipped as u64,
        }
    }
    /// Checks that `sequence` ends an atomic batch: a stored row's structure,
    /// not event content, so the store answers it and it is not a query
    /// (ADR 020 D4). The check stays out of the service surface because no
    /// other language reads batch structure.
    #[service(skip)]
    pub async fn batch_end(&self, sequence: u64) -> Result<()> {
        self.0.store.batch_end(&self.0.key, sequence).await
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

    /// The latest interpreter-compatible checkpoint at `through`, if one is
    /// valid for this branch and its fixed ancestor chain. The resume it names
    /// is read with `from_event` and returned at the position that event
    /// occupies; the recorded position is used when no event carries the id.
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
            let Some(anchor) = self.anchor(branch, snapshot.sequence).await? else {
                continue;
            };
            if anchor.id != snapshot.event_id || anchor.sequence != snapshot.sequence {
                continue;
            }
            let Some(mut checkpoint) = decode(&snapshot, &anchor) else {
                continue;
            };
            if let Some(position) = self.resume_position(&checkpoint.resume).await? {
                checkpoint.resume.branch_id = position.branch_id;
                checkpoint.resume.sequence = position.sequence;
            }
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
    /// The position of the event a checkpoint resumes at, read with
    /// `from_event`, which names the branch the event sits in. A resume event
    /// the store holds no id for (an event inside a merged output row) keeps
    /// the position the checkpoint recorded.
    #[service(skip)]
    async fn resume_position(&self, resume: &ResumePoint) -> Result<Option<TimelinePosition>> {
        let query = TimelineQuery {
            scope: TimelineQueryScope::Chain,
            include_publishing: false,
            from_event: Some(resume.event_id.clone()),
            from_sequence: None,
            order_by: TimelineQueryOrderBy::Sequence,
            direction: TimelineQueryDirection::Ascending,
            kinds: Vec::new(),
            flags: Vec::new(),
            event_id: None,
            predicate: None,
            predicates: Vec::new(),
            text: None,
            limit: std::num::NonZeroU64::new(1).expect("one is nonzero"),
            offset: 0,
        };
        Ok(self.query(query).await?.positions.into_iter().next())
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
