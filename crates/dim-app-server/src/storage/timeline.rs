use super::{
    SqliteStorage,
    codec::{
        self, CONTENT_BUDGET, ContentRow, ListReferences, Previous, Reader, Values, WireEvent,
    },
    files::{self, SessionFiles},
    metadata::{self, Titles},
    sqlite::error,
};
use dim_agent_capabilities::host::SessionFilesHost;
use dim_agent_runtime::Names;
use dim_agent_runtime::{
    AgentError, BoxFuture, PublishCallback, Result, SessionConfig, StoredBranch, StoredEvent,
    TimelineInfo, TimelineKey, TimelineStore, TimelineTrace,
};
use dim_agent_types::{
    AgentEvent, BranchCreateAt, ContextCompactionResult, EventKind, ModelRequest, RecoverySnapshot,
    SessionForkedData,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, host_key};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    ops::RangeInclusive,
    sync::{Arc, Mutex, Weak, mpsc},
    thread,
    time::{Duration, Instant},
};

/// Rounds of output deltas alone wait until this long after the previous
/// round started (timeline storage §3).
const ROUND_WINDOW: Duration = Duration::from_millis(20);
struct Pending {
    key: TimelineKey,
    events: Vec<StoredEvent>,
    publish: PublishCallback,
}
enum Command {
    Append(Pending),
    Rollback {
        session_id: String,
        done: tokio::sync::oneshot::Sender<Result<()>>,
    },
    Close,
}

/// One process writer batches every Session; only pending events reside here.
pub struct SqliteTimelineStore {
    db: Arc<SqliteStorage>,
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
            memo.lock().unwrap().get(session).map_or(0, |branches| {
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
                    Command::Rollback { session_id, done } => {
                        let Some(store) = weak.upgrade() else { break };
                        let _ = done.send(store.rollback_creation(&session_id));
                        continue;
                    }
                    Command::Close => break,
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
    fn reader(&self) -> Reader {
        Reader::new(self.content_budget)
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
    async fn read_indexed(
        &self,
        session: &str,
        locate: impl Fn(&Connection) -> Result<Option<(String, u64)>> + Send + Sync + 'static,
    ) -> Result<Option<StoredEvent>> {
        let mut reader = self.reader();
        let record = read_indexed(&self.db, &mut reader, session.to_owned(), locate).await?;
        self.finish(reader);
        Ok(record)
    }
    fn commit_round(&self, pending: Vec<Pending>) {
        let mut sessions: BTreeMap<String, Vec<Pending>> = BTreeMap::new();
        for batch in pending {
            let failure = self
                .admission
                .lock()
                .unwrap()
                .failures
                .get(&batch.key.session_id)
                .cloned();
            if let Some(failure) = failure {
                (batch.publish)(Err(failure));
            } else {
                sessions
                    .entry(batch.key.session_id.clone())
                    .or_default()
                    .push(batch);
            }
        }
        let mut prepared = BTreeMap::new();
        for (session, batches) in sessions {
            match self.prepare(&session, &batches) {
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
            memo.lock().unwrap().retain(|_, branches| {
                branches.retain(|_, entry| entry.request.strong_count() > 0);
                !branches.is_empty()
            });
        }
    }
    fn commit_prepared(&self, prepared: BTreeMap<String, (Vec<Pending>, Prepared)>) {
        let mut guard = self.db.connection.lock().unwrap();
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
                for batch in batches {
                    (batch.publish)(Ok(()));
                }
            }
            return;
        }
        for (session, (batches, encoding)) in prepared {
            let existing = self
                .admission
                .lock()
                .unwrap()
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
                        .unwrap()
                        .failures
                        .insert(session, failure.clone());
                }
            }
            for batch in batches {
                (batch.publish)(result.clone());
            }
            guard = self.db.connection.lock().unwrap();
        }
    }
    /// Fails `batches` of `session` without writing them.
    fn fail(&self, session: String, batches: Vec<Pending>, failure: AgentError) {
        self.forget(&session);
        self.admission
            .lock()
            .unwrap()
            .failures
            .insert(session, failure.clone());
        for batch in batches {
            (batch.publish)(Err(failure.clone()));
        }
    }
    /// Records the last request of each branch a commit wrote.
    fn remember(&self, session: &str, encoding: Prepared) {
        let Some(memo) = &self.memo else { return };
        let mut memo = memo.lock().unwrap();
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
            memo.lock().unwrap().remove(session);
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
            let guard = self.db.connection.lock().unwrap();
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
        let mut reader = self.reader();
        let mut round = Round::default();
        let mut prepared = Prepared {
            lists: Vec::with_capacity(batches.len()),
            content: vec![],
            last: BTreeMap::new(),
        };
        let result = (|| {
            for batch in batches {
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
                        let info: TimelineInfo = serde_json::from_str(&info).map_err(error)?;
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
    fn rollback_creation(&self, session_id: &str) -> Result<()> {
        self.forget(session_id);
        let mut guard = self.db.connection.lock().unwrap();
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
            "session_metadata",
            "session_owners",
            "agent_sessions",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE session_id=?1"),
                [session_id],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        if released > 0 {
            self.sweep_files.notify_one();
        }
        self.admission.lock().unwrap().failures.remove(session_id);
        Ok(())
    }
    pub async fn close(self: &Arc<Self>) -> Result<()> {
        let mut receiver = {
            let mut closing = self.closing.lock().unwrap();
            closing
                .get_or_insert_with(|| {
                    let (sender, receiver) = tokio::sync::watch::channel(None);
                    let mut admission = self.admission.lock().unwrap();
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

/// Session directory service; Agent hosting owns the profile and owner meaning.
#[derive(Clone)]
pub struct SessionStore {
    timeline: Arc<SqliteTimelineStore>,
    /// The file steps on the Session's resource node: Session files and the
    /// command output a delete removes.
    files: SessionFiles,
    /// Title metadata (S5b): its one writer.
    pub(super) titles: Titles,
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
        }
    }
    pub fn timeline_store(&self) -> Arc<dyn TimelineStore> {
        self.timeline.clone()
    }
    pub async fn sessions(&self) -> Result<Vec<String>> {
        self.timeline
            .db
            .run(|connection| {
                connection
                    .prepare("SELECT session_id FROM agent_sessions ORDER BY session_id")
                    .map_err(error)?
                    .query_map([], |row| row.get(0))
                    .map_err(error)?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(error)
            })
            .await
    }
    /// Directory-only reads never validate or decode Timeline events. Each
    /// record carries its title metadata, read in the same query.
    pub async fn directory(
        &self,
        agent: Option<String>,
        version: u32,
    ) -> Result<Vec<crate::protocol::SessionInfo>> {
        self.timeline
            .db
            .run(move |connection| {
                let filter = if agent.is_some() {
                    " AND o.agent_id=?2"
                } else {
                    ""
                };
                let sql = format!(
                    "SELECT s.session_id, s.config, s.created_at_ms, b.sequence, o.agent_id, \
                 m.revision, m.title, m.title_source \
                 FROM session_owners o JOIN agent_sessions s USING(session_id) \
                 JOIN timeline_branches b ON b.session_id=s.session_id AND b.branch_id='main' \
                 LEFT JOIN session_metadata m ON m.session_id=s.session_id \
                 WHERE o.protocol_version=?1{filter} ORDER BY s.session_id"
                );
                let mut query = connection.prepare(&sql).map_err(error)?;
                let mut parameters = vec![rusqlite::types::Value::Integer(i64::from(version))];
                if let Some(agent) = agent {
                    parameters.push(agent.into());
                }
                let mut rows = query
                    .query(rusqlite::params_from_iter(parameters))
                    .map_err(error)?;
                let mut result = vec![];
                while let Some(row) = rows.next().map_err(error)? {
                    let id: String = row.get(0).map_err(error)?;
                    let parsed = (|| -> Result<_> {
                        let config: String = row.get(1).map_err(error)?;
                        let config: SessionConfig = serde_json::from_str(&config).map_err(error)?;
                        Ok(crate::protocol::SessionInfo {
                            session_id: id.clone(),
                            cwd: config.cwd,
                            model: dim_agent_types::ModelId {
                                provider_id: config.model.provider_id,
                                model_id: config.model.model_id,
                            },
                            created_at: read_sequence(row, 2).map_err(error)?,
                            sequence: read_sequence(row, 3).map_err(error)?,
                            running: false,
                            attached_clients: 0,
                            agent_id: Some(row.get(4).map_err(error)?),
                            title: Some(metadata::decode(row, 5).map_err(error)?.into()),
                        })
                    })();
                    match parsed {
                        Ok(record) => result.push(record),
                        Err(error) => {
                            tracing::error!(session_id=%id, %error, "Invalid Session directory")
                        }
                    }
                }
                Ok(result)
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
        let key = TimelineKey {
            session_id: id.into(),
            branch_id: "main".into(),
        };
        Ok(self
            .timeline
            .read_page(&key, after, limit, through)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }
    pub async fn read_event(
        &self,
        id: &str,
        event_id: &str,
    ) -> Result<Option<Arc<dim_agent_types::AgentEvent>>> {
        let (session, event_id) = (id.to_owned(), event_id.to_owned());
        let wanted = event_id.clone();
        let record = self
            .timeline
            .read_indexed(id, move |connection| {
                connection
                    .query_row(
                        "SELECT branch_id, sequence FROM timeline_events WHERE session_id=?1 AND event_id=?2",
                        params![session, event_id],
                        |row| Ok((row.get(0)?, read_sequence(row, 1)?)),
                    )
                    .optional()
                    .map_err(error)
            })
            .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        if record.event.id != wanted {
            return Err(error("event identity index does not match row"));
        }
        Ok(Some(record.event))
    }
    /// The main-branch `step/tool-use-start` of call `call_id` in request
    /// `request_id`, found through `timeline_tool_use_starts`.
    pub async fn read_tool_use_start(
        &self,
        id: &str,
        request_id: &str,
        call_id: &str,
    ) -> Result<Option<Arc<dim_agent_types::AgentEvent>>> {
        let session = id.to_owned();
        let request_id = request_id.to_owned();
        let call_id = call_id.to_owned();
        let record = self
            .timeline
            .read_indexed(id, move |connection| {
                connection
                    .query_row(
                        "SELECT sequence FROM timeline_events WHERE session_id=?1 AND branch_id='main' AND kind='step/tool-use-start' AND json_extract(body, '$.record.event.data.request_id')=?2 AND json_extract(body, '$.record.event.data.call_id')=?3",
                        params![session, request_id, call_id],
                        |row| read_sequence(row, 0),
                    )
                    .optional()
                    .map_err(error)
                    .map(|sequence| sequence.map(|sequence| ("main".to_owned(), sequence)))
            })
            .await?;
        Ok(record.map(|record| record.event))
    }
    pub async fn has_records(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.timeline
            .db
            .run(move |connection| {
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
    /// once Agents has closed the Session: its command output directory goes
    /// first, durably, then every row of it in one transaction on the
    /// Timeline writer, the operation a creation rollback runs, then the
    /// Session-file sweep is scheduled for the files it released. Nothing of
    /// the Session is read, so a damaged one goes too, and an unknown id has
    /// nothing to remove. A failure before the commit leaves every row.
    /// Every database in the directory shares `command-output/`, so the
    /// directory is removed only while this database has rows of the id
    /// (§4): a directory of an id without rows here belongs to another
    /// database, or to none. `id` is one path component, as Agents checked
    /// (D3). Returns whether this database had rows of the id.
    pub async fn delete_session(&self, id: &str) -> Result<bool> {
        let stored = self.has_records(id).await?;
        if stored {
            self.files.remove_command_output(id).await?;
        }
        self.timeline.rollback_session(id).await?;
        self.timeline.sweep_files.notify_one();
        Ok(stored)
    }
    /// The prepare step of `session.fork` ([S5c §6.3](../../../../docs/design/appserver/session-deletion-and-fork.md#63-what-a-fork-copies)),
    /// after Agents checked the cut: in one transaction `fork` gets the
    /// source's `SessionConfig` text and owner, the source's `main` through
    /// `at` with the content rows its events reference and the snapshots
    /// anchored on them, a directory without events for every branch `main`
    /// created by then, and an owner row for every Session file the source
    /// owns. Rows are copied as stored. A source whose delete committed first
    /// is `Closed`. The runtime validates the copy when it builds the fork.
    pub async fn fork_session(&self, source: &str, fork: &str, at: u64) -> Result<()> {
        let (source, fork) = (source.to_owned(), fork.to_owned());
        self.timeline
            .db
            .run(move |connection| {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(error)?;
                let config: Option<String> = tx
                    .query_row(
                        "SELECT config FROM agent_sessions WHERE session_id=?1",
                        [&source],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(error)?;
                let Some(config) = config else {
                    return Err(AgentError::Closed(source));
                };
                let cut = sql_sequence(at)?;
                tx.execute(
                    "INSERT INTO agent_sessions VALUES (?1, ?2, ?3)",
                    params![
                        fork,
                        config,
                        sql_sequence(dim_agent_runtime::core::unix_millis())?
                    ],
                )
                .map_err(error)?;
                tx.execute(
                    "INSERT INTO session_owners (session_id, agent_id, protocol_version) \
                     SELECT ?2, agent_id, protocol_version FROM session_owners WHERE session_id=?1",
                    params![source, fork],
                )
                .map_err(error)?;
                let branches = tx
                    .prepare("SELECT branch_id, info FROM timeline_branches WHERE session_id=?1")
                    .map_err(error)?
                    .query_map([&source], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                    .map_err(error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(error)?;
                for (id, info) in branches {
                    let parsed: TimelineInfo = serde_json::from_str(&info).map_err(error)?;
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
                tx.execute(
                    "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body, event_id) \
                     SELECT ?2, branch_id, sequence, last_sequence, kind, body, event_id FROM timeline_events \
                     WHERE session_id=?1 AND branch_id='main' AND sequence<=?3",
                    params![source, fork, cut],
                )
                .map_err(error)?;
                let mut references = BTreeSet::new();
                {
                    let mut bodies = tx
                        .prepare("SELECT body FROM timeline_events WHERE session_id=?1")
                        .map_err(error)?;
                    let bodies = bodies
                        .query_map([&fork], |row| row.get::<_, String>(0))
                        .map_err(error)?;
                    for body in bodies {
                        let row: Row = serde_json::from_str(&body.map_err(error)?).map_err(error)?;
                        for wire in row.wires() {
                            references.extend(wire.references().map(str::to_owned));
                        }
                    }
                }
                tx.execute(
                    "WITH RECURSIVE closure(hash) AS ( \
                       SELECT value FROM json_each(?3) \
                       UNION SELECT json_extract(c.body, '$.parent') FROM closure \
                         CROSS JOIN timeline_content c ON c.session_id=?1 AND c.hash=closure.hash \
                         WHERE c.kind='node' AND json_valid(c.body) AND json_type(c.body, '$.parent')='text') \
                     INSERT INTO timeline_content (session_id, hash, kind, body) \
                     SELECT ?2, hash, kind, body FROM timeline_content \
                     WHERE session_id=?1 AND hash IN (SELECT hash FROM closure)",
                    params![
                        source,
                        fork,
                        serde_json::to_string(&references).map_err(error)?
                    ],
                )
                .map_err(error)?;
                tx.execute(
                    "INSERT INTO timeline_snapshots (session_id, branch_id, sequence, interpreter, event_id, state) \
                     SELECT ?2, branch_id, sequence, interpreter, event_id, state FROM timeline_snapshots \
                     WHERE session_id=?1 AND branch_id='main' AND sequence<=?3",
                    params![source, fork, cut],
                )
                .map_err(error)?;
                // The fork's own fact follows the copied facts in the same
                // transaction, so F never exists without it (fork §6.5). A
                // fork at 0 copies nothing and starts like a new Session.
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
                        session_id: fork.clone(),
                        branch_id: "main".into(),
                    };
                    let wire = codec::encode(&forked, std::iter::empty())?;
                    insert_event(
                        &tx,
                        &key,
                        at + 1,
                        forked.event.kind.key(),
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
                files::share(&tx, &source, &fork).map_err(error)?;
                tx.commit().map_err(error)
            })
            .await
    }
    pub async fn create_owned_session(
        &self,
        session_id: &str,
        config: &SessionConfig,
        agent_id: &str,
        protocol_version: u32,
    ) -> Result<()> {
        let session = session_id.to_owned();
        let config = config.clone();
        let agent = agent_id.to_owned();
        self.timeline
            .db
            .run(move |connection| {
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(error)?;
                create_session(&tx, &session, &config)?;
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

fn create_session(connection: &Connection, session: &str, config: &SessionConfig) -> Result<()> {
    let config = serde_json::to_string(config).map_err(error)?;
    connection
        .execute(
            "INSERT INTO agent_sessions VALUES (?1, ?2, ?3)",
            params![
                session,
                config,
                sql_sequence(dim_agent_runtime::core::unix_millis())?
            ],
        )
        .map_err(error)?;
    let info = TimelineInfo {
        id: "main".into(),
        parent: None,
        branch_point: None,
        create_anchor: None,
    };
    insert_branch(connection, session, &info)
}
fn insert_branch(connection: &Connection, session: &str, info: &TimelineInfo) -> Result<()> {
    connection
        .execute(
            "INSERT INTO timeline_branches VALUES (?1, ?2, ?3, 0)",
            params![
                session,
                info.id,
                serde_json::to_string(info).map_err(error)?
            ],
        )
        .map_err(error)?;
    Ok(())
}
/// Writes each Session's prepared content rows, then its batches.
fn transaction<'a>(
    connection: &mut Connection,
    path: &std::path::Path,
    mut sessions: impl Iterator<Item = (&'a [Pending], &'a Prepared)>,
) -> Result<()> {
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
        batches
            .iter()
            .zip(&prepared.lists)
            .try_for_each(|(batch, lists)| write_batch(&tx, batch, lists))
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
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "lowercase", deny_unknown_fields)]
enum Row {
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
    fn wires(&self) -> &[WireEvent] {
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
        .query_row(
            "SELECT sequence FROM timeline_branches WHERE session_id = ?1 AND branch_id = ?2",
            params![key.session_id, key.branch_id],
            |row| read_sequence(row, 0),
        )
        .map_err(error)
}
fn read_sequence(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn sql_sequence(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(error)
}
/// Writes `batch`, with `lists` the prepared lists of each of its records.
fn write_batch(
    connection: &Connection,
    batch: &Pending,
    lists: &[Vec<Option<ListReferences>>],
) -> Result<()> {
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
                let previous: Option<(u64, String, String)> = connection.query_row("SELECT sequence, kind, body FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 ORDER BY sequence DESC LIMIT 1", params![batch.key.session_id, batch.key.branch_id], |row| Ok((read_sequence(row, 0)?, row.get(1)?, row.get(2)?))).optional().map_err(error)?;
                let output_data = wire.event["data"]
                    .as_object_mut()
                    .ok_or_else(|| error("output data is missing in event encoding"))?;
                output_data.remove("turn_id");
                output_data.remove("request_id");
                if let Some((start, previous_kind, body)) = previous
                    && previous_kind == kind
                    && body.len() < 4096
                    && let Row::Output {
                        turn_id,
                        request_id,
                        started_at,
                        mut records,
                    } = serde_json::from_str(&body).map_err(error)?
                    && turn_id == data.turn_id
                    && request_id == data.request_id
                    && record.event.create_at.saturating_sub(started_at) < 1000
                {
                    records.push(wire);
                    let body = serde_json::to_string(&Row::Output {
                        turn_id,
                        request_id,
                        started_at,
                        records,
                    })
                    .map_err(error)?;
                    connection.execute("UPDATE timeline_events SET last_sequence = ?3, body = ?4 WHERE session_id = ?1 AND branch_id = ?2 AND sequence = ?5", params![batch.key.session_id, batch.key.branch_id, sql_sequence(sequence)?, body, sql_sequence(start)?]).map_err(error)?;
                    continue;
                }
                let row = Row::Output {
                    turn_id: data.turn_id.clone(),
                    request_id: data.request_id.clone(),
                    started_at: record.event.create_at,
                    records: vec![wire],
                };
                insert_event(connection, &batch.key, sequence, kind, &row)?;
            }
            _ => {
                insert_event(
                    connection,
                    &batch.key,
                    sequence,
                    kind,
                    &Row::Event {
                        record: Box::new(wire),
                    },
                )?;
            }
        }
        if let EventKind::BranchCreate(data) = &record.event.kind {
            let info = TimelineInfo {
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
        .execute(
            "UPDATE timeline_branches SET sequence = ?3 WHERE session_id = ?1 AND branch_id = ?2",
            params![
                batch.key.session_id,
                batch.key.branch_id,
                sql_sequence(sequence)?
            ],
        )
        .map_err(error)?;
    Ok(())
}
fn insert_event(
    connection: &Connection,
    key: &TimelineKey,
    sequence: u64,
    kind: &str,
    row: &Row,
) -> Result<()> {
    connection
        .execute(
            "INSERT INTO timeline_events VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
            params![
                key.session_id,
                key.branch_id,
                sql_sequence(sequence)?,
                kind,
                serde_json::to_string(row).map_err(error)?,
                match row {
                    Row::Event { record } => record.event["id"].as_str(),
                    Row::Output { .. } => None,
                }
            ],
        )
        .map_err(error)?;
    Ok(())
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
        let mut query = connection.prepare_cached("SELECT sequence, last_sequence, kind, body FROM timeline_events WHERE session_id = ?1 AND branch_id = ?2 AND sequence <= ?4 AND last_sequence >= ?3 ORDER BY last_sequence").map_err(error)?;
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
/// Runs `step` with `reader` in connection holds until it returns `true`;
/// each hold is one `run` on the connection and allows `reader` at most
/// `NODE_LOADS_PER_HOLD` node loads (§7.2 of the request storage design),
/// so a waiting writer gets the connection between them. `state` carries
/// the work across holds.
async fn holds<S: Send + 'static>(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    mut state: S,
    step: impl Fn(&Connection, &mut Reader, &mut S) -> Result<bool> + Send + Sync + 'static,
) -> Result<S> {
    let step = Arc::new(step);
    loop {
        let (step, mut owned) = (step.clone(), std::mem::take(reader));
        let (result, owned, returned) = db
            .run(move |connection| {
                owned.begin_hold();
                let result = step(connection, &mut owned, &mut state);
                Ok((result, owned, state))
            })
            .await?;
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
/// The record at `sequence` of the branch an index query finds in the
/// first hold, or `None` when it finds none.
async fn read_indexed(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    session: String,
    locate: impl Fn(&Connection) -> Result<Option<(String, u64)>> + Send + Sync + 'static,
) -> Result<Option<StoredEvent>> {
    let found = holds(
        db,
        reader,
        None,
        move |connection, reader, found: &mut Option<Option<(u64, Rows)>>| {
            let found = match found {
                Some(found) => found,
                None => found.insert(match locate(connection)? {
                    Some((branch_id, sequence)) => {
                        let key = TimelineKey {
                            session_id: session.clone(),
                            branch_id,
                        };
                        Some((
                            sequence,
                            Rows::fetch(connection, &key, &(sequence..=sequence))?,
                        ))
                    }
                    None => None,
                }),
            };
            match found {
                Some((_, rows)) => rows.advance(connection, &session, reader),
                None => Ok(true),
            }
        },
    )
    .await?;
    let Some((sequence, mut rows)) = found.flatten() else {
        return Ok(None);
    };
    Ok(Some(
        rows.records(&(sequence..=sequence), false)?
            .pop()
            .ok_or_else(|| error("indexed event is missing"))?,
    ))
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
            self.db
                .run(move |connection| {
                    let tx = connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(error)?;
                    create_session(&tx, &session, &config)?;
                    tx.commit().map_err(error)
                })
                .await
        })
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (done, wait) = tokio::sync::oneshot::channel();
            {
                let admission = self.admission.lock().unwrap();
                if admission.closed {
                    return Err(error("Timeline storage is closed"));
                }
                admission
                    .sender
                    .send(Command::Rollback {
                        session_id: session_id.into(),
                        done,
                    })
                    .map_err(error)?;
            }
            wait.await.map_err(error)?
        })
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        Box::pin(async move {
            let session = session_id.to_owned();
            self.db
                .run(move |connection| {
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
            let mut reader = self.reader();
            let branches = validate_session(&self.db, &mut reader, session_id).await?;
            self.finish(reader);
            Ok(branches)
        })
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let key = key.clone();
            self.db.run(move |connection| head(connection, &key)).await
        })
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        let admission = self.admission.lock().unwrap();
        if admission.closed {
            return Err(error("Timeline storage is closed"));
        }
        if let Some(failure) = admission.failures.get(&key.session_id) {
            return Err(failure.clone());
        }
        admission
            .sender
            .send(Command::Append(Pending {
                key,
                events,
                publish,
            }))
            .map_err(error)
    }
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            if after > through {
                return Err(AgentError::InvalidInput("future Timeline cursor".into()));
            }
            self.read(key, through..=through).await?;
            if after > 0 && after != through {
                self.read(key, after..=after).await?;
            }
            let mut end = through.min(after.saturating_add(limit as u64));
            if end <= after {
                return Ok(vec![]);
            }
            let mut next = after + 1;
            let mut events = vec![];
            let mut reader = self.reader();
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
        })
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        Box::pin(async move {
            let session = session_id.to_owned();
            let mut result = Vec::with_capacity(segments.len());
            let mut reader = self.reader();
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
        })
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        Box::pin(async move {
            let mut reader = self.reader();
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
            let mut reader = self.reader();
            let sequence = snapshot.sequence;
            holds(&self.db, &mut reader, None, move |connection, reader, rows: &mut Option<Rows>| {
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
            self.db.run(move |connection| {
                connection.query_row(
                    "SELECT s.sequence, s.event_id, s.interpreter, s.state FROM timeline_snapshots s JOIN timeline_events e ON e.session_id=s.session_id AND e.branch_id=s.branch_id AND e.sequence=s.sequence AND e.event_id=s.event_id WHERE s.session_id=?1 AND s.branch_id=?2 AND s.sequence<=?3 AND s.interpreter=?4 AND typeof(s.sequence)='integer' AND s.sequence>0 AND typeof(s.event_id)='text' AND typeof(s.state)='text' AND json_valid(e.body) AND json_extract(e.body, '$.record.event.id')=s.event_id AND json_extract(e.body, '$.record.event.sequence')=s.sequence ORDER BY s.sequence DESC LIMIT 1",
                    params![key.session_id, key.branch_id, sql_sequence(through)?, interpreter],
                    |row| Ok(RecoverySnapshot { sequence: read_sequence(row, 0)?, event_id: row.get(1)?, interpreter: row.get(2)?, state: row.get(3)? }),
                ).optional().map_err(error)
            }).await
        })
    }
}

/// Checks every row of `session` with one reader, which carries its content
/// cache and kept request across the pass.
pub(super) async fn validate_session(
    db: &Arc<SqliteStorage>,
    reader: &mut Reader,
    session: &str,
) -> Result<Vec<StoredBranch>> {
    let id = session.to_owned();
    let directory = db.run(move |connection| {
        let config: String = connection.query_row("SELECT config FROM agent_sessions WHERE session_id=?1", [&id], |row| row.get(0)).map_err(error)?;
        let _: SessionConfig = serde_json::from_str(&config).map_err(error)?;
        let orphan: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM timeline_events e LEFT JOIN timeline_branches b \
             ON b.session_id=e.session_id AND b.branch_id=e.branch_id \
             WHERE e.session_id=?1 AND b.branch_id IS NULL)", [&id], |row| row.get(0)).map_err(error)?;
        if orphan { return Err(error("Timeline event references a missing branch")); }
        connection.prepare("SELECT branch_id, info, sequence FROM timeline_branches WHERE session_id=?1 ORDER BY branch_id")
            .map_err(error)?.query_map([id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, read_sequence(row, 2)?)))
            .map_err(error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(error)
    }).await?;
    let mut branches = BTreeMap::new();
    let mut event_ids = BTreeSet::new();
    let mut created = BTreeSet::new();
    let mut completions = vec![];
    for (id, info, sequence) in directory {
        let info: TimelineInfo = serde_json::from_str(&info).map_err(error)?;
        if info.id != id {
            return Err(error("branch directory identity mismatch"));
        }
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
                        || record.batch_end > sequence
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
        if next != sequence + 1 || batch_end != sequence {
            return Err(error("Timeline head or final batch boundary is invalid"));
        }
        branches.insert(id, StoredBranch { info, sequence });
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
            .ok_or_else(|| error("child branch has no parent"))?;
        let anchor = branch
            .info
            .create_anchor
            .ok_or_else(|| error("child branch has no create anchor"))?;
        let cut = branch
            .info
            .branch_point
            .ok_or_else(|| error("child branch has no branch point"))?;
        let parent_branch = branches
            .get(parent)
            .ok_or_else(|| error("child branch parent is missing"))?;
        if anchor > parent_branch.sequence {
            return Err(error("child branch references future create anchor"));
        }
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
