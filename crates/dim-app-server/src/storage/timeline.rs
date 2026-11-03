use super::{
    SqliteStorage,
    codec::{
        self, CONTENT_BUDGET, ContentRow, ListReferences, Previous, Reader, RequestBodies, Values,
        WireEvent,
    },
    files::{self, SessionFiles},
    metadata::{self, Titles},
    sqlite::error,
};
use crate::protocol::SessionOrigin;
use dim_agent_capabilities::host::SessionFilesHost;
use dim_agent_runtime::Names;
use dim_agent_runtime::{
    AgentError, BoxFuture, BranchRecord, CompactionCut, PolicyEvent, PolicyFacts, PolicyOrder,
    PolicyOrderEvent, PolicyWindow, PublishCallback, Result, SessionConfig, StoredBranch,
    StoredEvent, TimelineKey, TimelineStore, TimelineTrace, panic_message,
};
use dim_agent_types::{
    AgentEvent, BranchCreateAt, ContextCompactionResult, EventKind, ModelRequest, RecoverySnapshot,
    SessionForkedData, TimelinePosition,
    grant::{GrantKind, GrantMode, SessionGrant},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, host_key};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    ops::RangeInclusive,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, Weak, mpsc},
    thread,
    time::{Duration, Instant},
};

/// Rounds of output deltas alone wait until this long after the previous
/// round started (timeline storage §3).
const ROUND_WINDOW: Duration = Duration::from_millis(20);

/// Starts one `read_tool_use_starts` statement looks up. Each start is one
/// term of a `UNION ALL`, and SQLite's default `SQLITE_MAX_COMPOUND_SELECT`
/// is 500 terms, so a batch is read in statements of half that.
const STARTS_PER_STATEMENT: usize = 250;
struct Pending {
    key: TimelineKey,
    events: Vec<StoredEvent>,
    publish: Option<PublishCallback>,
    #[cfg(test)]
    fault: Option<fn(&str)>,
}
impl Pending {
    fn publish(&mut self, result: Result<()>) {
        if let Some(publish) = self.publish.take()
            && let Err(panic) = catch_unwind(AssertUnwindSafe(|| publish(result)))
        {
            tracing::error!(session_id = %self.key.session_id, branch_id = %self.key.branch_id, panic = %panic_message(panic), "Timeline publication callback panicked");
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.publish.is_some() {
            self.publish(Err(error(
                "Timeline writer stopped before publishing the batch",
            )));
        }
    }
}
enum Command {
    Append(Pending),
    #[cfg(test)]
    Rollback {
        session_id: String,
        done: tokio::sync::oneshot::Sender<Result<()>>,
        #[cfg(test)]
        fault: Option<fn()>,
    },
    Release {
        session_id: String,
        done: tokio::sync::oneshot::Sender<Result<()>>,
    },
    Close,
    #[cfg(test)]
    Panic,
}

/// One process writer batches every Session; only pending events reside here.
pub struct SqliteTimelineStore {
    pub(super) db: Arc<SqliteStorage>,
    /// Bytes of decoded request content each read or validation pass keeps.
    content_budget: usize,
    /// Schedules a Session-file sweep after a commit released owner rows.
    sweep_files: Arc<tokio::sync::Notify>,
    admission: Mutex<Admission>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    closing: Mutex<Option<tokio::sync::watch::Receiver<Option<Result<()>>>>>,
    /// The writer memo (§6.2 of the request storage design), `None` when a
    /// flow test disabled it. Only the writer thread changes it.
    memo: Option<Mutex<Memo>>,
    /// Per Session, the branch directory a successful validation pass checked
    /// or a closing load last left. A restore reuses the pass while the stored
    /// directory equals this record ([restore validation reuse](../../../../docs/design/appserver/restore-validation.md)).
    validated: Mutex<BTreeMap<String, ValidatedDirectory>>,
    #[cfg(feature = "testing")]
    node_loads: Mutex<codec::NodeLoads>,
    #[cfg(feature = "testing")]
    round_deadlines: Mutex<Vec<Option<Instant>>>,
}
struct Admission {
    sender: mpsc::Sender<Command>,
    failures: BTreeMap<String, AgentError>,
    closed: bool,
}
/// Per Session and branch, the last request row the writer committed: its
/// sequence, its request while the runtime still holds it, and its list.
type Memo = BTreeMap<String, BTreeMap<String, MemoEntry>>;
struct MemoEntry {
    sequence: u64,
    request: Weak<ModelRequest>,
    refs: ListReferences,
}
/// What the writer prepared for one Session's batches of a round before
/// its write transaction (§6.1 of the request storage design).
struct Prepared {
    /// Per batch and record, the lists of its event request and request
    /// drafts, in `codec::drafts` order; empty for other records.
    lists: Vec<Vec<Vec<Option<ListReferences>>>>,
    /// The nodes and changed `tools` and `system` the lists add.
    content: Vec<ContentRow>,
    /// Per branch, the last request of the round, for the memo.
    last: BTreeMap<String, (u64, Arc<ModelRequest>, ListReferences)>,
}
/// The requests a Session's preparation has encoded so far in a round.
#[derive(Default)]
struct Round {
    /// Per branch, its request records, in order.
    requests: BTreeMap<String, Vec<Previous>>,
    /// Branches created in the round: parent and cut.
    created: BTreeMap<String, (String, u64)>,
    /// The parent of each node the round adds, which no later record of the
    /// round can load from the store.
    fresh: HashMap<String, Option<String>>,
}
impl SqliteTimelineStore {
    pub fn new(db: Arc<SqliteStorage>) -> Arc<Self> {
        Self::start(db, CONTENT_BUDGET, true, ROUND_WINDOW)
    }
    /// A store whose reads keep at most `bytes` of decoded request content;
    /// flow tests make it small to force evictions. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn with_content_budget(db: Arc<SqliteStorage>, bytes: usize) -> Arc<Self> {
        Self::start(db, bytes, true, ROUND_WINDOW)
    }
    /// A store whose writer has no memo, so that every request write reads
    /// its previous request from the store. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn without_writer_memo(db: Arc<SqliteStorage>) -> Arc<Self> {
        Self::start(db, CONTENT_BUDGET, false, ROUND_WINDOW)
    }
    /// A store whose rounds of output deltas wait until `window` after the
    /// previous round started; a flow test makes it longer than its publish
    /// timeout, so that a wait the writer should end or not begin fails the
    /// test. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn with_round_window(db: Arc<SqliteStorage>, window: Duration) -> Arc<Self> {
        Self::start(db, CONTENT_BUDGET, true, window)
    }
    /// For each write round since the last call, the instant until which it
    /// was to wait for more batches: the window after the previous round
    /// started when it began with output deltas, `None` when it started at
    /// once. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn take_round_deadlines(&self) -> Vec<Option<Instant>> {
        std::mem::take(&mut *self.round_deadlines.lock().unwrap())
    }
    /// The node rows reads and the writer loaded since the last call, how
    /// they were spread over connection holds, and the nodes the writer
    /// encoded. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn take_node_loads(&self) -> codec::NodeLoads {
        std::mem::take(&mut *self.node_loads.lock().unwrap())
    }
    /// The requests of Session `session` the writer memo can still reach.
    /// Feature `testing` only.
    #[cfg(feature = "testing")]
    pub fn writer_memo_requests(&self, session: &str) -> usize {
        self.memo.as_ref().map_or(0, |memo| {
            memo.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(session)
                .map_or(0, |branches| {
                    branches
                        .values()
                        .filter(|entry| entry.request.strong_count() > 0)
                        .count()
                })
        })
    }
    fn start(
        db: Arc<SqliteStorage>,
        content_budget: usize,
        memo: bool,
        window: Duration,
    ) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel();
        let store = Arc::new(Self {
            db,
            content_budget,
            sweep_files: Arc::new(tokio::sync::Notify::new()),
            admission: Mutex::new(Admission {
                sender,
                failures: BTreeMap::new(),
                closed: false,
            }),
            worker: Mutex::new(None),
            closing: Mutex::new(None),
            memo: memo.then(Mutex::default),
            validated: Mutex::default(),
            #[cfg(feature = "testing")]
            node_loads: Mutex::default(),
            #[cfg(feature = "testing")]
            round_deadlines: Mutex::default(),
        });
        let weak = Arc::downgrade(&store);
        let writer = thread::spawn(move || {
            let mut last_round: Option<Instant> = None;
            let mut control = None;
            loop {
                let command = match control.take().or_else(|| receiver.recv().ok()) {
                    Some(command) => command,
                    None => break,
                };
                let first = match command {
                    Command::Append(first) => first,
                    #[cfg(test)]
                    Command::Rollback {
                        session_id,
                        done,
                        #[cfg(test)]
                        fault,
                    } => {
                        let Some(store) = weak.upgrade() else { break };
                        let result = writer_operation(|| {
                            store.rollback_creation(
                                &session_id,
                                #[cfg(test)]
                                fault,
                            )
                        });
                        let _ = done.send(result);
                        continue;
                    }
                    Command::Release { session_id, done } => {
                        let Some(store) = weak.upgrade() else { break };
                        store.forget(&session_id);
                        store
                            .admission
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .failures
                            .remove(&session_id);
                        store.refresh_validated(&session_id);
                        let _ = done.send(Ok(()));
                        continue;
                    }
                    Command::Close => break,
                    #[cfg(test)]
                    Command::Panic => panic!("injected unexpected Timeline writer exit"),
                };
                let outputs = |batch: &Pending| {
                    batch
                        .events
                        .iter()
                        .all(|record| output_row(record.event.kind.key()))
                };
                let mut deadline = last_round
                    .filter(|_| outputs(&first))
                    .map(|started| started + window);
                #[cfg(feature = "testing")]
                let waited = deadline;
                let mut pending = vec![first];
                loop {
                    let wait = deadline.map_or(Duration::ZERO, |deadline| {
                        deadline.saturating_duration_since(Instant::now())
                    });
                    match receiver.recv_timeout(wait) {
                        Ok(Command::Append(batch)) => {
                            deadline = deadline.filter(|_| outputs(&batch));
                            pending.push(batch);
                        }
                        Ok(command) => {
                            control = Some(command);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                last_round = Some(Instant::now());
                let Some(store) = weak.upgrade() else { break };
                #[cfg(feature = "testing")]
                store.round_deadlines.lock().unwrap().push(waited);
                store.commit_round(pending);
            }
        });
        *store.worker.lock().unwrap() = Some(writer);
        store
    }
    /// A reader for one read call or validation pass.
    fn reader(&self, request_bodies: RequestBodies) -> Reader {
        match request_bodies {
            RequestBodies::Rebuild => Reader::new(self.content_budget),
            RequestBodies::Unused => Reader::without_request_bodies(self.content_budget),
        }
    }
    /// One page of complete batches, with `request_bodies` deciding whether the
    /// records it returns carry their rebuilt requests (§7.5 of the request
    /// storage design).
    async fn page(
        &self,
        key: &TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
        request_bodies: RequestBodies,
    ) -> Result<Vec<StoredEvent>> {
        if after > through {
            return Err(AgentError::InvalidInput("future Timeline cursor".into()));
        }
        let session = key.session_id.clone();
        self.segments(
            &session,
            vec![(key.branch_id.clone(), through..=through)],
            request_bodies,
        )
        .await?;
        if after > 0 && after != through {
            self.segments(
                &session,
                vec![(key.branch_id.clone(), after..=after)],
                request_bodies,
            )
            .await?;
        }
        let mut end = through.min(after.saturating_add(limit as u64));
        if end <= after {
            return Ok(vec![]);
        }
        let mut next = after + 1;
        let mut events = vec![];
        let mut reader = self.reader(request_bodies);
        while next <= end {
            let last = end.min(next.saturating_add(255));
            let page = read_range(&self.db, &mut reader, key, next..=last, false).await?;
            if page.len() as u64 != last - next + 1 {
                return Err(error("Timeline read contains a sequence gap"));
            }
            if last == end {
                end = page.last().unwrap().batch_end;
                if end < last || end > through {
                    return Err(error("invalid Timeline batch boundary"));
                }
            }
            events.extend(page);
            next = last + 1;
        }
        self.finish(reader);
        Ok(events)
    }
    /// The records of `segments`, decoded with one reader of `request_bodies`.
    async fn segments(
        &self,
        session_id: &str,
        segments: Vec<(String, RangeInclusive<u64>)>,
        request_bodies: RequestBodies,
    ) -> Result<Vec<Vec<StoredEvent>>> {
        let session = session_id.to_owned();
        let mut result = Vec::with_capacity(segments.len());
        let mut reader = self.reader(request_bodies);
        for (branch_id, range) in segments {
            let key = TimelineKey {
                session_id: session.clone(),
                branch_id,
            };
            let through = self.head(&key).await?;
            if *range.end() > through {
                return Err(AgentError::InvalidInput("future Timeline sequence".into()));
            }
            let mut records = vec![];
            let mut next = *range.start();
            while next <= *range.end() {
                let end = next.saturating_add(255).min(*range.end());
                let page = read_range(&self.db, &mut reader, &key, next..=end, false).await?;
                let expected = if end >= next.max(1) {
                    end - next.max(1) + 1
                } else {
                    0
                };
                if page.len() as u64 != expected {
                    return Err(error("Timeline read contains a sequence gap"));
                }
                records.extend(page);
                next = end + 1;
            }
            if records
                .last()
                .is_some_and(|record| record.batch_end > *range.end())
            {
                return Err(AgentError::InvalidInput(format!(
                    "sequence {} is inside an atomic batch",
                    range.end()
                )));
            }
            result.push(records);
        }
        self.finish(reader);
        Ok(result)
    }
    /// The records of `key` in `range`, decoded with one reader of
    /// `request_bodies`.
    async fn read_records(
        &self,
        key: &TimelineKey,
        range: RangeInclusive<u64>,
        request_bodies: RequestBodies,
    ) -> Result<Vec<StoredEvent>> {
        let mut segments = self
            .segments(
                &key.session_id,
                vec![(key.branch_id.clone(), range)],
                request_bodies,
            )
            .await?;
        Ok(segments.pop().expect("one range returns one segment"))
    }
    /// Ends `reader`'s read call; feature `testing` counts its node loads.
    fn finish(&self, reader: Reader) {
        #[cfg(feature = "testing")]
        {
            let mut reader = reader;
            self.node_loads.lock().unwrap().add(reader.loads());
        }
        #[cfg(not(feature = "testing"))]
        drop(reader);
    }
    fn commit_round(&self, pending: Vec<Pending>) {
        let mut sessions: BTreeMap<String, Vec<Pending>> = BTreeMap::new();
        for mut batch in pending {
            let failure = self
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .failures
                .get(&batch.key.session_id)
                .cloned();
            if let Some(failure) = failure {
                batch.publish(Err(failure));
            } else {
                sessions
                    .entry(batch.key.session_id.clone())
                    .or_default()
                    .push(batch);
            }
        }
        let mut prepared = BTreeMap::new();
        for (session, batches) in sessions {
            match writer_operation(|| self.prepare(&session, &batches)) {
                Ok(encoding) => {
                    prepared.insert(session, (batches, encoding));
                }
                Err(failure) => {
                    tracing::error!(db_path = %self.db.path.display(), adapter = "rust", phase = "prepare", pid = std::process::id(), %failure, "storage.transaction");
                    self.fail(session, batches, failure);
                }
            }
        }
        if !prepared.is_empty() {
            self.commit_prepared(prepared);
        }
        if let Some(memo) = &self.memo {
            memo.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|_, branches| {
                    branches.retain(|_, entry| entry.request.strong_count() > 0);
                    !branches.is_empty()
                });
        }
        self.db.checkpoint();
    }
    fn commit_prepared(&self, prepared: BTreeMap<String, (Vec<Pending>, Prepared)>) {
        let mut guard = self
            .db
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = guard
            .as_mut()
            .ok_or_else(|| error("SQLite storage is closed"))
            .and_then(|connection| {
                transaction(
                    connection,
                    &self.db.path,
                    prepared
                        .values()
                        .map(|(batches, encoding)| (batches.as_slice(), encoding)),
                )
            });
        if result.is_ok() {
            drop(guard);
            for (session, (batches, encoding)) in prepared {
                self.remember(&session, encoding);
                for mut batch in batches {
                    batch.publish(Ok(()));
                }
            }
            return;
        }
        for (session, (batches, encoding)) in prepared {
            let existing = self
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .failures
                .get(&session)
                .cloned();
            let result = match existing {
                Some(failure) => Err(failure),
                None => guard
                    .as_mut()
                    .ok_or_else(|| error("SQLite storage is closed"))
                    .and_then(|connection| {
                        transaction(
                            connection,
                            &self.db.path,
                            std::iter::once((batches.as_slice(), &encoding)),
                        )
                    }),
            };
            drop(guard);
            match &result {
                Ok(()) => self.remember(&session, encoding),
                Err(failure) => {
                    self.forget(&session);
                    self.admission
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failures
                        .insert(session, failure.clone());
                }
            }
            for mut batch in batches {
                batch.publish(result.clone());
            }
            guard = self
                .db
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    /// Fails `batches` of `session` without writing them.
    fn fail(&self, session: String, batches: Vec<Pending>, failure: AgentError) {
        self.forget(&session);
        self.admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failures
            .insert(session, failure.clone());
        for mut batch in batches {
            batch.publish(Err(failure.clone()));
        }
    }
    /// Records the last request of each branch a commit wrote.
    fn remember(&self, session: &str, encoding: Prepared) {
        let Some(memo) = &self.memo else { return };
        let mut memo = memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let branches = memo.entry(session.to_owned()).or_default();
        for (branch, (sequence, request, refs)) in encoding.last {
            branches.insert(
                branch,
                MemoEntry {
                    sequence,
                    request: Arc::downgrade(&request),
                    refs,
                },
            );
        }
    }
    fn forget(&self, session: &str) {
        if let Some(memo) = &self.memo {
            memo.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(session);
        }
    }
    /// Drops the validation record of `session`, so its next restore validates.
    fn forget_validated(&self, session: &str) {
        self.validated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session);
    }
    /// Refreshes the validation record of `session` to the directory the writer
    /// just settled. Only a Session whose stored history this process validated
    /// has a record; a failed read drops it, and the next restore validates.
    fn refresh_validated(&self, session: &str) {
        if !self
            .validated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(session)
        {
            return;
        }
        let current = {
            let guard = self
                .db
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .as_ref()
                .ok_or_else(|| error("SQLite storage is closed"))
                .and_then(|connection| directory_rows(connection, session))
        };
        let mut validated = self
            .validated
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match current {
            Ok(directory) => {
                validated.insert(session.to_owned(), directory);
            }
            Err(_) => {
                validated.remove(session);
            }
        }
    }
    /// Runs `step` with `reader` in connection holds of at most
    /// `NODE_LOADS_PER_HOLD` node loads until it returns a value; the
    /// writer's form of `holds`.
    fn hold<T>(
        &self,
        reader: &mut Reader,
        mut step: impl FnMut(&Connection, &mut Reader) -> Result<Option<T>>,
    ) -> Result<T> {
        loop {
            let guard = self
                .db
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let connection = guard
                .as_ref()
                .ok_or_else(|| error("SQLite storage is closed"))?;
            reader.begin_hold();
            if let Some(value) = step(connection, reader)? {
                return Ok(value);
            }
        }
    }
    /// Prepares the request records of `session`'s batches, in order: each
    /// request's list against its previous request (§6.1 of the request
    /// storage design), and each request draft's against its record's
    /// request.
    fn prepare(&self, session: &str, batches: &[Pending]) -> Result<Prepared> {
        let mut reader = self.reader(RequestBodies::Rebuild);
        let mut round = Round::default();
        let mut prepared = Prepared {
            lists: Vec::with_capacity(batches.len()),
            content: vec![],
            last: BTreeMap::new(),
        };
        let result = (|| {
            for batch in batches {
                #[cfg(test)]
                if let Some(fault) = batch.fault {
                    fault("prepare");
                }
                let branch = &batch.key.branch_id;
                let mut lists = Vec::with_capacity(batch.events.len());
                for record in &batch.events {
                    let sequence = record.event.sequence;
                    if let EventKind::BranchCreate(data) = &record.event.kind {
                        let cut = match data.at {
                            BranchCreateAt::Sequence { sequence } => sequence,
                            BranchCreateAt::Create {} => sequence,
                        };
                        round
                            .created
                            .insert(data.branch.clone(), (branch.clone(), cut));
                    }
                    let Some(request) = codec::request(&record.event.kind) else {
                        lists.push(vec![]);
                        continue;
                    };
                    let previous = self.previous(
                        session,
                        branch,
                        sequence.saturating_sub(1),
                        &round,
                        &mut reader,
                    )?;
                    let refs = self.encode_list(
                        session,
                        request,
                        previous.as_ref(),
                        &mut round,
                        &mut prepared.content,
                        &mut reader,
                    )?;
                    let event = Previous {
                        sequence,
                        values: Values::Written(request.clone()),
                        refs: refs.clone(),
                    };
                    let mut record_lists = vec![Some(refs.clone())];
                    for draft in codec::drafts(&record.trace) {
                        let list = codec::request(&draft.kind)
                            .map(|draft| {
                                self.encode_list(
                                    session,
                                    draft,
                                    Some(&event),
                                    &mut round,
                                    &mut prepared.content,
                                    &mut reader,
                                )
                            })
                            .transpose()?;
                        record_lists.push(list);
                    }
                    lists.push(record_lists);
                    prepared
                        .last
                        .insert(branch.clone(), (sequence, request.clone(), refs));
                    round
                        .requests
                        .entry(branch.clone())
                        .or_default()
                        .push(event);
                }
                prepared.lists.push(lists);
            }
            Ok(())
        })();
        #[cfg(feature = "testing")]
        {
            self.node_loads.lock().unwrap().encoded += prepared
                .content
                .iter()
                .filter(|row| row.kind == "node")
                .count();
        }
        self.finish(reader);
        result.map(|()| prepared)
    }
    /// `request`'s list against `previous`: the common prefix keeps
    /// `previous`'s nodes, and the node before the first new message is
    /// found by stepping back from `previous`'s last node when the prefix
    /// is shorter than `previous`.
    fn encode_list(
        &self,
        session: &str,
        request: &ModelRequest,
        previous: Option<&Previous>,
        round: &mut Round,
        content: &mut Vec<ContentRow>,
        reader: &mut Reader,
    ) -> Result<ListReferences> {
        let keep = previous.map_or(0, |previous| codec::prefix(previous, request));
        let parent = match previous {
            Some(previous) if keep > 0 => {
                let mut address = previous
                    .refs
                    .list
                    .clone()
                    .ok_or_else(|| error("request list length mismatch"))?;
                let mut steps = previous.refs.length - keep;
                while steps > 0
                    && let Some(parent) = round.fresh.get(&address)
                {
                    address = parent
                        .clone()
                        .ok_or_else(|| error("request list length mismatch"))?;
                    steps -= 1;
                }
                if steps > 0 {
                    self.hold(reader, |connection, reader| {
                        while steps > 0 {
                            match reader.parent(connection, session, &address)? {
                                Some(parent) => address = parent,
                                None => return Ok(None),
                            }
                            steps -= 1;
                        }
                        Ok(Some(()))
                    })?;
                }
                Some(address)
            }
            _ => None,
        };
        codec::encode_list(request, keep, parent, previous, content, &mut round.fresh)
    }
    /// The previous request of a request on `branch` (§6.1 of the request
    /// storage design): the last request through `through` on `branch`,
    /// else through the cut on each ancestor in turn. Earlier records of
    /// the round come first, then the memo, then the store, whose request
    /// is rebuilt from its list in holds of node loads. A v16 row has no
    /// list to share, so it gives no previous request.
    fn previous(
        &self,
        session: &str,
        branch: &str,
        through: u64,
        round: &Round,
        reader: &mut Reader,
    ) -> Result<Option<Previous>> {
        let (mut branch, mut through) = (branch.to_owned(), through);
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(branch.clone()) {
                return Err(error("branch directory cycle"));
            }
            if let Some(previous) = round.requests.get(&branch).and_then(|requests| {
                requests
                    .iter()
                    .rev()
                    .find(|previous| previous.sequence <= through)
            }) {
                return Ok(Some(previous.clone()));
            }
            if let Some(memo) = &self.memo
                && let Some(entry) = memo
                    .lock()
                    .unwrap()
                    .get(session)
                    .and_then(|branches| branches.get(&branch))
                && entry.sequence <= through
                && let Some(request) = entry.request.upgrade()
            {
                return Ok(Some(Previous {
                    sequence: entry.sequence,
                    values: Values::Written(request),
                    refs: entry.refs.clone(),
                }));
            }
            let (found, parent) = self.hold(reader, |connection, _| {
                let found: Option<(u64, String)> = connection
                    .prepare_cached("SELECT sequence, body FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 AND sequence <= ?3 AND kind IN ('context/prepare', 'model/call') ORDER BY sequence DESC LIMIT 1")
                    .map_err(error)?
                    .query_row(params![session, branch, sql_sequence(through)?], |row| Ok((read_sequence(row, 0)?, row.get(1)?)))
                    .optional()
                    .map_err(error)?;
                if found.is_some() {
                    return Ok(Some((found, None)));
                }
                let parent = match round.created.get(&branch) {
                    Some(parent) => Some(parent.clone()),
                    None => {
                        let info: String = connection
                            .query_row(
                                "SELECT info FROM timeline_branches WHERE session_id = ?1 AND branch_id = ?2",
                                params![session, branch],
                                |row| row.get(0),
                            )
                            .map_err(error)?;
                        let info: BranchRecord = serde_json::from_str(&info).map_err(error)?;
                        match (info.parent, info.branch_point) {
                            (Some(parent), Some(cut)) => Some((parent, cut)),
                            (None, None) => None,
                            _ => return Err(error("invalid branch directory")),
                        }
                    }
                };
                Ok(Some((None, parent)))
            })?;
            if let Some((sequence, body)) = found {
                let Row::Event { record } = serde_json::from_str(&body).map_err(error)? else {
                    return Err(error("request row is an output row"));
                };
                let Some(refs) = record.list()?.cloned() else {
                    return Ok(None);
                };
                return self
                    .hold(reader, |connection, reader| {
                        reader.previous(connection, session, sequence, &refs)
                    })
                    .map(Some);
            }
            match parent {
                Some((parent, cut)) => (branch, through) = (parent, cut),
                None => return Ok(None),
            }
        }
    }
    #[cfg(test)]
    fn rollback_creation(&self, session_id: &str, fault: Option<fn()>) -> Result<()> {
        self.forget(session_id);
        let mut guard = self
            .db
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let connection = guard
            .as_mut()
            .ok_or_else(|| error("SQLite storage is closed"))?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let released = files::release(&tx, session_id).map_err(error)?;
        for table in [
            "timeline_snapshots",
            "timeline_events",
            "timeline_branches",
            "timeline_content",
            "session_grants",
            "session_output_owners",
            "session_metadata",
            "session_catalog",
            "session_owners",
            "agent_sessions",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE session_id=?1"),
                [session_id],
            )
            .map_err(error)?;
        }
        #[cfg(test)]
        if let Some(fault) = fault {
            fault();
        }
        tx.commit().map_err(error)?;
        drop(guard);
        self.db.checkpoint();
        if released > 0 {
            self.sweep_files.notify_one();
        }
        self.forget_validated(session_id);
        self.admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failures
            .remove(session_id);
        Ok(())
    }
    pub async fn close(self: &Arc<Self>) -> Result<()> {
        let mut receiver = {
            let mut closing = self.closing.lock().unwrap();
            closing
                .get_or_insert_with(|| {
                    let (sender, receiver) = tokio::sync::watch::channel(None);
                    let mut admission = self
                        .admission
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    admission.closed = true;
                    let requested = admission.sender.send(Command::Close).map_err(error);
                    let worker = self.worker.lock().unwrap().take();
                    let owned = self.clone();
                    tokio::spawn(async move {
                        let result = match worker {
                            Some(worker) => tokio::task::spawn_blocking(move || {
                                worker.join().map_err(|_| error("Timeline writer panicked"))
                            })
                            .await
                            .map_err(error)
                            .and_then(|result| result)
                            .and(requested),
                            None => requested,
                        };
                        sender.send_replace(Some(result));
                        drop(owned);
                    });
                    receiver
                })
                .clone()
        };
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver.changed().await.map_err(error)?;
        }
    }
}

/// The newest turn at a watermark and the first event a window over it must
/// include: the turn start, or an input the turn consumed when that is older
/// (session-history-window §4.2).
#[derive(Debug, Clone, Copy)]
pub struct TurnWindow {
    /// `None` when the branch has no `turn/start` at or below the watermark.
    pub turn_start: Option<u64>,
    /// The window's first sequence, always a batch start. `1` without a turn.
    pub window_start: u64,
}

/// Session directory service; Agent hosting owns the profile and owner meaning.
#[derive(Clone)]
pub struct SessionStore {
    pub(super) timeline: Arc<SqliteTimelineStore>,
    /// The file steps on the Session's resource node: Session files and the
    /// command output a delete removes.
    files: SessionFiles,
    /// Title metadata (S5b): its one writer.
    pub(super) titles: Titles,
    /// Serializes output ownership across Sessions: a delete's shared-owner
    /// check, removal and row deletion, and a fork's copy of the output rows,
    /// so a fork either copies before the removal or sees no rows (ADR 013 D4).
    ownership: Arc<tokio::sync::Mutex<()>>,
}
impl SessionStore {
    /// `ctx` is SessionStore's own context, from which each file step reads
    /// the node's FileStorage; `files_root` is the FileStorage root of the
    /// database directory.
    pub fn new(db: Arc<SqliteStorage>, ctx: Ctx, files_root: usize) -> Self {
        let timeline = SqliteTimelineStore::new(db.clone());
        let titles = Titles::new(db.clone(), ctx.clone());
        let files = SessionFiles::new(db, ctx, files_root, timeline.sweep_files.clone());
        Self {
            timeline,
            files,
            titles,
            ownership: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn timeline_store(&self) -> Arc<dyn TimelineStore> {
        self.timeline.clone()
    }
    /// Adds a canonical user grant for `session_id`. The caller resolves
    /// `path`; a write grant is not downgraded by a later read grant, and a
    /// read grant may become write.
    pub async fn add_grant(
        &self,
        session_id: &str,
        path: PathBuf,
        kind: GrantKind,
        mode: GrantMode,
    ) -> Result<()> {
        let id = session_id.to_owned();
        let path = path.to_string_lossy().into_owned();
        let kind = match kind {
            GrantKind::File => "file",
            GrantKind::Dir => "dir",
        };
        let mode = match mode {
            GrantMode::Read => "read",
            GrantMode::Write => "write",
        };
        self.timeline
            .db
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO session_grants (session_id, path, kind, mode) VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT(session_id, path) DO UPDATE SET kind=excluded.kind, \
                         mode=CASE WHEN session_grants.mode='write' THEN 'write' ELSE excluded.mode END",
                        params![id, path, kind, mode],
                    )
                    .map_err(error)?;
                Ok(())
            })
            .await
    }
    /// Removes the grant at the canonical `path`; the caller resolves it the
    /// same way `add_grant` does.
    pub async fn remove_grant(&self, session_id: &str, path: PathBuf) -> Result<()> {
        let id = session_id.to_owned();
        let path = path.to_string_lossy().into_owned();
        self.timeline
            .db
            .run(move |connection| {
                connection
                    .execute(
                        "DELETE FROM session_grants WHERE session_id=?1 AND path=?2",
                        params![id, path],
                    )
                    .map_err(error)?;
                Ok(())
            })
            .await
    }
    pub async fn grants(&self, session_id: &str) -> Result<Vec<SessionGrant>> {
        let id = session_id.to_owned();
        self.timeline
            .db
            .run_read(move |connection| {
                let mut query = connection
                    .prepare("SELECT path, kind, mode FROM session_grants WHERE session_id=?1 ORDER BY path")
                    .map_err(error)?;
                let rows = query
                    .query_map([id], |row| {
                        let path: String = row.get(0)?;
                        let kind: String = row.get(1)?;
                        let mode: String = row.get(2)?;
                        Ok((path, kind, mode))
                    })
                    .map_err(error)?;
                let mut grants = Vec::new();
                for row in rows {
                    let (path, kind, mode) = row.map_err(error)?;
                    grants.push(SessionGrant {
                        path: PathBuf::from(path),
                        kind: if kind == "file" { GrantKind::File } else { GrantKind::Dir },
                        mode: if mode == "write" { GrantMode::Write } else { GrantMode::Read },
                    });
                }
                Ok(grants)
            })
            .await
    }
    /// ADR 013 D2: records `path` as output owned by `session_id` before the
    /// tool result hands it to the model. A repeated path is a no-op; a
    /// missing Session fails.
    pub async fn register(&self, session_id: &str, path: PathBuf) -> Result<()> {
        self.files
            .register(session_id.to_owned(), path)
            .await
            .map_err(error)
    }
    /// ADR 013 D3: whether `session_id` owns the resolved `path` through
    /// either ownership table.
    pub async fn owns(&self, session_id: &str, path: PathBuf) -> Result<bool> {
        self.files
            .owns(session_id.to_owned(), path)
            .await
            .map_err(error)
    }
    /// ADR 013 D3: the Session file directories and output paths the Session
    /// owns.
    pub async fn owned_paths(&self, session_id: &str) -> Result<Vec<PathBuf>> {
        self.files
            .owned_paths(session_id.to_owned())
            .await
            .map_err(error)
    }
    /// One Session's stored creation origin and leader link, for an attach
    /// reply (#557): one row of `agent_sessions`, no config decode, no history
    /// and no lease.
    pub async fn creation_record(
        &self,
        session_id: &str,
    ) -> Result<(SessionOrigin, Option<String>)> {
        let id = session_id.to_owned();
        self.timeline
            .db
            .run_read(move |connection| {
                let (text, parent): (String, Option<String>) = connection
                    .query_row(
                        "SELECT origin, leader_session_id FROM agent_sessions WHERE session_id=?1",
                        [&id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(error)?
                    .ok_or_else(|| AgentError::Closed(id))?;
                Ok((origin_from_stored(&text)?, parent))
            })
            .await
    }
    pub async fn read_page(
        &self,
        id: &str,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
        self.main_page(id, after, limit, through, RequestBodies::Rebuild)
            .await
    }
    /// A page for readers that discard `context/prepare` and `model/call`
    /// records or use only their other fields: their requests are not
    /// rebuilt.
    pub async fn read_page_without_request_bodies(
        &self,
        id: &str,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
        self.main_page(id, after, limit, through, RequestBodies::Unused)
            .await
    }
    async fn main_page(
        &self,
        id: &str,
        after: u64,
        limit: usize,
        through: u64,
        request_bodies: RequestBodies,
    ) -> Result<Vec<Arc<dim_agent_types::AgentEvent>>> {
        let key = TimelineKey {
            session_id: id.into(),
            branch_id: "main".into(),
        };
        Ok(self
            .timeline
            .page(&key, after, limit, through, request_bodies)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }
    /// The newest turn at `through`, and the first event a window over it must
    /// include: the turn start, or the messages the turn's first request
    /// consumed when they are older.
    pub async fn turn_window(&self, id: &str, branch: &str, through: u64) -> Result<TurnWindow> {
        let session = id.to_owned();
        let branch = branch.to_owned();
        self.timeline
            .db
            .run_read(move |connection| {
                let turn: Option<i64> = connection
                    .prepare(
                        "SELECT max(sequence) FROM timeline_events \
                         WHERE session_id=?1 AND branch_id=?2 AND kind='turn/start' \
                         AND sequence<=?3",
                    )
                    .map_err(error)?
                    .query_row(params![&session, &branch, sql_sequence(through)?], |row| {
                        row.get(0)
                    })
                    .map_err(error)?;
                let Some(turn) = turn else {
                    return Ok(TurnWindow {
                        turn_start: None,
                        window_start: 1,
                    });
                };
                let turn_start = sequence_value(turn)?;
                let first_step: Option<i64> = connection
                    .prepare(
                        "SELECT min(sequence) FROM timeline_events \
                         WHERE session_id=?1 AND branch_id=?2 AND kind='step/model-start' \
                         AND sequence>?3 AND sequence<=?4",
                    )
                    .map_err(error)?
                    .query_row(
                        params![
                            &session,
                            &branch,
                            sql_sequence(turn_start)?,
                            sql_sequence(through)?
                        ],
                        |row| row.get(0),
                    )
                    .map_err(error)?;
                let mut window_start = turn_start;
                let mut consumed = false;
                if let Some(first_step) = first_step {
                    let inputs: Vec<String> = connection
                        .prepare(
                            "SELECT json_extract(body, '$.record.event.data.input_id') \
                             FROM timeline_events \
                             WHERE session_id=?1 AND branch_id=?2 AND kind='input/consumed' \
                             AND sequence>?3 AND sequence<?4 ORDER BY sequence",
                        )
                        .map_err(error)?
                        .query_map(
                            params![&session, &branch, sql_sequence(turn_start)?, first_step],
                            |row| row.get(0),
                        )
                        .map_err(error)?
                        .collect::<rusqlite::Result<_>>()
                        .map_err(error)?;
                    for input in inputs {
                        let sequence: Option<i64> = connection
                            .prepare(
                                "SELECT sequence FROM timeline_events \
                                 WHERE session_id=?1 AND event_id=?2",
                            )
                            .map_err(error)?
                            .query_row(params![&session, input], |row| row.get(0))
                            .optional()
                            .map_err(error)?;
                        if let Some(sequence) = sequence {
                            window_start = window_start.min(sequence_value(sequence)?);
                            consumed = true;
                        }
                    }
                }
                if !consumed {
                    let newest: Option<i64> = connection
                        .prepare(
                            "SELECT max(sequence) FROM timeline_events \
                             WHERE session_id=?1 AND branch_id=?2 AND kind='input/message' \
                             AND sequence<=?3",
                        )
                        .map_err(error)?
                        .query_row(
                            params![&session, &branch, sql_sequence(turn_start)?],
                            |row| row.get(0),
                        )
                        .map_err(error)?;
                    if let Some(newest) = newest {
                        window_start = window_start.min(sequence_value(newest)?);
                    }
                }
                Ok(TurnWindow {
                    turn_start: Some(turn_start),
                    window_start,
                })
            })
            .await
    }

    /// The newest events below `before` inside `window_start..=through`, whole
    /// batches at both ends (session-history-window §4.2). A `before` inside a
    /// batch clamps down to the batch end; a `through` inside one fails like
    /// the forward read. Every event carries its batch end, so the caller can
    /// trim by bytes without splitting one.
    pub async fn read_page_before(
        &self,
        id: &str,
        branch: &str,
        before: u64,
        limit: usize,
        through: u64,
        window_start: u64,
    ) -> Result<Vec<StoredEvent>> {
        let key = TimelineKey {
            session_id: id.into(),
            branch_id: branch.into(),
        };
        if through > 0 {
            self.timeline
                .read_records(&key, through..=through, RequestBodies::Unused)
                .await?;
        }
        let top = before.saturating_sub(1).min(through);
        if top == 0 || top < window_start || limit == 0 {
            return Ok(vec![]);
        }
        let Some(at_top) = self.read_any(&key, top..=top).await?.pop() else {
            return Ok(vec![]);
        };
        let end = if at_top.batch_end <= top {
            at_top.batch_end
        } else {
            self.previous_batch_end(&key, at_top.event.sequence, at_top.batch_end, window_start)
                .await?
        };
        if end < window_start {
            return Ok(vec![]);
        }
        let mut low = end
            .saturating_sub(limit.saturating_sub(1) as u64)
            .max(1)
            .max(window_start);
        loop {
            let page = self
                .timeline
                .read_records(&key, low..=end, RequestBodies::Unused)
                .await?;
            let Some(first) = page.first() else {
                return Ok(vec![]);
            };
            if low <= window_start || low == 1 {
                return Ok(page);
            }
            let start = self
                .batch_start(&key, first.event.sequence, first.batch_end, window_start)
                .await?;
            if start >= low {
                return Ok(page);
            }
            low = start;
        }
    }
    /// Reads a sequence range that may start and end inside a batch; the
    /// caller checks the boundaries itself.
    async fn read_any(
        &self,
        key: &TimelineKey,
        range: RangeInclusive<u64>,
    ) -> Result<Vec<StoredEvent>> {
        let mut reader = self.timeline.reader(RequestBodies::Unused);
        let records = read_range(&self.timeline.db, &mut reader, key, range, false).await;
        self.timeline.finish(reader);
        records
    }
    /// The end of the batch before the one holding `sequence..=batch_end`,
    /// searching no lower than `floor`.
    async fn previous_batch_end(
        &self,
        key: &TimelineKey,
        sequence: u64,
        batch_end: u64,
        floor: u64,
    ) -> Result<u64> {
        let mut probe = sequence.saturating_sub(1);
        loop {
            if probe == 0 {
                return Ok(0);
            }
            let low = probe.saturating_sub(63).max(1).max(floor);
            let chunk = self.read_any(key, low..=probe).await?;
            if let Some(previous) = chunk
                .iter()
                .rev()
                .find(|record| record.batch_end != batch_end)
            {
                return Ok(previous.event.sequence);
            }
            if low <= floor || low == 1 {
                return Ok(low.saturating_sub(1));
            }
            probe = low - 1;
        }
    }
    /// The first sequence of the batch holding `sequence..=batch_end`,
    /// searching no lower than `floor` (a batch start).
    async fn batch_start(
        &self,
        key: &TimelineKey,
        sequence: u64,
        batch_end: u64,
        floor: u64,
    ) -> Result<u64> {
        let mut probe = sequence.saturating_sub(1);
        loop {
            if probe == 0 || probe < floor {
                return Ok(floor);
            }
            let low = probe.saturating_sub(63).max(1).max(floor);
            let chunk = self.read_any(key, low..=probe).await?;
            if let Some(previous) = chunk
                .iter()
                .rev()
                .find(|record| record.batch_end != batch_end)
            {
                return Ok(previous.event.sequence + 1);
            }
            if low <= floor || low == 1 {
                return Ok(low);
            }
            probe = low - 1;
        }
    }
    /// The stored events of `event_ids` in one statement; ids the store
    /// does not hold are absent.
    pub async fn read_events(
        &self,
        id: &str,
        event_ids: &[String],
    ) -> Result<HashMap<String, Arc<dim_agent_types::AgentEvent>>> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let session = id.to_owned();
        let wanted = event_ids.to_vec();
        let rows = self
            .read_rows(id, move |connection| {
                let columns = (0..wanted.len())
                    .map(|at| format!("?{}", at + 2))
                    .collect::<Vec<_>>()
                    .join(",");
                let mut query = connection
                    .prepare(&format!(
                        "SELECT sequence, last_sequence, kind, body, event_id FROM timeline_events \
                     WHERE session_id=?1 AND event_id IN ({columns})"
                    ))
                    .map_err(error)?;
                let mut parameters: Vec<&dyn rusqlite::ToSql> = vec![&session];
                parameters.extend(wanted.iter().map(|id| id as &dyn rusqlite::ToSql));
                query_fetched(&mut query, rusqlite::params_from_iter(parameters))
            })
            .await?;
        let mut events = HashMap::with_capacity(rows.len());
        for row in rows {
            let record = indexed(row)?;
            events.insert(record.event.id.clone(), record.event);
        }
        Ok(events)
    }
    /// The main-branch `step/tool-use-start`s of `keys`; calls the store does
    /// not hold are absent. The keys are read in bounded statement chunks.
    pub async fn read_tool_use_starts(
        &self,
        id: &str,
        keys: &[(String, String)],
    ) -> Result<HashMap<(String, String), Arc<dim_agent_types::AgentEvent>>> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let session = id.to_owned();
        let wanted = keys.to_vec();
        let rows = self
            .read_rows(id, move |connection| {
                let mut rows = Vec::with_capacity(wanted.len());
                for chunk in wanted.chunks(STARTS_PER_STATEMENT) {
                    let selects = (0..chunk.len())
                        .map(|at| {
                            format!(
                                "SELECT sequence, last_sequence, kind, body, event_id FROM \
                             timeline_events WHERE session_id=?1 AND branch_id='main' AND \
                             kind='step/tool-use-start' AND \
                             json_extract(body, '$.record.event.data.request_id')=?{} AND \
                             json_extract(body, '$.record.event.data.call_id')=?{}",
                                at * 2 + 2,
                                at * 2 + 3
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" UNION ALL ");
                    let mut query = connection.prepare(&selects).map_err(error)?;
                    let mut parameters: Vec<&dyn rusqlite::ToSql> = vec![&session];
                    for (request_id, call_id) in chunk {
                        parameters.push(request_id);
                        parameters.push(call_id);
                    }
                    rows.extend(query_fetched(
                        &mut query,
                        rusqlite::params_from_iter(parameters),
                    )?);
                }
                Ok(rows)
            })
            .await?;
        let mut starts = HashMap::with_capacity(rows.len());
        for row in rows {
            let record = indexed(row)?;
            if let EventKind::ToolUseStart(start) = &record.event.kind {
                starts.insert(
                    (start.request_id.clone(), start.call_id.clone()),
                    record.event,
                );
            }
        }
        Ok(starts)
    }
    /// The rows `select` picks, decoded with one content reader; `select`
    /// runs its one statement in the first connection hold.
    async fn read_rows(
        &self,
        session: &str,
        select: impl Fn(&Connection) -> Result<Vec<Fetched>> + Send + Sync + 'static,
    ) -> Result<Vec<Fetched>> {
        let session = session.to_owned();
        let mut reader = self.timeline.reader(RequestBodies::Rebuild);
        let rows = holds(
            &self.timeline.db,
            &mut reader,
            None,
            move |connection, reader, rows: &mut Option<Rows>| {
                let rows = match rows {
                    Some(rows) => rows,
                    None => rows.insert(Rows {
                        fetched: select(connection)?.into(),
                        decoded: vec![],
                    }),
                };
                rows.advance(connection, &session, reader)
            },
        )
        .await?;
        self.timeline.finish(reader);
        Ok(rows.expect("a finished read fetched its rows").decoded)
    }
    pub async fn has_records(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.timeline
            .db
            .run_read(move |connection| {
                connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id=?1) \
                 OR EXISTS(SELECT 1 FROM session_owners WHERE session_id=?1) \
                 OR EXISTS(SELECT 1 FROM timeline_branches WHERE session_id=?1) \
                 OR EXISTS(SELECT 1 FROM timeline_events WHERE session_id=?1) \
                 OR EXISTS(SELECT 1 FROM timeline_content WHERE session_id=?1)",
                        [id],
                        |row| row.get(0),
                    )
                    .map_err(error)
            })
            .await
    }
    /// The storage steps of `session.delete` ([S5c §3.2](../../../../docs/design/appserver/session-deletion-and-fork.md#32-sequence)),
    /// once Agents has closed the Session: its output directories go first,
    /// durably (ADR 013 D4), then the rows that make it visible in one
    /// transaction and its Timeline rows in bounded ones, the operation a
    /// creation rollback runs, then the Session-file sweep is scheduled for
    /// the files it released. Nothing of the Session is read, so a damaged one
    /// goes too, and an unknown id has nothing to remove. A failure before the
    /// first commit leaves every row. The
    /// output removal's last-owner guard is scoped to this database: a
    /// directory another database's Session uses belongs to that database.
    /// `id` is one path component, as Agents checked (D3). Returns whether
    /// this database had rows of the id.
    pub async fn delete_session(&self, id: &str) -> Result<bool> {
        let _ownership = self.ownership.lock().await;
        let stored = self.has_records(id).await?;
        if stored {
            self.files.remove_output(id).await?;
        }
        self.timeline.rollback_session(id).await?;
        self.timeline.sweep_files.notify_one();
        Ok(stored)
    }
    /// The prepare step of `session.fork`; the ownership lock keeps this copy
    /// and a delete's output removal apart. [`SqliteTimelineStore::fork_session`]
    /// copies the rows.
    pub async fn fork_session(&self, source: &str, fork: &str, at: u64) -> Result<()> {
        let _ownership = self.ownership.lock().await;
        self.timeline.fork_session(source, fork, at).await
    }
    pub async fn create_owned_session(
        &self,
        session_id: &str,
        config: &SessionConfig,
        agent_id: &str,
        protocol_version: u32,
        leader: Option<&str>,
        origin: SessionOrigin,
    ) -> Result<()> {
        let session = session_id.to_owned();
        let config = config.clone();
        let agent = agent_id.to_owned();
        let leader = leader.map(str::to_owned);
        self.timeline
            .db
            .run(move |connection| {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(error)?;
                create_session(&tx, &session, &config, leader.as_deref(), origin)?;
                tx.execute(
                    "INSERT INTO session_owners VALUES (?1, ?2, ?3)",
                    params![session, agent, protocol_version],
                )
                .map_err(error)?;
                tx.commit().map_err(error)
            })
            .await
    }
}

/// The Session directory, the `sessionFiles` service and the
/// `sessionMetadata` service. Session files live below the FileStorage root
/// `files_root`, which is the database directory.
pub struct SessionStorePlugin {
    files_root: usize,
    db: Key<SqliteStorage>,
}
impl SessionStorePlugin {
    pub fn new(names: &Names<'_>, files_root: usize) -> rutis::Result<Self> {
        Ok(Self {
            files_root,
            db: names.key()?,
        })
    }
}
impl DynPlugin for SessionStorePlugin {
    fn name(&self) -> &str {
        "SessionStore"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.db.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let db = ctx.require_at(&self.db)?;
            let store = Arc::new(SessionStore::new(db, ctx.clone(), self.files_root));
            let files = store.files.clone();
            let (timeline, owned_files, titles) =
                (store.timeline.clone(), files.clone(), store.titles.clone());
            ctx.effect(move || async move {
                titles.close().await;
                owned_files.close().await;
                timeline.close().await.map_err(Into::into)
            })?;
            files.start();
            ctx.provide_as::<SessionStore>(store.clone())?;
            ctx.provide_as::<SessionFilesHost>(Arc::new(SessionFilesHost(Arc::new(files))))?;
            ctx.provide_at(
                host_key("sessionMetadata"),
                Arc::new(metadata::Dispatch(store)) as Arc<dyn HostDispatch>,
            )?;
            Ok(Effect::Done)
        })
    }
}

fn create_session(
    connection: &Connection,
    session: &str,
    config: &SessionConfig,
    leader: Option<&str>,
    origin: SessionOrigin,
) -> Result<()> {
    let config = serde_json::to_string(config).map_err(error)?;
    connection
        .execute(
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id, origin) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session,
                config,
                sql_sequence(dim_agent_runtime::core::unix_millis())?,
                leader,
                origin.as_str()
            ],
        )
        .map_err(error)?;
    let info = BranchRecord {
        id: "main".into(),
        parent: None,
        branch_point: None,
        create_anchor: None,
    };
    insert_branch(connection, session, &info)?;
    super::catalog::create(connection, session)
}
/// The creation origin a stored value names. The column's `CHECK` and the
/// format gate keep any other text out; reaching this error means the store
/// was written around them, so it is reported rather than mapped to a value.
fn origin_from_stored(text: &str) -> Result<SessionOrigin> {
    SessionOrigin::from_stored(text)
        .ok_or_else(|| error(format!("Session origin {text:?} is not a stored origin")))
}
pub(super) fn stored_origin(row: &rusqlite::Row<'_>, index: usize) -> Result<SessionOrigin> {
    origin_from_stored(&row.get::<_, String>(index).map_err(error)?)
}
fn insert_branch(connection: &Connection, session: &str, info: &BranchRecord) -> Result<()> {
    connection
        .prepare_cached("INSERT INTO timeline_branches VALUES (?1, ?2, ?3, 0)")
        .map_err(error)?
        .execute(params![
            session,
            info.id,
            serde_json::to_string(info).map_err(error)?
        ])
        .map_err(error)?;
    Ok(())
}
fn writer_operation<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or_else(|panic| {
        Err(error(format!(
            "Timeline writer operation panicked: {}",
            panic_message(panic)
        )))
    })
}
/// Writes each Session's prepared content rows, then its batches.
fn transaction<'a>(
    connection: &mut Connection,
    path: &std::path::Path,
    mut sessions: impl Iterator<Item = (&'a [Pending], &'a Prepared)>,
) -> Result<()> {
    writer_operation(|| {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|failure| {
        tracing::error!(db_path = %path.display(), adapter = "rust", phase = "BEGIN", pid = std::process::id(), sqlite_code = ?failure.sqlite_error_code(), %failure, "storage.transaction");
        error(failure)
    })?;
        let written = sessions.try_for_each(|(batches, prepared)| {
            let mut insert = tx
                .prepare_cached("INSERT OR IGNORE INTO timeline_content VALUES (?1, ?2, ?3, ?4)")
                .map_err(error)?;
            let session = &batches[0].key.session_id;
            for row in &prepared.content {
                insert
                    .execute(params![session, row.hash, row.kind, row.body])
                    .map_err(error)?;
            }
            let mut outputs = BTreeMap::new();
            batches
                .iter()
                .zip(&prepared.lists)
                .try_for_each(|(batch, lists)| write_batch(&tx, batch, lists, &mut outputs))
        });
        match written {
        Ok(()) => tx.commit().map_err(|failure| {
            tracing::error!(db_path = %path.display(), adapter = "rust", phase = "COMMIT", pid = std::process::id(), sqlite_code = ?failure.sqlite_error_code(), %failure, "storage.transaction");
            error(failure)
        }),
        Err(failure) => {
            tracing::error!(db_path = %path.display(), adapter = "rust", phase = "callback", pid = std::process::id(), %failure, "storage.transaction");
            if let Err(rollback) = tx.rollback() {
                tracing::error!(db_path = %path.display(), adapter = "rust", phase = "ROLLBACK", pid = std::process::id(), sqlite_code = ?rollback.sqlite_error_code(), %rollback, "storage.transaction");
                return Err(failure.with_cleanup(error(rollback)));
            }
            Err(failure)
        }
    }
    })
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum Row {
    Event {
        record: Box<WireEvent>,
    },
    Output {
        turn_id: String,
        request_id: String,
        started_at: u64,
        records: Vec<WireEvent>,
    },
}
impl Row {
    pub(super) fn wires(&self) -> &[WireEvent] {
        match self {
            Row::Event { record } => std::slice::from_ref(record),
            Row::Output { records, .. } => records,
        }
    }
}
/// Whether events of `kind` are streamed model output, which `write_batch`
/// merges into output rows (native storage format). Output rows carry no
/// identity index, and a write round of them alone waits for its window.
fn output_row(kind: &str) -> bool {
    matches!(kind, "step/text" | "step/thinking")
}
fn head(connection: &Connection, key: &TimelineKey) -> Result<u64> {
    connection
        .prepare_cached(
            "SELECT sequence FROM timeline_branches WHERE session_id = ?1 AND branch_id = ?2",
        )
        .map_err(error)?
        .query_row(params![key.session_id, key.branch_id], |row| {
            read_sequence(row, 0)
        })
        .map_err(error)
}
pub(super) fn read_sequence(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn sql_sequence(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(error)
}
fn sequence_value(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| error("invalid Timeline sequence"))
}
struct OutputRow {
    sequence: u64,
    kind: String,
    bytes: usize,
    row: Row,
}

/// Writes `batch`, with `lists` the prepared lists of each of its records.
fn write_batch(
    connection: &Connection,
    batch: &Pending,
    lists: &[Vec<Option<ListReferences>>],
    outputs: &mut BTreeMap<String, OutputRow>,
) -> Result<()> {
    #[cfg(test)]
    if let Some(fault) = batch.fault {
        fault("write");
    }
    let mut sequence = head(connection, &batch.key)?;
    for (record, lists) in batch.events.iter().zip(lists) {
        if record.event.sequence != sequence + 1 {
            return Err(error("Timeline sequence is not contiguous"));
        }
        sequence += 1;
        let mut wire = codec::encode(record, lists.iter().cloned())?;
        let kind = record.event.kind.key();
        match &record.event.kind {
            EventKind::Text(data) | EventKind::Thinking(data) => {
                if !outputs.contains_key(&batch.key.branch_id) {
                    let previous: Option<(u64, String, String)> = connection.prepare_cached("SELECT sequence, kind, body FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 ORDER BY sequence DESC LIMIT 1").map_err(error)?.query_row(params![batch.key.session_id, batch.key.branch_id], |row| Ok((read_sequence(row, 0)?, row.get(1)?, row.get(2)?))).optional().map_err(error)?;
                    if let Some((sequence, previous_kind, body)) = previous
                        && previous_kind == kind
                        && body.len() < 4096
                    {
                        outputs.insert(
                            batch.key.branch_id.clone(),
                            OutputRow {
                                sequence,
                                kind: previous_kind,
                                bytes: body.len(),
                                row: serde_json::from_str(&body).map_err(error)?,
                            },
                        );
                    }
                }
                let output_data = wire.event["data"]
                    .as_object_mut()
                    .ok_or_else(|| error("output data is missing in event encoding"))?;
                output_data.remove("turn_id");
                output_data.remove("request_id");
                if let Some(previous) = outputs.get_mut(&batch.key.branch_id)
                    && previous.kind == kind
                    && previous.bytes < 4096
                    && let Row::Output {
                        turn_id,
                        request_id,
                        started_at,
                        records,
                    } = &mut previous.row
                    && *turn_id == data.turn_id
                    && *request_id == data.request_id
                    && record.event.create_at.saturating_sub(*started_at) < 1000
                {
                    records.push(wire);
                    let body = serde_json::to_string(&previous.row).map_err(error)?;
                    connection.prepare_cached("UPDATE timeline_events SET last_sequence = ?3, body = ?4 WHERE session_id = ?1 AND branch_id = ?2 AND sequence = ?5").map_err(error)?.execute(params![batch.key.session_id, batch.key.branch_id, sql_sequence(sequence)?, body, sql_sequence(previous.sequence)?]).map_err(error)?;
                    previous.bytes = body.len();
                    continue;
                }
                let row = Row::Output {
                    turn_id: data.turn_id.clone(),
                    request_id: data.request_id.clone(),
                    started_at: record.event.create_at,
                    records: vec![wire],
                };
                let bytes = insert_event(
                    connection,
                    &batch.key,
                    sequence,
                    kind,
                    record.event.kind.context_flags(),
                    &row,
                )?;
                outputs.insert(
                    batch.key.branch_id.clone(),
                    OutputRow {
                        sequence,
                        kind: kind.into(),
                        bytes,
                        row,
                    },
                );
            }
            _ => {
                outputs.remove(&batch.key.branch_id);
                insert_event(
                    connection,
                    &batch.key,
                    sequence,
                    kind,
                    record.event.kind.context_flags(),
                    &Row::Event {
                        record: Box::new(wire),
                    },
                )?;
            }
        }
        if let EventKind::BranchCreate(data) = &record.event.kind {
            let info = BranchRecord {
                id: data.branch.clone(),
                parent: Some(batch.key.branch_id.clone()),
                branch_point: Some(match data.at {
                    BranchCreateAt::Sequence { sequence } => sequence,
                    BranchCreateAt::Create {} => sequence,
                }),
                create_anchor: Some(sequence),
            };
            insert_branch(connection, &batch.key.session_id, &info)?;
        }
    }
    connection
        .prepare_cached(
            "UPDATE timeline_branches SET sequence = ?3 WHERE session_id = ?1 AND branch_id = ?2",
        )
        .map_err(error)?
        .execute(params![
            batch.key.session_id,
            batch.key.branch_id,
            sql_sequence(sequence)?
        ])
        .map_err(error)?;
    if batch.key.branch_id == "main" {
        if let Some(last) = batch.events.last() {
            connection
                .execute(
                    "UPDATE session_catalog SET updated_at_ms=?2 WHERE session_id=?1",
                    params![batch.key.session_id, sql_sequence(last.event.create_at)?],
                )
                .map_err(error)?;
        }
        if batch.events.first().is_some_and(|record| record.event.sequence <= 256)
            && batch.events.iter().any(|record| matches!(&record.event.kind, EventKind::InputMessage(input) if input.user.is_some()))
        {
            super::catalog::refresh(connection, &batch.key.session_id)?;
        }
    }
    Ok(())
}
fn insert_event(
    connection: &Connection,
    key: &TimelineKey,
    sequence: u64,
    kind: &str,
    flags: (bool, bool),
    row: &Row,
) -> Result<usize> {
    let body = serde_json::to_string(row).map_err(error)?;
    connection
        .prepare_cached("INSERT INTO timeline_events VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8)")
        .map_err(error)?
        .execute(params![
            key.session_id,
            key.branch_id,
            sql_sequence(sequence)?,
            kind,
            body,
            match row {
                Row::Event { record } => record.event["id"].as_str(),
                Row::Output { .. } => None,
            },
            i64::from(flags.0),
            i64::from(flags.1),
        ])
        .map_err(error)?;
    Ok(body.len())
}
/// A stored row whose records are decoded in order, across connection holds.
struct Fetched {
    first: u64,
    last: u64,
    kind: String,
    event_id: Option<String>,
    /// Records not decoded yet.
    wires: VecDeque<WireEvent>,
    records: Vec<StoredEvent>,
}
impl Fetched {
    fn parse(
        first: u64,
        last: u64,
        kind: String,
        body: &str,
        event_id: Option<String>,
    ) -> Result<Self> {
        let row: Row = serde_json::from_str(body).map_err(error)?;
        let wires = match row {
            Row::Event { record } => VecDeque::from([*record]),
            Row::Output {
                turn_id,
                request_id,
                records,
                ..
            } => {
                if !output_row(&kind) {
                    return Err(error("output row has invalid event kind"));
                }
                records
                    .into_iter()
                    .map(|mut wire| {
                        let data = wire.event["data"]
                            .as_object_mut()
                            .ok_or_else(|| error("output data is missing"))?;
                        data.insert("turn_id".into(), turn_id.clone().into());
                        data.insert("request_id".into(), request_id.clone().into());
                        Ok(wire)
                    })
                    .collect::<Result<_>>()?
            }
        };
        Ok(Self {
            first,
            last,
            kind,
            event_id,
            wires,
            records: vec![],
        })
    }
}
/// The rows a batch read selects, in the five-column form both of them use:
/// sequence, last sequence, kind, body and the identity the index matched.
fn query_fetched(
    query: &mut rusqlite::Statement<'_>,
    parameters: impl rusqlite::Params,
) -> Result<Vec<Fetched>> {
    query
        .query_map(parameters, |row| {
            Ok((
                read_sequence(row, 0)?,
                read_sequence(row, 1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(error)?
        .map(|row| {
            let (first, last, kind, body, event_id) = row.map_err(error)?;
            Fetched::parse(first, last, kind, &body, event_id)
        })
        .collect()
}
/// The rows of one query, decoded in sequence order with one `Reader`.
struct Rows {
    fetched: VecDeque<Fetched>,
    decoded: Vec<Fetched>,
}
impl Rows {
    /// The rows of `key` that hold a sequence of `range`.
    fn fetch(
        connection: &Connection,
        key: &TimelineKey,
        range: &RangeInclusive<u64>,
    ) -> Result<Self> {
        let mut query = connection.prepare_cached("SELECT sequence, last_sequence, kind, body FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 AND sequence >= coalesce((SELECT max(sequence) FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 AND sequence <= ?3), ?3) AND sequence <= ?4 AND last_sequence >= ?3 ORDER BY sequence").map_err(error)?;
        let rows = query
            .query_map(
                params![
                    key.session_id,
                    key.branch_id,
                    sql_sequence(*range.start())?,
                    sql_sequence(*range.end())?
                ],
                |row| {
                    Ok((
                        read_sequence(row, 0)?,
                        read_sequence(row, 1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(error)?;
        let fetched = rows
            .map(|row| {
                let (first, last, kind, body) = row.map_err(error)?;
                Fetched::parse(first, last, kind, &body, None)
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            fetched,
            decoded: vec![],
        })
    }
    /// Decodes rows in order: `true` once every row is decoded, `false`
    /// when this hold ran out of node loads.
    fn advance(
        &mut self,
        connection: &Connection,
        session: &str,
        reader: &mut Reader,
    ) -> Result<bool> {
        while let Some(row) = self.fetched.front_mut() {
            while let Some(wire) = row.wires.pop_front() {
                match reader.decode(connection, session, wire)? {
                    Ok(record) => row.records.push(record),
                    Err(wire) => {
                        row.wires.push_front(wire);
                        return Ok(false);
                    }
                }
            }
            let row = self.fetched.pop_front().expect("row is being decoded");
            if row.records.is_empty()
                || row
                    .records
                    .iter()
                    .any(|record| record.event.kind.key() != row.kind)
            {
                return Err(error("stored event kind does not match row"));
            }
            self.decoded.push(row);
        }
        Ok(true)
    }
    /// The decoded records within `range`, after the row checks of a range
    /// read; `boundary` also requires `range` to end on a batch boundary
    /// and, after the first sequence, to have no gap.
    fn records(&mut self, range: &RangeInclusive<u64>, boundary: bool) -> Result<Vec<StoredEvent>> {
        let mut records = vec![];
        for row in std::mem::take(&mut self.decoded) {
            if row.records.first().unwrap().event.sequence != row.first
                || row.records.last().unwrap().event.sequence != row.last
                || row
                    .records
                    .windows(2)
                    .any(|pair| pair[0].event.sequence + 1 != pair[1].event.sequence)
            {
                return Err(error("stored output sequence range is invalid"));
            }
            records.extend(
                row.records
                    .into_iter()
                    .filter(|record| range.contains(&record.event.sequence)),
            );
        }
        if boundary
            && records
                .last()
                .is_some_and(|record| record.batch_end > *range.end())
        {
            return Err(AgentError::InvalidInput(format!(
                "sequence {} is inside an atomic batch",
                range.end()
            )));
        }
        if boundary && *range.start() > 0 && records.len() as u64 != range.end() - range.start() + 1
        {
            return Err(error("Timeline read contains a sequence gap"));
        }
        Ok(records)
    }
}
/// Runs `step` with `reader` in read-connection holds until it returns `true`;
/// each hold is one `run_read` and allows `reader` at most
/// `NODE_LOADS_PER_HOLD` node loads (§7.2 of the request storage design), so a
/// waiting writer gets its own connection between them. `state` carries the
/// work across holds.
async fn holds<S: Send + 'static>(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    state: S,
    step: impl Fn(&Connection, &mut Reader, &mut S) -> Result<bool> + Send + Sync + 'static,
) -> Result<S> {
    holds_in(db, reader, state, false, step).await
}
/// [`holds`] on the writer's connection, for a step that writes.
async fn holds_writing<S: Send + 'static>(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    state: S,
    step: impl Fn(&Connection, &mut Reader, &mut S) -> Result<bool> + Send + Sync + 'static,
) -> Result<S> {
    holds_in(db, reader, state, true, step).await
}
async fn holds_in<S: Send + 'static>(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    mut state: S,
    writing: bool,
    step: impl Fn(&Connection, &mut Reader, &mut S) -> Result<bool> + Send + Sync + 'static,
) -> Result<S> {
    let step = Arc::new(step);
    loop {
        let (step, mut owned) = (step.clone(), std::mem::take(reader));
        let (result, owned, returned) = if writing {
            db.run(move |connection| {
                owned.begin_hold();
                let result = step(connection, &mut owned, &mut state);
                Ok((result, owned, state))
            })
            .await?
        } else {
            db.run_read(move |connection| {
                owned.begin_hold();
                let result = step(connection, &mut owned, &mut state);
                Ok((result, owned, state))
            })
            .await?
        };
        *reader = owned;
        state = returned;
        if result? {
            return Ok(state);
        }
    }
}
/// The records of `key` in `range`, decoded with `reader`.
pub(super) async fn read_range(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    key: &TimelineKey,
    range: RangeInclusive<u64>,
    boundary: bool,
) -> Result<Vec<StoredEvent>> {
    if range.is_empty() {
        return Ok(vec![]);
    }
    let (key, fetch) = (key.clone(), range.clone());
    let rows = holds(
        db,
        reader,
        None,
        move |connection, reader, rows: &mut Option<Rows>| {
            let rows = match rows {
                Some(rows) => rows,
                None => rows.insert(Rows::fetch(connection, &key, &fetch)?),
            };
            rows.advance(connection, &key.session_id, reader)
        },
    )
    .await?;
    rows.expect("a finished read fetched its rows")
        .records(&range, boundary)
}
/// The one event of a row an identity index found.
fn indexed(row: Fetched) -> Result<StoredEvent> {
    let Fetched {
        mut records,
        event_id,
        ..
    } = row;
    let record = records
        .pop()
        .filter(|_| records.is_empty())
        .ok_or_else(|| error("indexed event is missing"))?;
    if event_id.as_deref() != Some(record.event.id.as_str()) {
        return Err(error("event identity index does not match row"));
    }
    Ok(record)
}

fn is_tool_result(event: &AgentEvent) -> bool {
    matches!(&event.kind, EventKind::ToolResult(_))
}

fn is_window_event(event: &AgentEvent) -> bool {
    matches!(
        &event.kind,
        EventKind::TurnStart(_) | EventKind::InputMessage(_) | EventKind::InputConsumed(_)
    )
}

fn is_policy_event(event: &AgentEvent) -> bool {
    matches!(
        &event.kind,
        EventKind::ContextConfigured(_)
            | EventKind::ContextCompaction(_)
            | EventKind::ContextBudget(_)
            | EventKind::ModelUsage(_)
            | EventKind::ModelCall(_)
            | EventKind::TurnStart(_)
            | EventKind::InputMessage(_)
            | EventKind::InputConsumed(_)
    )
}

fn window_rows(
    connection: &Connection,
    key: &TimelineKey,
    through: u64,
    turn_id: &str,
    cut: &str,
) -> Result<Vec<(i64, i64)>> {
    let mut rows = Vec::new();
    let anchor: Option<(i64, i64)> = connection
        .prepare(
            "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
             WHERE session_id=?1 AND branch_id=?2 AND event_id=?3 AND sequence<=?4",
        )
        .map_err(error)?
        .query_row(
            params![key.session_id, key.branch_id, cut, through as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some(anchor) = anchor {
        rows.push(anchor);
    }
    let turn: Option<(i64, i64)> = connection
        .prepare(
            "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
             WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 AND kind='turn/start' \
             AND json_extract(body, '$.record.event.data.turn_id')=?4 \
             ORDER BY sequence DESC LIMIT 1",
        )
        .map_err(error)?
        .query_row(
            params![key.session_id, key.branch_id, through as i64, turn_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let previous_cut: Option<String> = connection
        .prepare(
            "SELECT json_extract(body, '$.record.event.data.retain_from') \
             FROM timeline_events \
             WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 \
             AND kind='context/compaction' \
             AND json_extract(body, '$.record.event.data.retain_from') IS NOT NULL \
             ORDER BY sequence DESC LIMIT 1",
        )
        .map_err(error)?
        .query_row(
            params![key.session_id, key.branch_id, through as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let tool_start = match previous_cut {
        Some(cut) => connection
            .prepare(
                "SELECT sequence FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND event_id=?3 AND sequence<=?4",
            )
            .map_err(error)?
            .query_row(
                params![key.session_id, key.branch_id, cut, through as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(error)?
            .unwrap_or(1),
        None => 1,
    };
    rows.extend(
        connection
            .prepare(
                "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND sequence>=?4 AND sequence<=?3 \
                 AND kind='step/tool-result' ORDER BY sequence",
            )
            .map_err(error)?
            .query_map(
                params![key.session_id, key.branch_id, through as i64, tool_start],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?,
    );
    if let Some(turn) = turn {
        rows.push(turn);
        rows.extend(
            connection
                .prepare(
                    "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                     WHERE session_id=?1 AND branch_id=?2 AND sequence>=?3 AND sequence<=?4 \
                     AND kind IN ('turn/start','input/message','input/consumed') \
                     ORDER BY sequence",
                )
                .map_err(error)?
                .query_map(
                    params![key.session_id, key.branch_id, turn.0, through as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(error)?,
        );
        let mut consumed = connection
            .prepare(
                "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND sequence<=?4 \
                 AND kind='input/message' AND event_id IN (\
                   SELECT json_extract(body, '$.record.event.data.input_id') \
                   FROM timeline_events WHERE session_id=?1 AND branch_id=?2 \
                   AND sequence>=?3 AND sequence<=?4 AND kind='input/consumed') \
                 ORDER BY sequence",
            )
            .map_err(error)?;
        rows.extend(
            consumed
                .query_map(
                    params![key.session_id, key.branch_id, turn.0, through as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(error)?,
        );
    }
    Ok(rows)
}

fn policy_rows(
    connection: &Connection,
    key: &TimelineKey,
    through: u64,
    turn_id: &str,
) -> Result<Vec<(i64, i64)>> {
    let mut rows = Vec::new();
    for kind in ["context/configured", "context/compaction", "prompt/changed"] {
        let row: Option<(i64, i64)> = connection
            .prepare(
                "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 AND kind=?4 \
                 AND (?4 != 'context/compaction' OR \
                   json_extract(body, '$.record.event.data.status')='completed') \
                 ORDER BY sequence DESC LIMIT 1",
            )
            .map_err(error)?
            .query_row(
                params![key.session_id, key.branch_id, through as i64, kind],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some(row) = row {
            rows.push(row);
        }
    }
    let usage: Option<(i64, i64, Option<String>)> = connection
        .prepare(
            "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)), \
             json_extract(body, '$.record.event.data.request_id') \
             FROM timeline_events \
             WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 AND kind='model/usage' \
             ORDER BY sequence DESC LIMIT 1",
        )
        .map_err(error)?
        .query_row(
            params![key.session_id, key.branch_id, through as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some((sequence, last_sequence, Some(request_id))) = usage {
        rows.push((sequence, last_sequence));
        let call: Option<(i64, i64)> = connection
            .prepare(
                "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 \
                 AND kind='model/call' \
                 AND json_extract(body, '$.record.event.data.request_id')=?4 \
                 ORDER BY sequence DESC LIMIT 1",
            )
            .map_err(error)?
            .query_row(
                params![key.session_id, key.branch_id, through as i64, request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some(call) = call {
            rows.push(call);
        }
    }
    let turn: Option<i64> = connection
        .prepare(
            "SELECT sequence FROM timeline_events \
             WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 AND kind='turn/start' \
             AND json_extract(body, '$.record.event.data.turn_id')=?4 \
             ORDER BY sequence DESC LIMIT 1",
        )
        .map_err(error)?
        .query_row(
            params![key.session_id, key.branch_id, through as i64, turn_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if let Some(turn) = turn {
        let mut query = connection
            .prepare(
                "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND sequence>=?4 AND sequence<=?3 \
                 AND kind IN ('turn/start','input/message','input/consumed','context/budget') \
                 ORDER BY sequence",
            )
            .map_err(error)?;
        rows.extend(
            query
                .query_map(
                    params![key.session_id, key.branch_id, through as i64, turn],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(error)?,
        );
        let mut consumed = connection
            .prepare(
                "SELECT sequence, MAX(last_sequence, COALESCE(json_extract(body, '$.record.batch_end'), last_sequence)) FROM timeline_events \
                 WHERE session_id=?1 AND branch_id=?2 AND sequence<=?4 \
                 AND kind='input/message' AND event_id IN (\
                   SELECT json_extract(body, '$.record.event.data.input_id') \
                   FROM timeline_events WHERE session_id=?1 AND branch_id=?2 \
                   AND sequence>=?3 AND sequence<=?4 AND kind='input/consumed') \
                 ORDER BY sequence",
            )
            .map_err(error)?;
        rows.extend(
            consumed
                .query_map(
                    params![key.session_id, key.branch_id, turn, through as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(error)?,
        );
    }
    Ok(rows)
}
impl SqliteTimelineStore {
    /// The prepare step of `session.fork`
    /// ([S5c §6.3](../../../../docs/design/appserver/session-deletion-and-fork.md#63-what-a-fork-copies)):
    /// `fork` gets the source's `SessionConfig` text and owner, the source's
    /// `main` through `at` with the content rows its events reference and the
    /// snapshots anchored on them, a directory without events for every branch
    /// `main` created by then, and an owner row for every Session file and
    /// every output path the source owns (ADR 013 D4). Rows are copied as
    /// stored, except grants: a fork is a new Session and does not inherit
    /// them. A source whose delete committed first is `Closed`. The runtime
    /// validates the copy when it builds the fork.
    ///
    /// The events at or before `at` never change, so their references and the
    /// content closure are read in bounded holds on read connections first.
    /// The copy then writes bounded transactions on its own connection and
    /// ends with the fork's session row: only that last transaction makes the
    /// fork visible, and the writer's commits and publications run between
    /// the others.
    pub async fn fork_session(&self, source: &str, fork: &str, at: u64) -> Result<()> {
        let references = fork_references(&self.db, source, at).await?;
        let content: Vec<String> = content_closure(&self.db, source, references)
            .await?
            .into_iter()
            .collect();
        let (source, fork) = (source.to_owned(), fork.to_owned());
        self.db
            .run_copy(move |connection| copy_fork(connection, &source, &fork, at, &content))
            .await
    }

    /// The `session.delete` row step ([S5c §3.2](../../../../docs/design/appserver/session-deletion-and-fork.md#32-sequence)).
    /// One transaction removes what makes the Session visible — its file and
    /// output owners, grants, title, catalog, owner and session rows and its
    /// branch directories — then its events, snapshots and content go in
    /// bounded transactions, so the writer's commits and publications run
    /// between them. An interruption after the first transaction leaves
    /// unreachable rows of the id; repeating the delete removes them.
    async fn delete_session_rows(&self, session_id: &str) -> Result<()> {
        let session = session_id.to_owned();
        let released = self
            .db
            .run_copy(move |connection| drop_session(connection, &session))
            .await?;
        self.forget(session_id);
        self.admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failures
            .remove(session_id);
        if released > 0 {
            self.sweep_files.notify_one();
        }
        Ok(())
    }
}

/// Rows each copy transaction holds: enough to keep its commit short while
/// the number of commits stays small (#599).
const COPY_EVENTS: u64 = 256;
const COPY_SNAPSHOTS: u64 = 256;
const COPY_CONTENT: usize = 1024;

/// Runs `step` in one write transaction and lets the writer take SQLite's
/// write lock before the next one.
fn chunk<T>(connection: &mut Connection, step: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    let value = step(&tx)?;
    tx.commit().map_err(error)?;
    std::thread::sleep(Duration::from_millis(2));
    Ok(value)
}

/// Copies a fork's rows in bounded transactions; the final transaction
/// inserts the session row, so an interrupted copy leaves no Session.
fn copy_fork(
    connection: &mut Connection,
    source: &str,
    fork: &str,
    at: u64,
    content: &[String],
) -> Result<()> {
    let copied = copy_fork_rows(connection, source, fork, at, content);
    if copied.is_err()
        && let Err(cleanup) = drop_session(connection, fork)
    {
        tracing::error!(session_id = fork, error = %cleanup, "Incomplete fork rows were kept");
    }
    copied
}

fn copy_fork_rows(
    connection: &mut Connection,
    source: &str,
    fork: &str,
    at: u64,
    content: &[String],
) -> Result<()> {
    let cut = sql_sequence(at)?;
    // Intermediate chunks need no sync of their own: the final transaction
    // runs with `synchronous=FULL` and its fsync covers every earlier frame.
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(error)?;
    let mut config = None;
    let mut branches = Vec::new();
    chunk(connection, |tx| {
        let stored: Option<String> = tx
            .query_row(
                "SELECT config FROM agent_sessions WHERE session_id=?1",
                [source],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        config = Some(stored.ok_or_else(|| AgentError::Closed(source.to_owned()))?);
        branches = tx
            .prepare("SELECT branch_id, info FROM timeline_branches WHERE session_id=?1")
            .map_err(error)?
            .query_map([source], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        for (id, info) in &branches {
            let parsed: BranchRecord = serde_json::from_str(info).map_err(error)?;
            let head = if id == "main" {
                cut
            } else if parsed.parent.as_deref() == Some("main")
                && parsed.create_anchor.is_some_and(|anchor| anchor <= at)
            {
                0
            } else {
                continue;
            };
            tx.execute(
                "INSERT INTO timeline_branches VALUES (?1, ?2, ?3, ?4)",
                params![fork, id, info, head],
            )
            .map_err(error)?;
        }
        Ok(())
    })?;
    let mut after = 0;
    while after < at {
        let end = (after + COPY_EVENTS).min(at);
        chunk(connection, |tx| {
            tx.execute(
                "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body, event_id, context_render, context_state) \
                 SELECT ?2, branch_id, sequence, last_sequence, kind, body, event_id, context_render, context_state FROM timeline_events \
                 WHERE session_id=?1 AND branch_id='main' AND sequence>?3 AND sequence<=?4",
                params![source, fork, sql_sequence(after)?, sql_sequence(end)?],
            )
            .map_err(error)?;
            Ok(())
        })?;
        after = end;
    }
    for hashes in content.chunks(COPY_CONTENT) {
        let hashes = serde_json::to_string(hashes).map_err(error)?;
        chunk(connection, |tx| {
            tx.execute(
                "INSERT INTO timeline_content (session_id, hash, kind, body) \
                 SELECT ?2, hash, kind, body FROM timeline_content \
                 WHERE session_id=?1 AND hash IN (SELECT value FROM json_each(?3))",
                params![source, fork, hashes],
            )
            .map_err(error)?;
            Ok(())
        })?;
    }
    let mut after = 0;
    while after < at {
        let end = (after + COPY_SNAPSHOTS).min(at);
        chunk(connection, |tx| {
            tx.execute(
                "INSERT INTO timeline_snapshots (session_id, branch_id, sequence, interpreter, event_id, state) \
                 SELECT ?2, branch_id, sequence, interpreter, event_id, state FROM timeline_snapshots \
                 WHERE session_id=?1 AND branch_id='main' AND sequence>?3 AND sequence<=?4",
                params![source, fork, sql_sequence(after)?, sql_sequence(end)?],
            )
            .map_err(error)?;
            Ok(())
        })?;
        after = end;
    }
    let config = config.expect("the source config was read");
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(error)?;
    chunk(connection, |tx| {
        // A fork is a new Session of the client that forked it, never its
        // source's origin (session classification §3.2).
        tx.execute(
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                fork,
                config,
                sql_sequence(dim_agent_runtime::core::unix_millis())?,
                SessionOrigin::Client.as_str()
            ],
        )
        .map_err(error)?;
        let stored: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id=?1)",
                [source],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !stored {
            return Err(AgentError::Closed(source.to_owned()));
        }
        let owner: Option<(String, u32)> = tx
            .query_row(
                "SELECT agent_id, protocol_version FROM session_owners WHERE session_id=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some((agent_id, protocol_version)) = owner {
            tx.execute(
                "INSERT INTO session_owners VALUES (?1, ?2, ?3)",
                params![fork, agent_id, protocol_version],
            )
            .map_err(error)?;
        }
        // The fork's own fact follows the copied facts in the transaction
        // that makes the fork visible, so F never exists without it (fork
        // §6.5). A fork at 0 copies nothing and starts like a new Session.
        if at > 0 {
            let forked = StoredEvent {
                event: Arc::new(AgentEvent {
                    id: uuid::Uuid::new_v4().to_string(),
                    sequence: at + 1,
                    create_at: dim_agent_runtime::core::unix_millis(),
                    tags: None,
                    kind: EventKind::SessionForked(SessionForkedData {}),
                }),
                trace: None,
                batch_end: at + 1,
            };
            let key = TimelineKey {
                session_id: fork.to_owned(),
                branch_id: "main".into(),
            };
            let wire = codec::encode(&forked, std::iter::empty())?;
            insert_event(
                tx,
                &key,
                at + 1,
                forked.event.kind.key(),
                forked.event.kind.context_flags(),
                &Row::Event {
                    record: Box::new(wire),
                },
            )?;
            tx.execute(
                "UPDATE timeline_branches SET sequence = ?2 WHERE session_id = ?1 AND branch_id = 'main'",
                params![fork, sql_sequence(at + 1)?],
            )
            .map_err(error)?;
        }
        super::catalog::initialize_session_catalog(tx, fork)?;
        files::share(tx, source, fork).map_err(error)?;
        files::share_output(tx, source, fork).map_err(error)?;
        // A fork is a new Session and does not inherit grants (ADR 012 D1).
        Ok(())
    })
}

/// Removes what makes a Session visible in one transaction, then its large
/// Timeline tables in bounded transactions.
fn drop_session(connection: &mut Connection, session: &str) -> Result<usize> {
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(error)?;
    let released = chunk(connection, |tx| {
        let released = files::release(tx, session).map_err(error)?;
        for table in [
            "session_output_owners",
            "session_grants",
            "session_metadata",
            "session_catalog",
            "session_owners",
            "timeline_branches",
            "agent_sessions",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE session_id=?1"),
                [session],
            )
            .map_err(error)?;
        }
        Ok(released)
    })?;
    // The Session is invisible now; a lost delete chunk only leaves
    // unreachable rows for a retry.
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(error)?;
    for table in ["timeline_snapshots", "timeline_events", "timeline_content"] {
        loop {
            let deleted = chunk(connection, |tx| {
                tx.execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid IN \
                         (SELECT rowid FROM {table} WHERE session_id=?1 LIMIT 256)"
                    ),
                    [session],
                )
                .map_err(error)
            })?;
            if deleted == 0 {
                break;
            }
        }
    }
    Ok(released)
}

/// The content rows the events of `source`'s `main` through `at` reference,
/// parsed in bounded holds on read connections.
async fn fork_references(
    db: &Arc<SqliteStorage>,
    source: &str,
    at: u64,
) -> Result<BTreeSet<String>> {
    let mut references = BTreeSet::new();
    let mut after = 0;
    loop {
        let (session, start, limit) = (source.to_owned(), after, sql_sequence(at)?);
        let rows = db
            .run_read(move |connection| {
                let mut query = connection
                    .prepare_cached(
                        "SELECT last_sequence, body FROM timeline_events \
                         WHERE session_id=?1 AND branch_id='main' AND sequence>?2 AND sequence<=?3 \
                         ORDER BY sequence LIMIT 256",
                    )
                    .map_err(error)?;
                query
                    .query_map(params![session, sql_sequence(start)?, limit], |row| {
                        Ok((read_sequence(row, 0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(error)
            })
            .await?;
        let Some((last, _)) = rows.last() else {
            break;
        };
        for (_, body) in &rows {
            let row: Row = serde_json::from_str(body).map_err(error)?;
            for wire in row.wires() {
                references.extend(wire.references().map(str::to_owned));
            }
        }
        after = *last;
    }
    Ok(references)
}

/// The content rows the references reach through node parents, read in one
/// statement on a read connection.
async fn content_closure(
    db: &Arc<SqliteStorage>,
    source: &str,
    roots: BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if roots.is_empty() {
        return Ok(roots);
    }
    let (source, roots) = (
        source.to_owned(),
        serde_json::to_string(&roots).map_err(error)?,
    );
    db.run_read(move |connection| {
        let mut query = connection
            .prepare(
                "WITH RECURSIVE closure(hash) AS ( \
                   SELECT value FROM json_each(?2) \
                   UNION SELECT json_extract(c.body, '$.parent') FROM closure \
                     CROSS JOIN timeline_content c ON c.session_id=?1 AND c.hash=closure.hash \
                     WHERE c.kind='node' AND json_valid(c.body) AND json_type(c.body, '$.parent')='text') \
                 SELECT hash FROM closure",
            )
            .map_err(error)?;
        query
            .query_map(params![source, roots], |row| row.get::<_, String>(0))
            .map_err(error)?
            .collect::<rusqlite::Result<BTreeSet<_>>>()
            .map_err(error)
    })
    .await
}

impl TimelineStore for SqliteTimelineStore {
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let session = session_id.to_owned();
            let config = config.clone();
            let result = self
                .db
                .run(move |connection| {
                    let tx = connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(error)?;
                    create_session(&tx, &session, &config, None, SessionOrigin::Unknown)?;
                    tx.commit().map_err(error)
                })
                .await;
            if result.is_ok() {
                self.forget_validated(session_id);
            }
            result
        })
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // The release command drains the writer's queued batches of the
            // Session first; the rows then go in bounded transactions (#599).
            self.release_session(session_id).await?;
            self.delete_session_rows(session_id).await
        })
    }
    fn release_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (done, wait) = tokio::sync::oneshot::channel();
            {
                let admission = self
                    .admission
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if admission.closed
                    || admission
                        .sender
                        .send(Command::Release {
                            session_id: session_id.into(),
                            done,
                        })
                        .is_err()
                {
                    return Ok(());
                }
            }
            wait.await.unwrap_or(Ok(()))
        })
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        Box::pin(async move {
            let session = session_id.to_owned();
            self.db
                .run_read(move |connection| {
                    let config: Option<String> = connection
                        .query_row(
                            "SELECT config FROM agent_sessions WHERE session_id = ?1",
                            [&session],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(error)?;
                    config
                        .map(|config| serde_json::from_str(&config).map_err(error))
                        .transpose()
                })
                .await
        })
    }
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        Box::pin(async move {
            let directory = match read_directory(&self.db, session_id).await {
                Ok(directory) => directory,
                Err(failure) => {
                    self.forget_validated(session_id);
                    return Err(failure);
                }
            };
            let reused = self
                .validated
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(session_id)
                .is_some_and(|validated| validated == &directory);
            if reused {
                return match directory.branches() {
                    Ok(branches) => Ok(branches.into_values().collect()),
                    Err(failure) => {
                        self.forget_validated(session_id);
                        Err(failure)
                    }
                };
            }
            let mut reader = self.reader(RequestBodies::Rebuild);
            let result = validate_session(&self.db, &mut reader, session_id, &directory).await;
            self.finish(reader);
            match result {
                Ok(branches) => {
                    self.validated
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(session_id.to_owned(), directory);
                    Ok(branches)
                }
                Err(failure) => {
                    self.forget_validated(session_id);
                    Err(failure)
                }
            }
        })
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let key = key.clone();
            self.db
                .run_read(move |connection| head(connection, &key))
                .await
        })
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if admission.closed {
            return Err(error("Timeline storage is closed"));
        }
        if let Some(failure) = admission.failures.get(&key.session_id) {
            return Err(failure.clone());
        }
        let requested = admission.sender.send(Command::Append(Pending {
            key,
            events,
            publish: Some(publish),
            #[cfg(test)]
            fault: None,
        }));
        drop(admission);
        requested.map_err(|failure| {
            if let Command::Append(mut pending) = failure.0 {
                pending.publish.take();
            }
            error("Timeline writer stopped")
        })
    }
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            self.page(key, after, limit, through, RequestBodies::Rebuild)
                .await
        })
    }
    fn read_page_without_request_bodies<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            self.page(key, after, limit, through, RequestBodies::Unused)
                .await
        })
    }

    fn policy_order<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
    ) -> BoxFuture<'a, Result<PolicyOrder>> {
        Box::pin(async move {
            let session = key.session_id.clone();
            let branch = key.branch_id.clone();
            let query_branch = branch.clone();
            let rows = self
                .db
                .run_read(move |connection| {
                    let mut query = connection
                        .prepare(
                            "SELECT event_id, kind, sequence, \
                             json_extract(body, '$.record.event.data.through') \
                             FROM timeline_events \
                             WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 \
                             AND event_id IS NOT NULL \
                             ORDER BY sequence",
                        )
                        .map_err(error)?;
                    query
                        .query_map(params![session, query_branch, through as i64], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, Option<i64>>(3)?,
                            ))
                        })
                        .map_err(error)?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(error)
                })
                .await?;
            Ok(PolicyOrder {
                through,
                branches: Vec::new(),
                events: rows
                    .into_iter()
                    .map(|(id, kind, sequence, through)| PolicyOrderEvent {
                        id,
                        branch_id: branch.clone(),
                        sequence: sequence as u64,
                        turn_start: kind == "turn/start",
                        compaction_start: (kind == "context/compaction-start")
                            .then_some(through.map(|value| value as u64))
                            .flatten(),
                        boundary: false,
                    })
                    .collect(),
            })
        })
    }

    fn policy_window<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        turn_id: &'a str,
        cut: &'a str,
    ) -> BoxFuture<'a, Result<PolicyWindow>> {
        Box::pin(async move {
            let key = key.clone();
            let branch = key.branch_id.clone();
            let window_key = key.clone();
            let (window_turn, window_cut) = (turn_id.to_owned(), cut.to_owned());
            let rows = self
                .db
                .run_read(move |connection| {
                    window_rows(connection, &window_key, through, &window_turn, &window_cut)
                })
                .await?;
            let mut rows = rows;
            rows.sort_unstable();
            let mut ranges: Vec<(String, RangeInclusive<u64>)> = Vec::new();
            for (sequence, last_sequence) in rows {
                let sequence = sequence as u64;
                let last_sequence = last_sequence as u64;
                if let Some((_, range)) = ranges.last_mut()
                    && *range.end() + 1 >= sequence
                {
                    *range = *range.start()..=last_sequence.max(*range.end());
                    continue;
                }
                ranges.push((branch.clone(), sequence..=last_sequence));
            }
            let mut events = Vec::new();
            let mut tool_results = Vec::new();
            for event in self
                .read_segments(&key.session_id, ranges)
                .await?
                .into_iter()
                .flatten()
                .map(|record| record.event)
                .filter(|event| event.sequence <= through)
            {
                if is_tool_result(&event) {
                    tool_results.push(event.clone());
                }
                if event.id == cut || is_window_event(&event) {
                    events.push(event);
                }
            }
            let cut_sequence = events
                .iter()
                .find(|event| event.id == cut)
                .map_or(0, |event| event.sequence);
            Ok(PolicyWindow {
                through,
                cut: cut.to_owned(),
                cut_sequence,
                events,
                tool_results,
            })
        })
    }

    fn policy_facts<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        turn_id: &'a str,
    ) -> BoxFuture<'a, Result<PolicyFacts>> {
        Box::pin(async move {
            let key = key.clone();
            let policy_key = key.clone();
            let branch = key.branch_id.clone();
            let turn_id = turn_id.to_owned();
            let rows = self
                .db
                .run_read(move |connection| policy_rows(connection, &policy_key, through, &turn_id))
                .await?;
            let mut rows = rows;
            rows.sort_unstable();
            let mut ranges: Vec<(String, RangeInclusive<u64>)> = Vec::new();
            for (sequence, last_sequence) in rows {
                let sequence = sequence as u64;
                let last_sequence = last_sequence as u64;
                if let Some((_, range)) = ranges.last_mut()
                    && *range.end() + 1 >= sequence
                {
                    *range = *range.start()..=last_sequence.max(*range.end());
                    continue;
                }
                ranges.push((branch.clone(), sequence..=last_sequence));
            }
            let events = self
                .read_segments(&key.session_id, ranges)
                .await?
                .into_iter()
                .flatten()
                .filter(|record| record.event.sequence <= through && is_policy_event(&record.event))
                .map(|record| PolicyEvent {
                    position: TimelinePosition {
                        branch_id: key.branch_id.clone(),
                        sequence: record.event.sequence,
                    },
                    event: record.event,
                })
                .collect();
            Ok(PolicyFacts { through, events })
        })
    }
    fn events_of_kind<'a>(
        &'a self,
        key: &'a TimelineKey,
        kind: &'a str,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<Arc<dim_agent_types::AgentEvent>>>> {
        Box::pin(async move {
            let session = key.session_id.clone();
            let branch = key.branch_id.clone();
            let wanted = kind.to_owned();
            let end = sql_sequence(through)?;
            let mut reader = self.reader(RequestBodies::Rebuild);
            let rows = holds(
                &self.db,
                &mut reader,
                None,
                move |connection, reader, rows: &mut Option<Rows>| {
                    let rows = match rows {
                        Some(rows) => rows,
                        None => {
                            let mut query = connection
                                .prepare(
                                    "SELECT sequence, last_sequence, kind, body, event_id \
                                     FROM timeline_events \
                                     WHERE session_id=?1 AND branch_id=?2 AND kind=?3 AND sequence<=?4 \
                                     ORDER BY sequence",
                                )
                                .map_err(error)?;
                            rows.insert(Rows {
                                fetched: query_fetched(
                                    &mut query,
                                    rusqlite::params![session, branch, wanted, end],
                                )?
                                .into(),
                                decoded: vec![],
                            })
                        }
                    };
                    rows.advance(connection, &session, reader)
                },
            )
            .await?;
            self.finish(reader);
            let mut rows = rows.expect("a finished read fetched its rows");
            Ok(rows
                .records(&(1..=through), false)?
                .into_iter()
                .map(|record| record.event)
                .collect())
        })
    }
    fn read_kind_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        kinds: &'a [&'a str],
        before: u64,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            if before == 0 || limit == 0 || kinds.is_empty() {
                return Ok(Vec::new());
            }
            let session = key.session_id.clone();
            let branch = key.branch_id.clone();
            let end = sql_sequence(before)?;
            let sql = format!(
                "SELECT sequence, last_sequence, kind, body, event_id \
                 FROM timeline_events \
                 WHERE session_id=? AND branch_id=? AND sequence<=? AND kind IN ({}) \
                 ORDER BY sequence DESC LIMIT ?",
                vec!["?"; kinds.len()].join(",")
            );
            let mut values = vec![
                rusqlite::types::Value::Text(session.clone()),
                rusqlite::types::Value::Text(branch.clone()),
                rusqlite::types::Value::Integer(end),
            ];
            values.extend(
                kinds
                    .iter()
                    .map(|kind| rusqlite::types::Value::Text((*kind).into())),
            );
            values.push(rusqlite::types::Value::Integer(
                i64::try_from(limit).map_err(error)?,
            ));
            let mut reader = self.reader(RequestBodies::Rebuild);
            let rows = holds(
                &self.db,
                &mut reader,
                None,
                move |connection, reader, rows: &mut Option<Rows>| {
                    let rows = match rows {
                        Some(rows) => rows,
                        None => {
                            let mut query = connection.prepare(&sql).map_err(error)?;
                            let fetched = query_fetched(
                                &mut query,
                                rusqlite::params_from_iter(values.iter()),
                            )?;
                            rows.insert(Rows {
                                fetched: fetched.into(),
                                decoded: vec![],
                            })
                        }
                    };
                    rows.advance(connection, &session, reader)
                },
            )
            .await?;
            self.finish(reader);
            let mut records = rows
                .expect("a finished read fetched its rows")
                .records(&(1..=before), false)?;
            records.sort_by_key(|record| record.event.sequence);
            Ok(records)
        })
    }
    fn event_by_id<'a>(
        &'a self,
        key: &'a TimelineKey,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Arc<dim_agent_types::AgentEvent>>>> {
        Box::pin(async move {
            let session = key.session_id.clone();
            let branch = key.branch_id.clone();
            let wanted = id.to_owned();
            let sequence: Option<i64> = self
                .db
                .run_read(move |connection| {
                    connection
                        .query_row(
                            "SELECT sequence FROM timeline_events \
                             WHERE session_id=?1 AND branch_id=?2 AND event_id=?3 LIMIT 1",
                            rusqlite::params![session, branch, wanted],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(error)
                })
                .await?;
            let Some(sequence) = sequence else {
                return Ok(None);
            };
            let mut reader = self.reader(RequestBodies::Rebuild);
            let records = read_range(
                &self.db,
                &mut reader,
                key,
                sequence as u64..=sequence as u64,
                false,
            )
            .await?;
            self.finish(reader);
            Ok(records
                .into_iter()
                .map(|record| record.event)
                .find(|event| event.id == id))
        })
    }
    fn last_event<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<Arc<dim_agent_types::AgentEvent>>>> {
        Box::pin(async move {
            let mut reader = self.reader(RequestBodies::Rebuild);
            let records =
                read_range(&self.db, &mut reader, key, sequence..=sequence, false).await?;
            self.finish(reader);
            Ok(records
                .into_iter()
                .map(|record| record.event)
                .rfind(|event| event.sequence <= sequence))
        })
    }
    fn read_context_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            if after > through || limit == 0 {
                return Ok(Vec::new());
            }
            let mut records = Vec::new();
            let mut cursor = after.max(1);
            let mut decoded = 0;
            while cursor <= through && decoded < limit {
                let end = cursor.saturating_add(255).min(through);
                let mut page = self.read_context(key, cursor, end).await?;
                decoded += page.len();
                records.append(&mut page);
                cursor = end + 1;
            }
            let mut records: Vec<_> = records
                .into_iter()
                .filter(|record| record.event.sequence > after)
                .collect();
            if let Some(record) = records.get(limit - 1) {
                let end = record.batch_end;
                while cursor <= end && cursor <= through {
                    let window = cursor.saturating_add(255).min(through);
                    let mut page = self.read_context(key, cursor, window).await?;
                    cursor = window + 1;
                    records.append(&mut page);
                }
                records.retain(|record| record.event.sequence <= end);
            }
            Ok(records)
        })
    }
    fn read_context<'a>(
        &'a self,
        key: &'a TimelineKey,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            if from == 0 || from > through {
                return Ok(Vec::new());
            }
            let session = key.session_id.clone();
            let branch = key.branch_id.clone();
            let (start, end) = (sql_sequence(from)?, sql_sequence(through)?);
            let mut reader = self.reader(RequestBodies::Rebuild);
            let rows = holds(
                &self.db,
                &mut reader,
                None,
                move |connection, reader, rows: &mut Option<Rows>| {
                    let rows = match rows {
                        Some(rows) => rows,
                        None => {
                            let mut query = connection
                                .prepare(
                                    "SELECT sequence, last_sequence, kind, body, event_id FROM timeline_events \
                             WHERE session_id=?1 AND branch_id=?2 \
                             AND sequence >= coalesce((SELECT max(sequence) FROM timeline_events \
                               WHERE session_id=?1 AND branch_id=?2 AND sequence <= ?3), ?3) \
                             AND sequence<=?4 AND last_sequence >= ?3 \
                             AND (context_render=1 OR context_state=1) ORDER BY sequence"
                                )
                                .map_err(error)?;
                            rows.insert(Rows {
                                fetched: query_fetched(
                                    &mut query,
                                    rusqlite::params![session, branch, start, end],
                                )?
                                .into(),
                                decoded: vec![],
                            })
                        }
                    };
                    rows.advance(connection, &session, reader)
                },
            )
            .await?;
            self.finish(reader);
            let mut rows = rows.expect("a finished read fetched its rows");
            let records = rows.records(&(from..=through), false)?;
            Ok(records)
        })
    }
    fn last_compaction_cut<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
    ) -> BoxFuture<'a, Result<Option<CompactionCut>>> {
        Box::pin(async move {
            let key = key.clone();
            self.db
                .run(move |connection| {
                    connection
                        .query_row(
                            "SELECT json_extract(body, '$.record.event.data.retain_from'), sequence \
                             FROM timeline_events \
                             WHERE session_id=?1 AND branch_id=?2 AND kind='context/compaction' \
                             AND sequence<=?3 \
                             AND json_extract(body, '$.record.event.data.status')='completed' \
                             AND json_extract(body, '$.record.event.data.retain_from') IS NOT NULL \
                             ORDER BY sequence DESC LIMIT 1",
                            params![key.session_id, key.branch_id, sql_sequence(through)?],
                            |row| {
                                Ok(CompactionCut {
                                    retain_from: row.get(0)?,
                                    sequence: read_sequence(row, 1)?,
                                })
                            },
                        )
                        .optional()
                        .map_err(error)
                })
                .await
        })
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        Box::pin(async move {
            self.segments(session_id, segments, RequestBodies::Rebuild)
                .await
        })
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        Box::pin(async move {
            let mut reader = self.reader(RequestBodies::Rebuild);
            let record = read_range(&self.db, &mut reader, key, sequence..=sequence, false)
                .await?
                .pop();
            self.finish(reader);
            Ok(record.and_then(|record| record.trace))
        })
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let key = key.clone();
            let mut reader = self.reader(RequestBodies::Rebuild);
            let sequence = snapshot.sequence;
            holds_writing(&self.db, &mut reader, None, move |connection, reader, rows: &mut Option<Rows>| {
                let rows = match rows {
                    Some(rows) => rows,
                    None => rows.insert(Rows::fetch(connection, &key, &(sequence..=sequence))?),
                };
                if !rows.advance(connection, &key.session_id, reader)? {
                    return Ok(false);
                }
                let anchor = rows
                    .records(&(sequence..=sequence), false)?
                    .pop()
                    .ok_or_else(|| error("snapshot anchor is missing"))?;
                if anchor.event.id != snapshot.event_id {
                    return Err(error("snapshot anchor is not the identified event"));
                }
                connection.execute(
                    "INSERT OR REPLACE INTO timeline_snapshots VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![key.session_id, key.branch_id, sql_sequence(snapshot.sequence)?, snapshot.interpreter, snapshot.event_id, snapshot.state],
                ).map_err(error)?;
                Ok(true)
            })
            .await?;
            self.finish(reader);
            Ok(())
        })
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        Box::pin(async move {
            let key = key.clone();
            let interpreter = interpreter.to_owned();
            self.db.run_read(move |connection| {
                connection.query_row(
                    "SELECT s.sequence, s.event_id, s.interpreter, s.state FROM timeline_snapshots s JOIN timeline_events e ON e.session_id=s.session_id AND e.branch_id=s.branch_id AND e.sequence=s.sequence AND e.event_id=s.event_id WHERE s.session_id=?1 AND s.branch_id=?2 AND s.sequence<=?3 AND s.interpreter=?4 AND typeof(s.sequence)='integer' AND s.sequence>0 AND typeof(s.event_id)='text' AND typeof(s.state)='text' AND json_valid(e.body) AND json_extract(e.body, '$.record.event.id')=s.event_id AND json_extract(e.body, '$.record.event.sequence')=s.sequence ORDER BY s.sequence DESC LIMIT 1",
                    params![key.session_id, key.branch_id, sql_sequence(through)?, interpreter],
                    |row| Ok(RecoverySnapshot { sequence: read_sequence(row, 0)?, event_id: row.get(1)?, interpreter: row.get(2)?, state: row.get(3)? }),
                ).optional().map_err(error)
            }).await
        })
    }
}

/// The branch directory a validation pass fixes: every branch row, in
/// branch-id order. The Session config is parsed by the read but is not part of
/// the directory (restore validation reuse).
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ValidatedDirectory {
    branches: Vec<(String, String, u64)>,
}
impl ValidatedDirectory {
    /// The directory's branches after the checks that need no event: identity,
    /// main as the root, and every child's parent, anchor, cut and a
    /// cycle-free ancestry.
    fn branches(&self) -> Result<BTreeMap<String, StoredBranch>> {
        let mut branches = BTreeMap::new();
        for (id, info, sequence) in &self.branches {
            let info: BranchRecord = serde_json::from_str(info).map_err(error)?;
            if info.id != *id {
                return Err(error("branch directory identity mismatch"));
            }
            branches.insert(
                id.clone(),
                StoredBranch {
                    info,
                    sequence: *sequence,
                },
            );
        }
        let main = branches
            .get("main")
            .ok_or_else(|| error("Session main directory is missing"))?;
        if main.info.parent.is_some()
            || main.info.branch_point.is_some()
            || main.info.create_anchor.is_some()
        {
            return Err(error("invalid main branch directory"));
        }
        for (id, branch) in &branches {
            if id == "main" {
                continue;
            }
            let parent = branch
                .info
                .parent
                .as_ref()
                .ok_or_else(|| error("child branch has no parent"))?;
            let anchor = branch
                .info
                .create_anchor
                .ok_or_else(|| error("child branch has no create anchor"))?;
            branch
                .info
                .branch_point
                .ok_or_else(|| error("child branch has no branch point"))?;
            let parent_branch = branches
                .get(parent)
                .ok_or_else(|| error("child branch parent is missing"))?;
            if anchor > parent_branch.sequence {
                return Err(error("child branch references future create anchor"));
            }
            let mut seen = BTreeSet::new();
            let mut cursor = Some(id);
            while let Some(current) = cursor {
                if !seen.insert(current) {
                    return Err(error("branch directory cycle"));
                }
                cursor = branches
                    .get(current)
                    .ok_or_else(|| error("child branch ancestor is missing"))?
                    .info
                    .parent
                    .as_ref();
            }
        }
        Ok(branches)
    }
}
/// Reads `session`'s directory: the config must exist and parse, and the branch
/// rows come in branch-id order.
fn directory_rows(connection: &Connection, session: &str) -> Result<ValidatedDirectory> {
    let config: String = connection
        .query_row(
            "SELECT config FROM agent_sessions WHERE session_id=?1",
            [session],
            |row| row.get(0),
        )
        .map_err(error)?;
    let _: SessionConfig = serde_json::from_str(&config).map_err(error)?;
    let branches = connection
        .prepare("SELECT branch_id, info, sequence FROM timeline_branches WHERE session_id=?1 ORDER BY branch_id")
        .map_err(error)?
        .query_map([session], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                read_sequence(row, 2)?,
            ))
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    Ok(ValidatedDirectory { branches })
}
/// The directory read of a restore or export pass.
pub(super) async fn read_directory(
    db: &Arc<SqliteStorage>,
    session: &str,
) -> Result<ValidatedDirectory> {
    let id = session.to_owned();
    db.run_read(move |connection| directory_rows(connection, &id))
        .await
}

/// Checks every row of `session` with one reader, which carries its content
/// cache and kept request across the pass. `directory` is the read this pass
/// validates and, on success, records.
pub(super) async fn validate_session(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    session: &str,
    directory: &ValidatedDirectory,
) -> Result<Vec<StoredBranch>> {
    let id = session.to_owned();
    let orphan: bool = db
        .run_read(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM timeline_events e LEFT JOIN timeline_branches b \
             ON b.session_id=e.session_id AND b.branch_id=e.branch_id \
             WHERE e.session_id=?1 AND b.branch_id IS NULL)",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(error)
        })
        .await?;
    if orphan {
        return Err(error("Timeline event references a missing branch"));
    }
    let branches = directory.branches()?;
    let mut event_ids = BTreeSet::new();
    let mut created = BTreeSet::new();
    let mut completions = vec![];
    for (id, _info, sequence) in &directory.branches {
        let mut next = 1;
        let mut batch_end = 0;
        loop {
            let key = TimelineKey {
                session_id: session.into(),
                branch_id: id.clone(),
            };
            let rows = holds(db, reader, None, move |connection, reader, rows: &mut Option<Rows>| {
                let rows = match rows {
                    Some(rows) => rows,
                    None => {
                        let mut query = connection.prepare_cached("SELECT sequence, last_sequence, kind, body, event_id FROM timeline_events WHERE session_id=?1 AND branch_id=?2 AND sequence>=?3 ORDER BY sequence LIMIT 64").map_err(error)?;
                        let fetched = query.query_map(params![key.session_id, key.branch_id, sql_sequence(next)?], |row| Ok((
                            read_sequence(row,0)?, read_sequence(row,1)?, row.get::<_,String>(2)?, row.get::<_,String>(3)?, row.get::<_,Option<String>>(4)?
                        ))).map_err(error)?.map(|row| {
                            let (first, last, kind, body, event_id) = row.map_err(error)?;
                            Fetched::parse(first, last, kind, &body, event_id)
                        }).collect::<Result<_>>()?;
                        rows.insert(Rows { fetched, decoded: vec![] })
                    }
                };
                rows.advance(connection, &key.session_id, reader)
            }).await?;
            let page = rows.expect("a finished pass fetched its rows").decoded;
            if page.is_empty() {
                break;
            }
            for Fetched {
                first,
                last,
                kind,
                event_id,
                records,
                ..
            } in page
            {
                let indexed = !output_row(&kind);
                if indexed && event_id.as_deref() != Some(records[0].event.id.as_str())
                    || !indexed && event_id.is_some()
                {
                    return Err(error("event identity index does not match row"));
                }
                if first != next || records.last().unwrap().event.sequence != last {
                    return Err(error("Timeline contains a sequence gap"));
                }
                for record in records {
                    if !event_ids.insert(record.event.id.clone()) {
                        return Err(error("duplicate Timeline event identity"));
                    }
                    if record.event.sequence != next
                        || record.batch_end < next
                        || record.batch_end > *sequence
                    {
                        return Err(error("invalid Timeline sequence or batch boundary"));
                    }
                    if next > batch_end {
                        batch_end = record.batch_end
                    } else if batch_end != record.batch_end {
                        return Err(error("inconsistent atomic batch boundary"));
                    }
                    if record.trace.as_ref().is_some_and(|trace| {
                        trace.event_id != record.event.id
                            || trace.original.kind.key() != kind
                            || trace.steps.iter().any(|step| {
                                step.result
                                    .as_ref()
                                    .is_some_and(|draft| draft.kind.key() != kind)
                            })
                    }) {
                        return Err(error("before trace does not match event"));
                    }
                    if let EventKind::BranchCreate(data) = &record.event.kind {
                        created.insert(data.branch.clone());
                    }
                    if let EventKind::ContextCompaction(data) = &record.event.kind
                        && matches!(data.outcome, ContextCompactionResult::Completed { .. })
                    {
                        completions.push((id.clone(), record.event.sequence, data.through));
                    }
                    next += 1;
                }
            }
        }
        if next != *sequence + 1 || batch_end != *sequence {
            return Err(error("Timeline head or final batch boundary is invalid"));
        }
    }
    if created.iter().any(|id| !branches.contains_key(id)) {
        return Err(error("branch create event has no directory"));
    }
    for (id, branch) in &branches {
        if id == "main" {
            continue;
        }
        let parent = branch
            .info
            .parent
            .as_ref()
            .expect("the directory check required a parent");
        let anchor = branch
            .info
            .create_anchor
            .expect("the directory check required a create anchor");
        let cut = branch
            .info
            .branch_point
            .expect("the directory check required a branch point");
        let parent_key = TimelineKey {
            session_id: session.into(),
            branch_id: parent.clone(),
        };
        let create = read_range(db, reader, &parent_key, anchor..=anchor, false)
            .await?
            .pop()
            .ok_or_else(|| error("branch create event is missing"))?;
        match &create.event.kind {
            EventKind::BranchCreate(data)
                if data.branch == *id
                    && cut
                        == match data.at {
                            BranchCreateAt::Create {} => anchor,
                            BranchCreateAt::Sequence { sequence } => sequence,
                        } => {}
            _ => return Err(error("branch directory does not match create event")),
        }
    }
    for (branch, at, through) in completions {
        if through >= at || through > branches[&branch].sequence {
            return Err(error("compaction refers to a future preparation boundary"));
        }
        let key = TimelineKey {
            session_id: session.into(),
            branch_id: branch.clone(),
        };
        if through > 0 {
            read_range(db, reader, &key, through..=through, true)
                .await
                .map_err(|_| error("compaction preparation boundary is inside an atomic batch"))?;
        }
    }
    Ok(branches.into_values().collect())
}

#[cfg(test)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_waits_for_a_copy_whose_waiter_was_aborted() {
    let directory = std::env::temp_dir().join(format!("dim-copy-close-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let db = SqliteStorage::open(directory.join("dimcode.sqlite")).unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let copying = {
        let db = db.clone();
        tokio::spawn(async move {
            db.run_copy(move |connection| {
                let _ = entered.send(());
                blocked
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the copy is released");
                connection
                    .execute_batch("PRAGMA user_version = 77")
                    .map_err(error)
            })
            .await
        })
    };
    ready.await.unwrap();
    copying.abort();
    let _ = copying.await;
    let closing = {
        let db = db.clone();
        tokio::spawn(async move { db.close().await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !closing.is_finished(),
        "close returned while the aborted copy was still running"
    );
    release.send(()).unwrap();
    closing.await.unwrap().unwrap();
    let connection = Connection::open(directory.join("dimcode.sqlite")).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 77, "the copy committed before close returned");
    drop(connection);
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(test)]
#[tokio::test]
async fn a_read_sees_the_last_commit_while_a_write_transaction_is_open() {
    let directory =
        std::env::temp_dir().join(format!("dim-read-snapshot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let db = SqliteStorage::open(directory.join("dimcode.sqlite")).unwrap();
    db.run(|connection| {
        connection
            .execute(
                "INSERT INTO settings_documents (source, version, document, imported_revision) \
                 VALUES ('user/test', 1, 'before', 'revision')",
                [],
            )
            .map_err(error)?;
        Ok(())
    })
    .await
    .unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let writing = {
        let db = db.clone();
        tokio::spawn(async move {
            db.run(move |connection| {
                connection.execute_batch("BEGIN IMMEDIATE").map_err(error)?;
                connection
                    .execute(
                        "UPDATE settings_documents SET document='after' WHERE source='user/test'",
                        [],
                    )
                    .map_err(error)?;
                let _ = entered.send(());
                blocked
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the write transaction is released");
                connection.execute_batch("COMMIT").map_err(error)
            })
            .await
        })
    };
    ready.await.unwrap();
    let document: String = db
        .run_read(|connection| {
            connection
                .query_row(
                    "SELECT document FROM settings_documents WHERE source='user/test'",
                    [],
                    |row| row.get(0),
                )
                .map_err(error)
        })
        .await
        .unwrap();
    assert_eq!(
        document, "before",
        "a read sees the last commit, not the open transaction"
    );
    release.send(()).unwrap();
    writing.await.unwrap().unwrap();
    let document: String = db
        .run_read(|connection| {
            connection
                .query_row(
                    "SELECT document FROM settings_documents WHERE source='user/test'",
                    [],
                    |row| row.get(0),
                )
                .map_err(error)
        })
        .await
        .unwrap();
    assert_eq!(document, "after");
    db.close().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(test)]
#[tokio::test]
async fn storage_panic_flow_preserves_connection_and_settles_accepted_batches() {
    use dim_agent_runtime::{Timeline, core::close_timeline};
    use dim_agent_types::{EventDraft, InputMessageData};
    struct CallbackResource {
        store: Arc<SqliteTimelineStore>,
        released: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Drop for CallbackResource {
        fn drop(&mut self) {
            self.released.store(
                self.store.admission.try_lock().is_ok(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        let directory = std::env::temp_dir().join(format!("dim-storage-panic-{}", uuid::Uuid::new_v4()));
        let db = SqliteStorage::open(directory.join("dimcode.sqlite")).unwrap();
        let failed = db.run::<()>(|connection| {
            connection.execute_batch("BEGIN IMMEDIATE; INSERT INTO settings_documents VALUES ('panic',1,'{}','test')").unwrap();
            panic!("injected SQLite operation panic");
        }).await;
        assert!(matches!(failed, Err(AgentError::Storage(message)) if message.contains("SQLite operation panicked: injected SQLite operation panic")));
        assert_eq!(db.run(|connection| connection.query_row("SELECT count(*) FROM settings_documents", [], |row| row.get::<_, i64>(0)).map_err(error)).await.unwrap(), 0);
        let store = SqliteTimelineStore::new(db.clone());
        for session in ["healthy", "prepare", "write", "rollback"] {
            store.create_session(session, &SessionConfig::default()).await.unwrap();
        }
        let key = |session: &str| TimelineKey { session_id: session.into(), branch_id: "main".into() };
        let record = |sequence| StoredEvent {
            event: Arc::new(AgentEvent {
                id: uuid::Uuid::new_v4().to_string(),
                sequence,
                create_at: dim_agent_runtime::core::unix_millis(),
                tags: None,
                kind: EventKind::InputMessage(InputMessageData::text("saved", None)),
            }),
            trace: None,
            batch_end: sequence,
        };
        store.append(key("healthy"), vec![record(1)], Box::new(|result| {
            result.unwrap();
            panic!("injected publication callback panic");
        })).unwrap();
        let (published, publication) = tokio::sync::oneshot::channel();
        store.append(key("healthy"), vec![record(2)], Box::new(move |result| { let _ = published.send(result); })).unwrap();
        publication.await.unwrap().unwrap();
        assert_eq!(store.head(&key("healthy")).await.unwrap(), 2);

        for (index, phase) in ["prepare", "write"].into_iter().enumerate() {
            let (failed, failure) = tokio::sync::oneshot::channel();
            let (healthy, publication) = tokio::sync::oneshot::channel();
            let fault: fn(&str) = if phase == "prepare" {
                |phase| { if phase == "prepare" { panic!("injected preparation panic"); } }
            } else {
                |phase| { if phase == "write" { panic!("injected transaction panic"); } }
            };
            {
                let _connection = db.connection.lock().unwrap();
                let (done, _) = tokio::sync::oneshot::channel();
                let admission = store.admission.lock().unwrap();
                admission.sender.send(Command::Rollback { session_id: "barrier".into(), done, fault: None }).unwrap();
                admission.sender.send(Command::Append(Pending {
                    key: key(phase), events: vec![record(1)],
                    publish: Some(Box::new(move |result| { let _ = failed.send(result); })),
                    fault: Some(fault),
                })).unwrap();
                drop(admission);
                store.append(key("healthy"), vec![record(index as u64 + 3)], Box::new(move |result| { let _ = healthy.send(result); })).unwrap();
            }
            let failure = failure.await.unwrap().unwrap_err();
            assert!(matches!(failure, AgentError::Storage(message) if message.contains("injected") && message.contains("panic")));
            publication.await.unwrap().unwrap();
            assert_eq!(store.head(&key(phase)).await.unwrap(), 0, "a panicking write must leave no durable prefix");
            assert_eq!(store.head(&key("healthy")).await.unwrap(), index as u64 + 3);
            let (unexpected, unaccepted) = tokio::sync::oneshot::channel();
            assert!(store.append(key(phase), vec![record(1)], Box::new(move |_| { let _ = unexpected.send(()); })).is_err());
            assert!(unaccepted.await.is_err(), "a rejected append must not invoke its callback");
        }
        let (done, rollback) = tokio::sync::oneshot::channel();
        store.admission.lock().unwrap().sender.send(Command::Rollback {
            session_id: "rollback".into(), done,
            fault: Some(|| panic!("injected rollback panic")),
        }).unwrap();
        assert!(matches!(rollback.await.unwrap(), Err(AgentError::Storage(message)) if message.contains("injected rollback panic")));
        assert!(store.session_config("rollback").await.unwrap().is_some());
        let (recovered, publication) = tokio::sync::oneshot::channel();
        store.append(key("rollback"), vec![record(1)], Box::new(move |result| { let _ = recovered.send(result); })).unwrap();
        publication.await.unwrap().unwrap();
        store.rollback_session("rollback").await.unwrap();
        assert!(store.session_config("rollback").await.unwrap().is_none());
        let (healthy, publication) = tokio::sync::oneshot::channel();
        store.append(key("healthy"), vec![record(5)], Box::new(move |result| { let _ = healthy.send(result); })).unwrap();
        publication.await.unwrap().unwrap();
        assert_eq!(store.head(&key("healthy")).await.unwrap(), 5);
        let poisoned = store.clone();
        assert!(thread::spawn(move || {
            let _admission = poisoned.admission.lock().unwrap();
            panic!("injected admission panic");
        }).join().is_err());
        store.release_session("healthy").await.unwrap();
        assert_eq!(store.head(&key("healthy")).await.unwrap(), 5);
        store.close().await.unwrap();
        assert_eq!(db.run(|connection| connection.query_row("SELECT count(*) FROM timeline_events", [], |row| row.get::<_, i64>(0)).map_err(error)).await.unwrap(), 5);
        let store = SqliteTimelineStore::new(db.clone());
        for session in ["first", "failed", "queued"] {
            store.create_session(session, &SessionConfig::default()).await.unwrap();
        }
        let timeline = Timeline::new(Ctx::root().unwrap(), key("failed"), vec![], store.clone());
        let (first, committed) = tokio::sync::oneshot::channel();
        let (queued, rejected) = tokio::sync::oneshot::channel();
        {
            let _connection = db.connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            store.append(key("first"), vec![record(1)], Box::new(move |result| { let _ = first.send(result); })).unwrap();
            assert!(store.admission.lock().unwrap().sender.send(Command::Panic).is_ok());
            timeline.append(EventDraft::new(EventKind::InputMessage(InputMessageData::text("pending", None)))).unwrap();
            store.append(key("queued"), vec![record(1)], Box::new(move |result| { let _ = queued.send(result); })).unwrap();
        }
        committed.await.unwrap().unwrap();
        assert!(rejected.await.unwrap().is_err(), "an accepted callback must receive an error, not disappear");
        assert!(close_timeline(&timeline).await.is_err());
        let (unexpected, unaccepted) = tokio::sync::oneshot::channel();
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resource = CallbackResource { store: store.clone(), released: released.clone() };
        assert!(store.append(key("queued"), vec![record(1)], Box::new(move |_| { drop(resource); let _ = unexpected.send(()); })).is_err());
        assert!(unaccepted.await.is_err(), "a rejected append must not invoke its callback");
        assert!(released.load(std::sync::atomic::Ordering::SeqCst), "rejected callback resources must be released outside admission");
        assert!(store.close().await.is_err());
        assert_eq!(db.run(|connection| connection.query_row("SELECT count(*) FROM timeline_events", [], |row| row.get::<_, i64>(0)).map_err(error)).await.unwrap(), 6);
        db.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }).await.expect("panic recovery and close must not hang");
}

#[cfg(test)]
#[tokio::test]
async fn release_flow_preserves_history_and_tolerates_a_stopped_writer() {
    use std::{
        future::Future,
        task::{Context, Waker},
    };
    let directory = std::env::temp_dir().join(format!("dim-release-{}", uuid::Uuid::new_v4()));
    let db = SqliteStorage::open(directory.join("dimcode.sqlite")).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("session", &SessionConfig::default())
        .await
        .unwrap();
    let key = TimelineKey {
        session_id: "session".into(),
        branch_id: "main".into(),
    };
    let record = |sequence| StoredEvent {
        event: Arc::new(AgentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            create_at: dim_agent_runtime::core::unix_millis(),
            tags: None,
            kind: EventKind::InputMessage(dim_agent_types::InputMessageData::text("saved", None)),
        }),
        trace: None,
        batch_end: sequence,
    };
    let (done, published) = tokio::sync::oneshot::channel();
    store
        .append(
            key.clone(),
            vec![record(1)],
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        )
        .unwrap();
    published.await.unwrap().unwrap();
    store.release_session("session").await.unwrap();
    store.release_session("session").await.unwrap();
    store.release_session("unknown").await.unwrap();
    let mut release = Box::pin(store.release_session("session"));
    let (done, pending) = tokio::sync::oneshot::channel();
    {
        let _connection = db.connection.lock().unwrap();
        store
            .append(
                key.clone(),
                vec![record(2)],
                Box::new(move |result| {
                    let _ = done.send(result);
                }),
            )
            .unwrap();
        assert!(
            store
                .admission
                .lock()
                .unwrap()
                .sender
                .send(Command::Close)
                .is_ok()
        );
        assert!(
            release
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }
    release.await.unwrap();
    pending.await.unwrap().unwrap();
    store.release_session("session").await.unwrap();
    assert_eq!(store.head(&key).await.unwrap(), 2);
    assert_eq!(store.read(&key, 1..=2).await.unwrap().len(), 2);
    assert!(store.close().await.is_err());
    store.release_session("session").await.unwrap();
    db.close().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
