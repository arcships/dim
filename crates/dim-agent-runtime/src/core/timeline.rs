use super::keys::KeyFilter;
use crate::{
    AgentError, BoxFuture, FaultReporter, QueryComparison, QueryFilter, QueryOperand, QueryOutput,
    QueryPage, QuerySelect, QuerySpec, QueryValue, Result, StoredEvent, TimelineKey, TimelineQuery,
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
    collections::{HashSet, VecDeque},
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
use uuid::Uuid;

tokio::task_local! {
    /// The child tool call whose task is producing events. `Timeline::append`
    /// stamps an event without an explicit parent from this scope.
    static PARENT_CALL_ID: String;
}

/// Runs `future` with `parent_call_id` as the implicit parent of events it
/// appends. A child batch or result that names its own parent keeps it.
pub async fn with_parent_call_id<F>(parent_call_id: String, future: F) -> F::Output
where
    F: std::future::Future,
{
    PARENT_CALL_ID.scope(parent_call_id, future).await
}

/// The child call whose task is running, for a background task that must
/// carry the same parent after it leaves the current task.
pub fn current_parent_call_id() -> Option<String> {
    PARENT_CALL_ID.try_with(Clone::clone).ok()
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
    data: Mutex<Data>,
    commit: Mutex<()>,
    changed: watch::Sender<(u64, bool)>,
    faults: FaultReporter,
}
/// The committed input one submission id names.
#[derive(Debug, Clone)]
pub(crate) struct SubmissionInput {
    pub id: String,
    pub sequence: u64,
}
/// What [`Timeline::input`] did.
pub(crate) enum Appended {
    Written(Arc<AgentEvent>),
    /// An input with the same submission id is already committed at this
    /// sequence; nothing was written.
    Existing(u64),
}
pub(crate) enum AppendedBatch {
    Written(Vec<Arc<AgentEvent>>),
    Existing(Vec<u64>),
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
    /// Appends one event stamped with the producer's `create_at`: the fact it
    /// records began before the producer could commit it, as a thinking block
    /// does with its first delta.
    #[service(skip)]
    pub fn append_with_create_at(
        &self,
        original: EventDraft,
        create_at: u64,
    ) -> std::result::Result<Arc<AgentEvent>, AgentError> {
        let mut events =
            self.append_batch_registered_with_create_at(vec![original], Some(create_at), |_| {
                Ok(())
            })?;
        Ok(events.pop().expect("single-event batch returned one event"))
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
        self.append_batch_registered_with_create_at(originals, None, register)
    }
    fn append_batch_registered_with_create_at(
        &self,
        mut originals: Vec<EventDraft>,
        create_at: Option<u64>,
        register: impl FnOnce(&[Arc<AgentEvent>]) -> Result<()>,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        if let Ok(parent) = PARENT_CALL_ID.try_with(Clone::clone) {
            for draft in &mut originals {
                if draft.parent_call_id.is_none() {
                    draft.parent_call_id = Some(parent.clone());
                }
            }
        }
        let _append = AppendGuard::enter()?;
        let _commit = self.0.commit.lock().unwrap();
        self.commit(originals, create_at, register)
    }
    /// Appends an input once per submission id, including accepted inputs
    /// awaiting publication. Concurrent attempts return the same input.
    pub(crate) async fn input_checked(
        &self,
        data: InputMessageData,
        admit: impl FnOnce() -> Result<()>,
    ) -> Result<Appended> {
        loop {
            let through = self.committed();
            let found = match &data.submission_id {
                Some(id) => self.submission(id, through).await?,
                None => None,
            };
            let _append = AppendGuard::enter()?;
            let _commit = self.0.commit.lock().unwrap();
            if data.submission_id.is_some() && self.committed() != through {
                continue;
            }
            if let Some(input) = found {
                return Ok(Appended::Existing(input.sequence));
            }
            admit()?;
            let mut events =
                self.commit(vec![EventKind::InputMessage(data).into()], None, |_| Ok(()))?;
            return Ok(Appended::Written(
                events.pop().expect("single-event batch returned one event"),
            ));
        }
    }
    /// Commits all inputs atomically, or returns the existing inputs in the same order.
    /// Every submission id must be present and distinct. A partially committed batch fails.
    pub(crate) async fn inputs_checked(
        &self,
        inputs: Vec<InputMessageData>,
        admit: impl FnOnce() -> Result<()>,
    ) -> Result<AppendedBatch> {
        let mut ids = HashSet::with_capacity(inputs.len());
        for input in &inputs {
            let Some(id) = &input.submission_id else {
                return Err(AgentError::InvalidInput(
                    "every input in a batch must carry a submission id".into(),
                ));
            };
            if !ids.insert(id) {
                return Err(AgentError::InvalidInput(format!(
                    "submission id {id} appears more than once in one batch"
                )));
            }
        }
        loop {
            let through = self.committed();
            let mut found = Vec::with_capacity(inputs.len());
            let mut pending = Vec::new();
            for input in &inputs {
                let id = input
                    .submission_id
                    .as_deref()
                    .expect("validated submission id");
                match self.submission(id, through).await? {
                    Some(input) => found.push(input.sequence),
                    None => pending.push(id),
                }
            }
            let _append = AppendGuard::enter()?;
            let _commit = self.0.commit.lock().unwrap();
            if self.committed() != through {
                continue;
            }
            if pending.is_empty() {
                return Ok(AppendedBatch::Existing(found));
            }
            if !found.is_empty() {
                return Err(AgentError::PartialSubmission(pending.join(", ")));
            }
            admit()?;
            let events = self.commit(
                inputs
                    .into_iter()
                    .map(|input| EventKind::InputMessage(input).into())
                    .collect(),
                None,
                |_| Ok(()),
            )?;
            return Ok(AppendedBatch::Written(events));
        }
    }
    /// The committed input at `through` with submission id `id`.
    /// More than one matching historical input is ambiguous.
    pub(crate) async fn submission(
        &self,
        id: &str,
        through: u64,
    ) -> Result<Option<SubmissionInput>> {
        let reply = self
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: Some(through),
                include_publishing: true,
                query: QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::Compare {
                                left: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                comparison: QueryComparison::Eq,
                                right: QueryOperand::Value {
                                    value: "input/message".into(),
                                },
                            },
                            QueryFilter::Compare {
                                left: QueryOperand::Field {
                                    path: "data.submission_id".into(),
                                },
                                comparison: QueryComparison::Eq,
                                right: QueryOperand::Value { value: id.into() },
                            },
                        ],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["id".into(), "position.sequence".into()],
                    },
                    group_by: Vec::new(),
                    pick: None,
                    order_by: Vec::new(),
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::new(2).expect("two inputs distinguish ambiguity"),
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Rows { mut rows } = reply.result else {
            unreachable!("submission lookup selects input identities")
        };
        if rows.len() > 1 {
            return Err(AgentError::AmbiguousSubmission(id.into()));
        }
        let Some(row) = rows.pop() else {
            return Ok(None);
        };
        let Ok(
            [
                QueryValue::Value {
                    value: serde_json::Value::String(id),
                },
                QueryValue::Value {
                    value: serde_json::Value::Number(sequence),
                },
            ],
        ) = <[_; 2]>::try_from(row.values)
        else {
            return Err(AgentError::fault(
                "timeline.input",
                "a submission has no event identity or position",
            ));
        };
        Ok(Some(SubmissionInput {
            id,
            sequence: sequence
                .as_u64()
                .expect("event sequences are unsigned integers"),
        }))
    }
    /// Commits a batch; the caller holds the append guard and commit lock.
    fn commit(
        &self,
        originals: Vec<EventDraft>,
        create_at: Option<u64>,
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
                create_at: create_at.unwrap_or_else(unix_millis),
                tags: draft.tags,
                parent_call_id: draft.parent_call_id,
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
        let scope = crate::TimelineReadScope {
            key,
            through: sequence,
            segments: vec![crate::TimelineReadSegment {
                branch_id: branch.into(),
                from: sequence,
                through: sequence,
                stored_through: sequence,
                tail: vec![],
            }],
        };
        let result = self
            .0
            .store
            .query(
                &scope,
                &QuerySpec {
                    filter: None,
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                Some(&QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            )
            .await?;
        let QueryOutput::Events { events } = result.result else {
            unreachable!("event selection returns events")
        };
        Ok(events.into_iter().next().map(|row| row.event))
    }

    /// Computes properties over one fixed branch or ancestry scope.
    /// The optional start must name a position inside that scope.
    pub async fn query(&self, query: crate::TimelineQuery) -> Result<crate::TimelineQueryReply> {
        query.query.validate(query.page.as_ref())?;
        if query.through.is_none() && query.page.as_ref().is_some_and(|page| page.after.is_some()) {
            return Err(AgentError::InvalidInput(
                "continued queries require the original through".into(),
            ));
        }
        let mut scope = {
            let data = self.0.data.lock().unwrap();
            let visible = if query.include_publishing {
                data.allocated
            } else {
                data.published
            };
            let through = query.through.unwrap_or(visible);
            if through > visible {
                return Err(AgentError::InvalidInput(format!(
                    "future timeline sequence {through}"
                )));
            }
            let mut segments = Vec::new();
            if query.scope == TimelineQueryScope::Chain {
                segments.extend(self.0.ancestors.iter().filter(|(_, cut)| *cut > 0).map(
                    |(branch, cut)| crate::TimelineReadSegment {
                        branch_id: branch.clone(),
                        from: 1,
                        through: *cut,
                        stored_through: *cut,
                        tail: Vec::new(),
                    },
                ));
            }
            segments.push(crate::TimelineReadSegment {
                branch_id: self.0.key.branch_id.clone(),
                from: 1,
                through,
                stored_through: data.published.min(through),
                tail: data
                    .hot
                    .iter()
                    .filter(|record| {
                        record.event.sequence > data.published && record.event.sequence <= through
                    })
                    .map(|record| record.event.clone())
                    .collect(),
            });
            crate::TimelineReadScope {
                key: self.0.key.clone(),
                through,
                segments,
            }
        };
        if let Some(from) = query.from {
            let index = scope
                .segments
                .iter()
                .position(|segment| segment.branch_id == from.branch_id)
                .ok_or_else(|| {
                    AgentError::InvalidInput("query start is outside its branch scope".into())
                })?;
            if from.sequence == 0 || from.sequence > scope.segments[index].through {
                return Err(AgentError::InvalidInput(
                    "query start is outside its sequence range".into(),
                ));
            }
            scope.segments.drain(..index);
            let first = &mut scope.segments[0];
            first.from = from.sequence;
            first.tail.retain(|event| event.sequence >= from.sequence);
        }
        let result = self
            .0
            .store
            .query(&scope, &query.query, query.page.as_ref())
            .await?;
        Ok(crate::TimelineQueryReply {
            through: scope.through,
            result: result.result,
            next: result.next,
        })
    }
    /// Checks that `sequence` ends an atomic batch: a stored row's structure,
    /// not event content, so the store answers it and it is not a query
    /// (ADR 020 D4). The check stays out of the service surface because no
    /// other language reads batch structure.
    #[service(skip)]
    pub async fn batch_end(&self, sequence: u64) -> Result<()> {
        self.0.store.batch_end(&self.0.key, sequence).await
    }
    /// Returns the containing batch's end, including the branch's committed tail.
    #[service(skip)]
    pub async fn batch_end_at(&self, sequence: u64) -> Result<u64> {
        self.bound(sequence)?;
        if sequence == 0 {
            return Ok(0);
        }
        {
            let data = self.0.data.lock().unwrap();
            if let Some(record) = data
                .hot
                .iter()
                .find(|record| record.event.sequence == sequence)
            {
                return Ok(record.batch_end);
            }
        }
        self.0.store.batch_end_at(&self.0.key, sequence).await
    }
    #[service(skip)]
    /// The branch chain the context covers, root first: each ancestor to its
    /// fixed cut and the local branch to `through`.
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
    /// The stored position of the checkpoint's resume event, if it still exists.
    #[service(skip)]
    async fn resume_position(&self, resume: &ResumePoint) -> Result<Option<TimelinePosition>> {
        let reply = self
            .query(TimelineQuery {
                scope: TimelineQueryScope::Chain,
                include_publishing: false,
                from: None,
                through: None,
                query: QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field { path: "id".into() },
                        values: vec![resume.event_id.clone().into()],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["position".into()],
                    },
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("field selection returns rows")
        };
        Ok(rows.into_iter().next().map(|row| {
            let [QueryValue::Value { value }] = row.values.as_slice() else {
                unreachable!("event has a position")
            };
            serde_json::from_value(value.clone()).expect("position follows the query contract")
        }))
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
