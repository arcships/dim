//! The loop file `scheduler/loops.json`: one inner v1 JSON document inside the
//! host's `{version, text}` envelope, kept by the same
//! [`PluginFileTransactions`] withLock and replace transactions as the run
//! history and the wakeups (design §12). A loop is a session-scoped record
//! whose rounds are ordinary `input/message` inputs of one branch.
//!
//! A fixed cadence skips a due window while its previous round is still pending
//! in the target branch, which it reads through `sessions.pending`; a skipped
//! window writes nothing and the next one is computed again (design 12.2).
use crate::{
    document::{MAX_PROMPT_BYTES, canonical_instant, format_instant},
    engine::sessions,
    methods::Shared,
    runs::SchedulerError,
};
use chrono::{DateTime, Duration, Utc};
use dim_agent_capabilities::session_broadcast::{SessionNotice, notify};
use dim_agent_types::markup::escaped;
use rutis_bridge::session::HostDispatch;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

/// The file inside the host-authorized `scheduler/` directory.
pub const LOOPS_FILE: &str = "scheduler/loops.json";
const FORMAT_VERSION: u32 = 1;
/// At most this many live loops per Session (§12.5).
pub const MAX_LOOPS_PER_SESSION: usize = 8;
/// A fixed cadence: at least a minute, at most seven days (design 12.2).
pub const MIN_EVERY_SECONDS: u64 = 60;
pub const MAX_EVERY_SECONDS: u64 = 7 * 24 * 60 * 60;
/// A self-paced `next` delay, clamped to this range (design 12.4).
pub const MIN_NEXT_SECONDS: i64 = 60;
pub const MAX_NEXT_SECONDS: i64 = 3600;
/// How long a finished loop is kept for `list` (§12.5).
const RETENTION_AFTER_TERMINAL: Duration = Duration::days(7);
/// Creation + this is a loop's `expires_at`.
const LIFETIME: Duration = Duration::days(7);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopState {
    /// A round is due, or delivered and the next one is armed.
    Running,
    /// A self-paced round finished without `next`: no `next_at`, and `next`
    /// resumes it (§12.3).
    Waiting,
    /// The user or the model stopped it, or its target is gone.
    Stopped,
    /// `expires_at` passed.
    Expired,
}

impl LoopState {
    /// The wire name, as the file and `session.loop.list` spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Stopped => "stopped",
            Self::Expired => "expired",
        }
    }

    /// A terminal state is never delivered again and carries `settled_at`.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Expired)
    }

    /// A state that may still deliver a round.
    pub fn is_live(self) -> bool {
        !self.is_terminal()
    }
}

/// The two cadences. `Every` is rounded up to whole minutes and carries the
/// anchor its windows are counted from, so window `k` is
/// `anchor + k * every_seconds` (§12.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cadence {
    SelfPaced,
    Every { every_seconds: u64, anchor: String },
}

impl Cadence {
    pub fn every_seconds(&self) -> Option<u64> {
        match self {
            Self::SelfPaced => None,
            Self::Every { every_seconds, .. } => Some(*every_seconds),
        }
    }

    /// The first window strictly after `now`, for a fixed cadence: a stretch of
    /// missed windows is one round, never a burst (§12.2). Windows are counted
    /// in milliseconds, so a `now` a fraction of a second past a window is
    /// inside that window and the answer is the following one.
    pub fn next_window_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let Self::Every {
            every_seconds,
            anchor,
        } = self
        else {
            return None;
        };
        let anchor = canonical_instant(anchor)?;
        if now < anchor {
            return Some(anchor);
        }
        let step_ms = i64::try_from(*every_seconds).ok()?.checked_mul(1_000)?;
        let elapsed_ms = (now - anchor).num_milliseconds();
        let windows = elapsed_ms.checked_div(step_ms)?.checked_add(1)?;
        Some(anchor + Duration::milliseconds(step_ms.checked_mul(windows)?))
    }
}

/// The fixed interval a loop may carry, rounded up to whole minutes and inside
/// the allowed range; `Err` is the message a refusal carries.
pub fn fixed_seconds(seconds: u64) -> Result<u64, String> {
    // u64::div_ceil is unstable here, so the ceiling is written out.
    let whole = seconds / 60;
    let rounded = if seconds.is_multiple_of(60) {
        whole * 60
    } else {
        (whole + 1) * 60
    };
    if !(MIN_EVERY_SECONDS..=MAX_EVERY_SECONDS).contains(&rounded) {
        return Err(format!(
            "every_seconds must be at least {MIN_EVERY_SECONDS} and at most {MAX_EVERY_SECONDS}"
        ));
    }
    Ok(rounded)
}

/// One loop record. `round` is the round the next delivery carries, and a
/// self-paced loop without `next_at` is `waiting`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loop {
    pub loop_id: String,
    pub session_id: String,
    pub branch_id: String,
    pub prompt: String,
    pub cadence: Cadence,
    pub state: LoopState,
    /// The next due round; absent while `waiting` and after a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<String>,
    /// The round number the next delivery carries: rounds are 1-based.
    pub round: u64,
    /// The round whose input may be unsettled: written before any external
    /// effect and cleared once the delivery settles (A.5, §12.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivering_round: Option<u64>,
    pub expires_at: String,
    pub created_at: String,
    /// The input id of the last delivered round, while it may still be
    /// pending in the target branch. Fixed cadence reads
    /// `sessions.pending` for it before a window delivers again (§12.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_round_at: Option<String>,
    /// Why the loop stopped, when its target was gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
}

impl Loop {
    /// The record invariants. A violation makes the whole file invalid, so the
    /// bytes stay unchanged.
    pub fn validate(&self) -> Result<(), String> {
        let timestamp = |text: &str| canonical_instant(text).is_some();
        if self.loop_id.is_empty()
            || self.session_id.is_empty()
            || self.branch_id.is_empty()
            || self.prompt.is_empty()
            || self.prompt.len() > MAX_PROMPT_BYTES
        {
            return Err(format!(
                "loop {} has an empty field or an oversized prompt",
                self.loop_id
            ));
        }
        if !timestamp(&self.expires_at) || !timestamp(&self.created_at) {
            return Err(format!(
                "loop {} has a non-canonical timestamp",
                self.loop_id
            ));
        }
        if self.settled_at.is_some() != self.state.is_terminal() {
            return Err(format!("loop {} is inconsistent", self.loop_id));
        }
        if self.state.is_terminal() && self.next_at.is_some() {
            return Err(format!(
                "loop {} is terminal but holds a next_at",
                self.loop_id
            ));
        }
        if self.state == LoopState::Waiting && self.next_at.is_some() {
            return Err(format!(
                "loop {} is waiting but holds a next_at",
                self.loop_id
            ));
        }
        // A running loop with a round outstanding may hold no `next_at`: the
        // round's input is unsettled, and the settle decides what follows.
        if self.state == LoopState::Running
            && self.next_at.is_none()
            && self.delivering_round.is_none()
        {
            return Err(format!("loop {} runs without a next_at", self.loop_id));
        }
        if self.round == 0 {
            return Err(format!("loop {} has a zero round", self.loop_id));
        }
        if let Cadence::Every {
            every_seconds,
            anchor,
        } = &self.cadence
        {
            if !(MIN_EVERY_SECONDS..=MAX_EVERY_SECONDS).contains(every_seconds) {
                return Err(format!(
                    "loop {} has an out-of-range every_seconds",
                    self.loop_id
                ));
            }
            if !timestamp(anchor) {
                return Err(format!("loop {} has a non-canonical anchor", self.loop_id));
            }
        }
        for (name, text) in [
            ("next_at", self.next_at.as_deref()),
            ("last_round_at", self.last_round_at.as_deref()),
            ("settled_at", self.settled_at.as_deref()),
        ] {
            if let Some(text) = text
                && !timestamp(text)
            {
                return Err(format!("loop {} has a non-canonical {name}", self.loop_id));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopFile {
    version: u32,
    loops: Vec<Loop>,
}

/// Decodes the inner text of the file; `None` is a missing file, which holds
/// no loops.
fn decode(text: Option<&str>) -> Result<Vec<Loop>, SchedulerError> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let file: LoopFile = serde_json::from_str(text)
        .map_err(|error| SchedulerError::invalid_data(format!("loops.json: {error}")))?;
    if file.version != FORMAT_VERSION {
        return Err(SchedulerError::invalid_data(format!(
            "loops.json has unknown version {}",
            file.version
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for record in &file.loops {
        if !seen.insert(record.loop_id.as_str()) {
            return Err(SchedulerError::invalid_data(format!(
                "loops.json repeats loop {}",
                record.loop_id
            )));
        }
        record.validate().map_err(SchedulerError::invalid_data)?;
    }
    Ok(file.loops)
}

fn encode(loops: Vec<Loop>) -> String {
    serde_json::to_string(&LoopFile {
        version: FORMAT_VERSION,
        loops,
    })
    .expect("loop records contain only serializable fields")
}

/// Reads the records. A file that is not valid v1 is an error, not an empty
/// list.
pub async fn read(
    files: &dim_app_server::plugin_files::PluginFileTransactions,
) -> Result<Vec<Loop>, SchedulerError> {
    files
        .transact(LOOPS_FILE, |current| (None, decode(current)))
        .await?
}

/// Applies `change` to the records under the file lock. `change` returns
/// whether it modified them; an unmodified list is not written back.
async fn update<R: Send + 'static>(
    files: &dim_app_server::plugin_files::PluginFileTransactions,
    change: impl FnOnce(&mut Vec<Loop>) -> Result<(bool, R), SchedulerError> + Send + 'static,
) -> Result<R, SchedulerError> {
    files
        .transact(LOOPS_FILE, move |current| {
            let mut loops = match decode(current) {
                Ok(loops) => loops,
                Err(error) => return (None, Err(error)),
            };
            match change(&mut loops) {
                Ok((true, value)) => (Some(encode(loops)), Ok(value)),
                Ok((false, value)) => (None, Ok(value)),
                Err(error) => (None, Err(error)),
            }
        })
        .await?
}

fn stamp(instant: DateTime<Utc>) -> String {
    format_instant(instant)
}

/// The clamp a self-paced `next` applies (§12.4).
pub fn clamp_next(seconds: i64) -> i64 {
    seconds.clamp(MIN_NEXT_SECONDS, MAX_NEXT_SECONDS)
}

/// The marker text of round `round` (§12.3).
pub fn marker(loop_id: &str, round: u64) -> String {
    format!(
        "<dim-scheduler kind=\"loop\" loop=\"{}\" round=\"{round}\"></dim-scheduler>",
        escaped(loop_id)
    )
}

/// The record the Session part asks the process owner to store.
pub struct StartRequest {
    pub session_id: String,
    pub branch_id: String,
    pub prompt: String,
    /// A fixed cadence's interval; absent starts a self-paced loop.
    pub every_seconds: Option<u64>,
}

/// `loop_not_found` names no loop of the calling Session.
const LOOP_NOT_FOUND: &str = "loop_not_found";
/// `loop_limit` is the per-Session bound (§12.5).
const LOOP_LIMIT: &str = "loop_limit";

/// The process-side service the Session part forwards to: the same shape as
/// [`Wakeups`](crate::wakeups::Wakeups), keyed by the Session part with
/// `names.key()`.
pub struct Loops {
    pub(crate) shared: Arc<Shared>,
}

impl Loops {
    /// Announces one committed record change on the target Session's channel
    /// (design §11). The notice reports that the record changed; `list` is its
    /// truth, and a Session that is not loaded or not observed drops it.
    fn notice(&self, record: &Loop) {
        notify(
            &self.shared.ctx,
            &record.session_id,
            SessionNotice {
                source: "Scheduler".into(),
                kind: "loop".into(),
                data: json!({
                    "branchId": record.branch_id,
                    "loopId": record.loop_id,
                    "state": record.state.name(),
                    "round": record.round,
                    "nextAt": record.next_at,
                }),
            },
        );
    }

    /// Records a loop whose first round is due now, so `/loop` starts through
    /// the delivery path rather than a plain user message (§12.2).
    pub async fn start(&self, request: StartRequest) -> Result<Loop, SchedulerError> {
        let _activity = self.shared.activity.take_hold().ok_or_else(|| {
            SchedulerError::new("service_reloading", "Process admission is paused")
        })?;
        let now = Utc::now();
        let cadence = match request.every_seconds {
            Some(seconds) => Cadence::Every {
                every_seconds: fixed_seconds(seconds)
                    .map_err(|message| SchedulerError::new("loop_invalid", message))?,
                anchor: stamp(now),
            },
            None => Cadence::SelfPaced,
        };
        let record = Loop {
            loop_id: uuid::Uuid::new_v4().to_string(),
            session_id: request.session_id,
            branch_id: request.branch_id,
            prompt: request.prompt,
            cadence,
            state: LoopState::Running,
            next_at: Some(stamp(now)),
            round: 1,
            delivering_round: None,
            expires_at: stamp(now + LIFETIME),
            created_at: stamp(now),
            last_round_at: None,
            last_input_id: None,
            stopped_reason: None,
            settled_at: None,
        };
        record.validate().map_err(SchedulerError::invalid_data)?;
        let created = record.clone();
        let stored = update(&self.shared.files, move |loops| {
            let live = loops
                .iter()
                .filter(|item| item.session_id == created.session_id && item.state.is_live())
                .count();
            if live >= MAX_LOOPS_PER_SESSION {
                return Err(SchedulerError::new(
                    LOOP_LIMIT,
                    format!("a Session holds at most {MAX_LOOPS_PER_SESSION} loops"),
                ));
            }
            loops.push(created);
            Ok((true, record))
        })
        .await?;
        self.note_live(&read(&self.shared.files).await?);
        self.notice(&stored);
        Ok(stored)
    }

    /// Arms the next round of a loop. The delay is clamped to 60..3600 s, and
    /// an armed round clears any round the previous process left unsettled.
    pub async fn next(
        &self,
        session_id: &str,
        loop_id: &str,
        delay_seconds: i64,
    ) -> Result<Loop, SchedulerError> {
        let _activity = self.shared.activity.take_hold().ok_or_else(|| {
            SchedulerError::new("service_reloading", "Process admission is paused")
        })?;
        let at = Utc::now() + Duration::seconds(clamp_next(delay_seconds));
        let (record, _) = self
            .change(session_id, loop_id, move |record| {
                if !record.state.is_live() {
                    return Ok((false, false));
                }
                record.state = LoopState::Running;
                record.next_at = Some(stamp(at));
                record.delivering_round = None;
                record.last_input_id = None;
                Ok((true, true))
            })
            .await?;
        Ok(record)
    }

    /// Stops a loop. A record already terminal is returned unchanged, so a
    /// second stop is not an error (§12.5).
    pub async fn stop(
        &self,
        session_id: &str,
        loop_id: &str,
    ) -> Result<(Loop, bool), SchedulerError> {
        let _activity = self.shared.activity.take_hold().ok_or_else(|| {
            SchedulerError::new("service_reloading", "Process admission is paused")
        })?;
        let (record, settled) = self
            .change(session_id, loop_id, |record| {
                if record.state.is_terminal() {
                    return Ok((false, false));
                }
                record.state = LoopState::Stopped;
                record.next_at = None;
                record.delivering_round = None;
                record.settled_at = Some(stamp(Utc::now()));
                Ok((true, true))
            })
            .await?;
        Ok((record, settled))
    }

    /// The loops of one Session branch, newest first.
    pub async fn list(
        &self,
        session_id: &str,
        branch_id: &str,
    ) -> Result<Vec<Loop>, SchedulerError> {
        let mut own: Vec<Loop> = read(&self.shared.files)
            .await?
            .into_iter()
            .filter(|record| record.session_id == session_id && record.branch_id == branch_id)
            .collect();
        own.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(b.loop_id.cmp(&a.loop_id))
        });
        Ok(own)
    }

    /// Applies `change` to one record of the calling Session under the file
    /// lock and announces the result. A record that is absent, or belongs to
    /// another Session, is `loop_not_found`.
    ///
    /// Takes `shared.loops` for the whole read-modify-write, so a change and
    /// the delivery pass are one serial order at the record level: a `next`
    /// accepted while a pass holds the lock keeps its deadline, because the
    /// pass settles before this change runs. `sessions.input` in queue mode
    /// does not wait for the target's turn, so taking it here cannot deadlock.
    async fn change<R, F>(
        &self,
        session_id: &str,
        loop_id: &str,
        change: F,
    ) -> Result<(Loop, R), SchedulerError>
    where
        F: FnOnce(&mut Loop) -> Result<(bool, R), SchedulerError> + Send + 'static,
        R: Send + 'static,
    {
        let _hold = self.shared.loops.lock().await;
        let session_id = session_id.to_owned();
        let loop_id = loop_id.to_owned();
        let (record, answer, changed) = update(&self.shared.files, move |loops| {
            let Some(record) = loops
                .iter_mut()
                .find(|record| record.loop_id == loop_id && record.session_id == session_id)
            else {
                return Err(SchedulerError::new(
                    LOOP_NOT_FOUND,
                    format!("no loop {loop_id}"),
                ));
            };
            let (changed, answer) = change(record)?;
            if record.state.is_terminal() && record.settled_at.is_none() {
                record.settled_at = Some(stamp(Utc::now()));
            }
            record.validate().map_err(SchedulerError::invalid_data)?;
            let snapshot = record.clone();
            // A recorded settle is a change even when the caller reported
            // none: the record it read now carries `settled_at`, so the caller
            // must see it stored and the file must hold it.
            let changed = changed || snapshot.state.is_terminal();
            Ok((changed, (snapshot, answer, changed)))
        })
        .await?;
        if changed {
            self.note_live(&read(&self.shared.files).await?);
            self.notice(&record);
        }
        Ok((record, answer))
    }
}

impl Loops {
    /// Recomputes `live_loops` from `records`. Every write under `loops`
    /// ends here, so a daemon with no live loop takes no loop work at all
    /// (design 12.2).
    pub(crate) fn note_live(&self, records: &[Loop]) {
        let live = records.iter().any(|record| record.state.is_live());
        self.shared
            .live_loops
            .store(live, std::sync::atomic::Ordering::Release);
    }

    /// Whether any live loop exists, as the last write recorded.
    fn any_live(&self) -> bool {
        self.shared
            .live_loops
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// One modification the pass applies in its single settle write.
struct Change {
    loop_id: String,
    state: Option<LoopState>,
    stopped_reason: Option<Option<String>>,
    next_at: Option<Option<String>>,
    delivering_round: Option<Option<u64>>,
    last_round_at: Option<Option<String>>,
    last_input_id: Option<Option<String>>,
    round: Option<u64>,
}

impl Change {
    /// A round settled. A self-paced loop names no following round, so it
    /// waits for `next`; a fixed one is already due at its next window, and
    /// its delivered input is remembered so that window can check whether the
    /// round is still pending (design 12.2, 12.3).
    fn delivered(record: &Loop, round: u64, input_id: Option<String>, now: DateTime<Utc>) -> Self {
        let done = round.saturating_add(1);
        let next_at = record.cadence.next_window_at(now);
        let state = if next_at.is_some() {
            LoopState::Running
        } else {
            LoopState::Waiting
        };
        Self {
            loop_id: record.loop_id.clone(),
            state: Some(state),
            stopped_reason: None,
            next_at: Some(next_at.map(stamp)),
            delivering_round: Some(None),
            last_round_at: Some(Some(stamp(now))),
            last_input_id: Some(input_id),
            round: Some(done),
        }
    }

    /// A terminal settle, with the code when the target was gone.
    fn settle(loop_id: &str, state: LoopState, reason: Option<String>) -> Self {
        Self {
            loop_id: loop_id.to_owned(),
            state: Some(state),
            stopped_reason: reason.map(Some),
            next_at: Some(None),
            delivering_round: Some(None),
            last_round_at: None,
            last_input_id: Some(None),
            round: None,
        }
    }

    fn apply(self, record: &mut Loop) {
        if let Some(state) = self.state {
            record.state = state;
        }
        if let Some(reason) = self.stopped_reason {
            record.stopped_reason = reason;
        }
        if let Some(next_at) = self.next_at {
            record.next_at = next_at;
        }
        if let Some(round) = self.delivering_round {
            record.delivering_round = round;
        }
        if let Some(round) = self.round {
            record.round = round;
        }
        if let Some(last) = self.last_round_at {
            record.last_round_at = last;
        }
        if let Some(input) = self.last_input_id {
            record.last_input_id = input;
        }
        if record.state.is_terminal() && record.settled_at.is_none() {
            record.settled_at = Some(stamp(Utc::now()));
        }
    }
}

/// What the target branch answered about a delivered round's input.
enum Pending {
    /// The branch still holds it: neither consumed nor deleted.
    Held,
    /// The branch no longer holds it, so the window may deliver.
    Consumed,
    /// The answer did not settle the question; the window is skipped.
    Unknown,
    /// The Session or branch is gone.
    Gone(String),
}

/// Asks the owner whether a round's input is still pending, through the
/// public `sessions.pending` read. Scheduler reads no Timeline (design §5), so
/// the branch's own InputQueue answers (design 12.2).
async fn pending(service: &dyn HostDispatch, record: &Loop, input_id: &str) -> Pending {
    let request = json!({
        "sessionId": record.session_id,
        "branchId": record.branch_id,
        "inputId": input_id,
    });
    match sessions(service, "pending", request).await {
        Ok(Ok(reply)) => {
            if reply.get("pending").and_then(|pending| pending.as_bool()) == Some(true) {
                Pending::Held
            } else {
                Pending::Consumed
            }
        }
        Ok(Err(code)) if crate::engine::target_gone(&code) => Pending::Gone(code),
        Ok(Err(_)) | Err(_) => Pending::Unknown,
    }
}

/// A round's submission identity: the branch accepts each once (A.2).
fn submission(loop_id: &str, round: u64) -> String {
    format!("loop:{loop_id}:{round}")
}

impl Loops {
    /// One pass of the timer over the loops, in the same one-second pass that
    /// serves the jobs and the wakeups (design §12.3): a running loop whose
    /// `next_at` is due delivers its round, a loop past `expires_at` settles
    /// `expired`, and a settled record past the retention window is removed.
    ///
    /// The pass reads the file once, takes one clock reading and writes at most
    /// once over the records it changed. It holds `shared.loops` across the
    /// snapshot read, the input writes and the settle batch, so a `next`, a
    /// `stop` and a pass are one serial order at the record level; nothing it
    /// awaits takes that lock again.
    pub async fn evaluate(&self) -> Result<(), SchedulerError> {
        if !self.any_live() {
            return Ok(());
        }
        let Some(_activity) = self.shared.activity.take_hold() else {
            return Ok(());
        };
        let _hold = self.shared.loops.lock().await;
        let now = Utc::now();
        let snapshot = read(&self.shared.files).await?;
        let retention_cut = now - RETENTION_AFTER_TERMINAL;

        let mut changes: Vec<Change> = Vec::new();
        let mut due: Vec<Loop> = Vec::new();
        for record in &snapshot {
            if record.state.is_terminal() {
                continue;
            }
            let expired = canonical_instant(&record.expires_at).is_some_and(|at| at <= now);
            if expired {
                changes.push(Change::settle(&record.loop_id, LoopState::Expired, None));
                continue;
            }
            if record.state != LoopState::Running {
                continue;
            }
            let Some(next_at) = record.next_at.as_deref().and_then(canonical_instant) else {
                continue;
            };
            if next_at <= now {
                due.push(record.clone());
            }
        }
        due.sort_by(|a, b| a.next_at.cmp(&b.next_at).then(a.loop_id.cmp(&b.loop_id)));

        for record in due {
            let round = record.round;
            // A fixed cadence skips this window while the previous round is
            // still pending in the target branch: it writes nothing and the
            // next window is computed again (design 12.2). A loop with no
            // outstanding id never asks, so a quiet loop restores its Session
            // only for its own write.
            if record.cadence.every_seconds().is_some()
                && let Some(input_id) = record.last_input_id.clone()
            {
                match pending(self.shared.sessions.as_ref(), &record, &input_id).await {
                    Pending::Held => {
                        let next = record.cadence.next_window_at(now);
                        changes.push(Change {
                            loop_id: record.loop_id.clone(),
                            state: None,
                            stopped_reason: None,
                            next_at: Some(next.map(stamp)),
                            delivering_round: None,
                            last_round_at: None,
                            last_input_id: None,
                            round: None,
                        });
                        continue;
                    }
                    // A reading that did not settle the question skips this
                    // window and asks again at the next one: leaving `next_at`
                    // in the past would ask on every one-second pass, and a
                    // later `false` would deliver late inside the window.
                    Pending::Unknown => {
                        let next = record.cadence.next_window_at(now);
                        changes.push(Change {
                            loop_id: record.loop_id.clone(),
                            state: None,
                            stopped_reason: None,
                            next_at: Some(next.map(stamp)),
                            delivering_round: None,
                            last_round_at: None,
                            last_input_id: None,
                            round: None,
                        });
                        continue;
                    }
                    Pending::Gone(code) => {
                        changes.push(Change::settle(
                            &record.loop_id,
                            LoopState::Stopped,
                            Some(code),
                        ));
                        continue;
                    }
                    Pending::Consumed => {}
                }
            }
            // The round is persisted before any external effect, so a restart
            // settles it by lookup instead of writing it again (A.5).
            let marked = update(&self.shared.files, {
                let loop_id = record.loop_id.clone();
                move |loops| {
                    let Some(item) = loops.iter_mut().find(|item| item.loop_id == loop_id) else {
                        return Ok((false, ()));
                    };
                    let open = item.delivering_round.is_none()
                        && item.round == round
                        && item.state.is_live();
                    if !open {
                        return Ok((false, ()));
                    }
                    item.delivering_round = Some(round);
                    Ok((true, ()))
                }
            })
            .await;
            if marked.is_err() {
                continue;
            }
            let input = json!({
                "sessionId": record.session_id,
                "branchId": record.branch_id,
                "mode": "queue",
                "submissionId": submission(&record.loop_id, round),
                "rawBlocks": [{"text": marker(&record.loop_id, round)}],
                "content": [{"type": "text", "text": record.prompt}],
            });
            match sessions(self.shared.sessions.as_ref(), "input", input).await {
                Ok(Ok(receipt)) => {
                    let input_id = receipt
                        .get("inputId")
                        .and_then(|id| id.as_str())
                        .map(str::to_owned);
                    changes.push(Change::delivered(&record, round, input_id, now));
                }
                Ok(Err(code)) if crate::engine::target_gone(&code) => changes.push(Change::settle(
                    &record.loop_id,
                    LoopState::Stopped,
                    Some(code),
                )),
                // `reloading`, an uncertain reply and any other code leave the
                // round `delivering`; a later pass settles it by lookup with
                // the same submission id, which a branch accepts once.
                _ => {}
            }
        }

        let prune: Vec<String> = snapshot
            .iter()
            .filter(|record| record.state.is_terminal())
            .filter(|record| {
                record
                    .settled_at
                    .as_deref()
                    .and_then(canonical_instant)
                    .is_some_and(|at| at <= retention_cut)
            })
            .map(|record| record.loop_id.clone())
            .collect();
        self.settle(changes, prune).await
    }

    /// Settles every round a previous process left `delivering`, by lookup with
    /// that round's submission id (A.5). A definite answer settles it and
    /// nothing is ever written a second time; an unresolved answer leaves the
    /// record for the next pass.
    pub async fn settle_delivering(&self) -> Result<(), SchedulerError> {
        if !self.any_live() {
            return Ok(());
        }
        let Some(_activity) = self.shared.activity.take_hold() else {
            return Ok(());
        };
        let _hold = self.shared.loops.lock().await;
        let now = Utc::now();
        let snapshot = read(&self.shared.files).await?;
        let mut changes: Vec<Change> = Vec::new();
        for record in snapshot
            .iter()
            .filter(|record| record.delivering_round.is_some())
        {
            let Some(round) = record.delivering_round else {
                continue;
            };
            let lookup = json!({
                "sessionId": record.session_id,
                "branchId": record.branch_id,
                "submissionId": submission(&record.loop_id, round),
            });
            match sessions(self.shared.sessions.as_ref(), "lookup", lookup).await {
                Ok(Ok(value)) => {
                    if value.get("status").and_then(|s| s.as_str()) == Some("accepted") {
                        // The lookup names the round's input, so the settled
                        // record keeps the id a later window checks.
                        let input_id = value
                            .get("inputId")
                            .and_then(|id| id.as_str())
                            .map(str::to_owned);
                        changes.push(Change::delivered(record, round, input_id, now));
                    } else {
                        // The branch never accepted this round, so it is due
                        // again rather than lost.
                        changes.push(Change {
                            loop_id: record.loop_id.clone(),
                            state: None,
                            stopped_reason: None,
                            next_at: None,
                            delivering_round: Some(None),
                            last_round_at: None,
                            last_input_id: None,
                            round: None,
                        });
                    }
                }
                Ok(Err(code)) if crate::engine::target_gone(&code) => changes.push(Change::settle(
                    &record.loop_id,
                    LoopState::Stopped,
                    Some(code),
                )),
                _ => {}
            }
        }
        self.settle(changes, Vec::new()).await
    }

    /// Applies the pass's changes in one write and announces each record.
    async fn settle(&self, changes: Vec<Change>, prune: Vec<String>) -> Result<(), SchedulerError> {
        if changes.is_empty() && prune.is_empty() {
            return Ok(());
        }
        let announced = update(&self.shared.files, move |loops| {
            let mut touched: Vec<Loop> = Vec::new();
            for change in changes {
                let Some(record) = loops
                    .iter_mut()
                    .find(|record| record.loop_id == change.loop_id)
                else {
                    continue;
                };
                change.apply(record);
                record.validate().map_err(SchedulerError::invalid_data)?;
                touched.push(record.clone());
            }
            loops.retain(|record| !prune.contains(&record.loop_id));
            Ok((!touched.is_empty() || !prune.is_empty(), touched))
        })
        .await?;
        let live = read(&self.shared.files).await?;
        self.note_live(&live);
        for record in announced {
            self.notice(&record);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn every_minute() -> Cadence {
        Cadence::Every {
            every_seconds: 60,
            anchor: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    /// A fixed cadence arms the first window strictly after the pass's instant,
    /// so a record written just past a window never holds a deadline that is
    /// already due.
    #[test]
    fn a_fixed_cadence_arms_the_next_window_after_now() {
        let cadence = every_minute();
        assert_eq!(
            cadence.next_window_at(at("2026-01-01T00:01:00.100Z")),
            Some(at("2026-01-01T00:02:00.000Z")),
            "a fraction of a second past a window is inside that window"
        );
        assert_eq!(
            cadence.next_window_at(at("2026-01-01T00:01:00.000Z")),
            Some(at("2026-01-01T00:02:00.000Z")),
            "a window exactly now is not armed again"
        );
        assert_eq!(
            cadence.next_window_at(at("2026-01-01T00:00:00.000Z")),
            Some(at("2026-01-01T00:01:00.000Z"))
        );
        assert_eq!(
            cadence.next_window_at(at("2025-12-31T23:59:30.000Z")),
            Some(at("2026-01-01T00:00:00.000Z")),
            "before the anchor the anchor is the first window"
        );
        assert_eq!(
            cadence.next_window_at(at("2026-01-01T00:10:00.500Z")),
            Some(at("2026-01-01T00:11:00.000Z")),
            "ten missed windows are one round at the next window"
        );
    }

    /// A self-paced loop has no window.
    #[test]
    fn a_self_paced_loop_has_no_window() {
        assert_eq!(Cadence::SelfPaced.next_window_at(Utc::now()), None);
    }
}
