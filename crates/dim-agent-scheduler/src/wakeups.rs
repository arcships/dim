//! The wakeup file `scheduler/wakeups.json`: one inner v1 JSON document
//! inside the host's `{version, text}` envelope, kept by the same
//! [`PluginFileTransactions`] withLock and replace transactions as the run
//! history ([design §10, A.7]). A wakeup is a one-shot scheduled `input/message`
//! into an existing Session: it captures no policy, launches nothing, has no
//! deadline, and never appears in `schedule.list` or in the run history.
use crate::{
    document::{MAX_PROMPT_BYTES, canonical_instant, format_instant},
    engine::{GRACE_SECONDS, escaped, lookup_input, sessions, target_gone},
    methods::Shared,
    runs::SchedulerError,
};
use chrono::{DateTime, Duration, Utc};
use dim_agent_capabilities::session_broadcast::{SessionNotice, notify};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

/// The file inside the host-authorized `scheduler/` directory (A.7's scope).
pub const WAKEUPS_FILE: &str = "scheduler/wakeups.json";
const FORMAT_VERSION: u32 = 1;
/// A terminal record is removed once it has been terminal for this long
/// (§10.4): its only reader is `list`, and a record nobody listed is already
/// unrecoverable, so the bound costs no information the design promises.
const RETENTION_AFTER_TERMINAL: Duration = Duration::days(7);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeupState {
    Scheduled,
    Fired,
    Cancelled,
    Expired,
}

impl WakeupState {
    /// The wire name, as the file and `session.wakeup.list` spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Fired => "fired",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    /// Terminal states carry `settled_at` and are never fired.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Scheduled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wakeup {
    pub wakeup_id: String,
    pub session_id: String,
    pub branch_id: String,
    /// The canonical UTC instant the input is written (A.1 timestamp form).
    pub at: String,
    pub prompt: String,
    pub state: WakeupState,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
}

impl Wakeup {
    /// The record invariants of §10. A violation makes the whole file invalid,
    /// so the bytes stay unchanged.
    pub fn validate(&self) -> Result<(), String> {
        let timestamp = |text: &str| canonical_instant(text).is_some();
        if self.wakeup_id.is_empty()
            || self.session_id.is_empty()
            || self.branch_id.is_empty()
            || self.prompt.is_empty()
            || self.prompt.len() > MAX_PROMPT_BYTES
        {
            return Err(format!(
                "wakeup {} has an empty field or an oversized prompt",
                self.wakeup_id
            ));
        }
        if !timestamp(&self.at) || !timestamp(&self.created_at) {
            return Err(format!(
                "wakeup {} has a non-canonical timestamp",
                self.wakeup_id
            ));
        }
        if self.settled_at.is_some() != self.state.is_terminal() {
            return Err(format!("wakeup {} is inconsistent", self.wakeup_id));
        }
        if let Some(text) = &self.settled_at
            && !timestamp(text)
        {
            return Err(format!(
                "wakeup {} has a non-canonical settled_at",
                self.wakeup_id
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeupFile {
    version: u32,
    wakeups: Vec<Wakeup>,
}

/// Decodes the inner text of the file; `None` is a missing file, which holds
/// no wakeups.
fn decode(text: Option<&str>) -> Result<Vec<Wakeup>, SchedulerError> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let file: WakeupFile = serde_json::from_str(text)
        .map_err(|error| SchedulerError::invalid_data(format!("wakeups.json: {error}")))?;
    if file.version != FORMAT_VERSION {
        return Err(SchedulerError::invalid_data(format!(
            "wakeups.json has unknown version {}",
            file.version
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for wakeup in &file.wakeups {
        if !seen.insert(wakeup.wakeup_id.as_str()) {
            return Err(SchedulerError::invalid_data(format!(
                "wakeups.json repeats wakeup {}",
                wakeup.wakeup_id
            )));
        }
        wakeup.validate().map_err(SchedulerError::invalid_data)?;
    }
    Ok(file.wakeups)
}

fn encode(wakeups: Vec<Wakeup>) -> String {
    serde_json::to_string(&WakeupFile {
        version: FORMAT_VERSION,
        wakeups,
    })
    .expect("wakeup records contain only serializable fields")
}

/// Reads the records. A file that is not valid v1 is an error, not an empty
/// list.
pub async fn read(
    files: &dim_app_server::plugin_files::PluginFileTransactions,
) -> Result<Vec<Wakeup>, SchedulerError> {
    files
        .transact(WAKEUPS_FILE, |current| (None, decode(current)))
        .await?
}

/// Applies `change` to the records under the file lock. `change` returns
/// whether it modified them; an unmodified list is not written back.
async fn update<R: Send + 'static>(
    files: &dim_app_server::plugin_files::PluginFileTransactions,
    change: impl FnOnce(&mut Vec<Wakeup>) -> Result<(bool, R), SchedulerError> + Send + 'static,
) -> Result<R, SchedulerError> {
    files
        .transact(WAKEUPS_FILE, move |current| {
            let mut wakeups = match decode(current) {
                Ok(wakeups) => wakeups,
                Err(error) => return (None, Err(error)),
            };
            match change(&mut wakeups) {
                Ok((true, value)) => (Some(encode(wakeups)), Ok(value)),
                Ok((false, value)) => (None, Ok(value)),
                Err(error) => (None, Err(error)),
            }
        })
        .await?
}

fn stamp(instant: DateTime<Utc>) -> String {
    format_instant(instant)
}

/// The record the Session part asks the process owner to store.
pub struct WakeupRequest {
    pub session_id: String,
    pub branch_id: String,
    pub at: DateTime<Utc>,
    pub prompt: String,
}

/// The one new error code: cancel names no wakeup, or one of another Session.
const WAKEUP_NOT_FOUND: &str = "wakeup_not_found";

/// The process-side service the Session part forwards to (design §10): the
/// same shape as `ScheduleJobs`, keyed by the Session part with `names.key()`.
pub struct Wakeups {
    pub(crate) shared: Arc<Shared>,
}

impl Wakeups {
    /// Announces one committed record change on the target Session's channel
    /// (design §11.1). The notice reports that the record changed; `list` is
    /// its truth. A Session that is not loaded or not observed drops it.
    fn notice(&self, wakeup: &Wakeup) {
        notify(
            &self.shared.ctx,
            &wakeup.session_id,
            SessionNotice {
                source: "Scheduler".into(),
                kind: "wakeup".into(),
                data: json!({
                    "branchId": wakeup.branch_id,
                    "wakeupId": wakeup.wakeup_id,
                    "state": wakeup.state.name(),
                }),
            },
        );
    }

    /// Records a one-shot wakeup. An instant already past the grace window is
    /// recorded `expired` at once; nothing is ever replayed (A.4's rule).
    pub async fn create(&self, request: WakeupRequest) -> Result<Wakeup, SchedulerError> {
        let now = Utc::now();
        let expired = now - request.at >= Duration::seconds(GRACE_SECONDS);
        let created = Wakeup {
            wakeup_id: uuid::Uuid::new_v4().to_string(),
            session_id: request.session_id,
            branch_id: request.branch_id,
            at: stamp(request.at),
            prompt: request.prompt,
            state: if expired {
                WakeupState::Expired
            } else {
                WakeupState::Scheduled
            },
            created_at: stamp(now),
            settled_at: expired.then(|| stamp(now)),
        };
        created.validate().map_err(SchedulerError::invalid_data)?;
        let record = created.clone();
        let record = update(&self.shared.files, move |wakeups| {
            wakeups.push(created);
            Ok((true, record))
        })
        .await?;
        self.notice(&record);
        Ok(record)
    }

    /// The wakeups of one Session branch; the method boundary defaults
    /// `branch_id` to `main`. Newest first.
    pub async fn list(
        &self,
        session_id: &str,
        branch_id: &str,
    ) -> Result<Vec<Wakeup>, SchedulerError> {
        let mut own: Vec<Wakeup> = read(&self.shared.files)
            .await?
            .into_iter()
            .filter(|wakeup| wakeup.session_id == session_id && wakeup.branch_id == branch_id)
            .collect();
        own.sort_by(|a, b| b.at.cmp(&a.at).then(b.wakeup_id.cmp(&a.wakeup_id)));
        Ok(own)
    }

    /// Cancels a scheduled record. The reply is the record and whether this
    /// call settled it: a record that already fired, expired or was cancelled
    /// is returned with `false` instead of failing (§10) — cancellation is
    /// not an error after the fact.
    pub async fn cancel(
        &self,
        session_id: &str,
        wakeup_id: &str,
    ) -> Result<(Wakeup, bool), SchedulerError> {
        let _hold = self.shared.wakeups.lock().await;
        let session_id = session_id.to_owned();
        let wakeup_id = wakeup_id.to_owned();
        let (record, settled) = update(&self.shared.files, move |wakeups| {
            let Some(position) = wakeups
                .iter()
                .position(|wakeup| wakeup.wakeup_id == wakeup_id)
            else {
                return Err(SchedulerError::new(
                    WAKEUP_NOT_FOUND,
                    format!("no wakeup {wakeup_id}"),
                ));
            };
            if wakeups[position].session_id != session_id {
                return Err(SchedulerError::new(
                    WAKEUP_NOT_FOUND,
                    format!("no wakeup {wakeup_id}"),
                ));
            }
            let record = &mut wakeups[position];
            let settled = record.state == WakeupState::Scheduled;
            if settled {
                record.state = WakeupState::Cancelled;
                record.settled_at = Some(stamp(Utc::now()));
            }
            let record = record.clone();
            Ok((settled, (record, settled)))
        })
        .await?;
        // A cancel of a terminal record changes nothing, so it announces
        // nothing.
        if settled {
            self.notice(&record);
        }
        Ok((record, settled))
    }

    /// One pass of the timer (and of the restart, whose first pass runs
    /// within a second of startup): a `scheduled` record whose instant is
    /// within the grace window is fired once, one whose instant is past the
    /// window is `expired` and never replayed — unless the target already
    /// accepted its submission id, when it is `fired` instead, or the lookup
    /// answered nothing definite, when it stays `scheduled` for the next pass
    /// — one whose Session or branch is gone is `cancelled`, and a terminal
    /// record older than [`RETENTION_AFTER_TERMINAL`] is removed. A fire is
    /// only the input write through the public `sessions.input`; there is no
    /// launch, stop or withdraw (§10). The pass reads the file once, takes one
    /// clock reading, and writes at most once.
    ///
    /// The whole pass holds `shared.wakeups` — the snapshot read, every
    /// input write and the settle batch — so a cancel and a pass are one
    /// serial order at the record level: a cancel that takes the lock first
    /// is observed by the read (the record is already terminal, so no input
    /// is written), and a cancel that takes it after the pass reports the
    /// settled state and writes nothing (§10.3's boundary at the write).
    /// Nothing this pass awaits takes that lock again: `sessions.input` only
    /// queues an input on the target Session.
    pub async fn evaluate(&self) -> Result<(), SchedulerError> {
        let _hold = self.shared.wakeups.lock().await;
        let now = Utc::now();
        let snapshot = read(&self.shared.files).await?;
        let grace = Duration::seconds(GRACE_SECONDS);
        // Overdue records reach one Session in `at` order, which is the order
        // their prompts were due, with the record id as the stable tie-breaker
        // between records sharing an instant; the file's order is not a
        // contract (§10.3).
        let mut due: Vec<Wakeup> = snapshot
            .iter()
            .filter(|wakeup| wakeup.state == WakeupState::Scheduled)
            .filter(|wakeup| {
                canonical_instant(&wakeup.at).is_some_and(|at| at <= now && now - at < grace)
            })
            .cloned()
            .collect();
        due.sort_by(|a, b| a.at.cmp(&b.at).then(a.wakeup_id.cmp(&b.wakeup_id)));
        let stale: Vec<Wakeup> = snapshot
            .iter()
            .filter(|wakeup| wakeup.state == WakeupState::Scheduled)
            .filter(|wakeup| {
                canonical_instant(&wakeup.at).is_some_and(|at| at <= now && now - at >= grace)
            })
            .cloned()
            .collect();
        let retention_cut = now - RETENTION_AFTER_TERMINAL;
        let prune: Vec<String> = snapshot
            .iter()
            .filter(|wakeup| wakeup.state.is_terminal())
            .filter(|wakeup| {
                wakeup
                    .settled_at
                    .as_deref()
                    .and_then(canonical_instant)
                    .is_some_and(|at| at <= retention_cut)
            })
            .map(|wakeup| wakeup.wakeup_id.clone())
            .collect();
        let mut outcomes: Vec<(String, WakeupState)> = Vec::new();
        for wakeup in due {
            let session_id = wakeup.session_id.clone();
            let branch_id = wakeup.branch_id.clone();
            let marker = format!(
                "<dim-scheduler kind=\"wakeup\">{}</dim-scheduler>",
                escaped(&wakeup.wakeup_id)
            );
            let input = json!({
                "sessionId": session_id,
                "branchId": branch_id,
                "mode": "queue",
                "submissionId": format!("wakeup:{}", wakeup.wakeup_id),
                "rawBlocks": [{"text": marker}],
                "content": [{"type": "text", "text": wakeup.prompt}],
            });
            match sessions(self.shared.sessions.as_ref(), "input", input).await {
                Ok(Ok(_)) => {
                    let wakeup_id = wakeup.wakeup_id;
                    outcomes.push((wakeup_id.clone(), WakeupState::Fired));
                    // The test seam holds the pass after the input write and
                    // before the settle write, which is where a cancel must
                    // wait for the lock (§10.3); it does nothing in a build
                    // without tests.
                    #[cfg(test)]
                    crate::wakeup_pass::pause(&wakeup_id).await;
                }
                Ok(Err(code)) if target_gone(&code) => {
                    outcomes.push((wakeup.wakeup_id, WakeupState::Cancelled))
                }
                // `reloading`, transport failures and any other code leave the
                // record scheduled: the next pass retries with the same
                // submission id, which a branch accepts once, or the grace
                // window expires the record.
                _ => {}
            }
        }
        // A record past the grace may still have been delivered: the pass
        // publishes the input before it settles the record, so a process that
        // stops between the two leaves a `scheduled` record whose prompt is
        // already in the target's history. The submission id makes the check
        // exact — the same `sessions.lookup` the run path uses after an
        // uncertain write (A.5) — and a found input settles `fired` without
        // writing anything, so delivery stays exactly-once. Only an answer
        // that the target never accepted the submission expires the record,
        // and one that the target is gone settles it `cancelled`; a lookup
        // that answers neither leaves it scheduled, and the next pass repeats
        // it.
        let mut expired: Vec<String> = Vec::new();
        for wakeup in stale {
            let submission_id = format!("wakeup:{}", wakeup.wakeup_id);
            match lookup_input(
                &self.shared,
                &wakeup.session_id,
                &wakeup.branch_id,
                &submission_id,
            )
            .await
            {
                Ok(Some(_)) => outcomes.push((wakeup.wakeup_id, WakeupState::Fired)),
                Ok(None) => expired.push(wakeup.wakeup_id),
                // A target that is gone can never accept the submission, so
                // the answer is definite: settle the record `cancelled` as the
                // fire path does instead of repeating a lookup that cannot
                // succeed (§10.4).
                Err(code) if target_gone(&code) => {
                    outcomes.push((wakeup.wakeup_id, WakeupState::Cancelled))
                }
                // Any other failure says nothing about delivery, so the record
                // stays scheduled and the next pass repeats the check (§10.4).
                Err(_) => {}
            }
        }
        if outcomes.is_empty() && expired.is_empty() && prune.is_empty() {
            return Ok(());
        }
        let applied = update(&self.shared.files, move |wakeups| {
            let mut changed = false;
            let mut applied = Vec::new();
            for (wakeup_id, state) in &outcomes {
                let Some(record) = wakeups
                    .iter_mut()
                    .find(|wakeup| wakeup.wakeup_id == *wakeup_id)
                else {
                    continue;
                };
                if record.state != WakeupState::Scheduled {
                    continue;
                }
                record.state = *state;
                record.settled_at = Some(stamp(now));
                applied.push(record.clone());
                changed = true;
            }
            for wakeup_id in &expired {
                let Some(record) = wakeups
                    .iter_mut()
                    .find(|wakeup| wakeup.wakeup_id == *wakeup_id)
                else {
                    continue;
                };
                if record.state != WakeupState::Scheduled {
                    continue;
                }
                record.state = WakeupState::Expired;
                record.settled_at = Some(stamp(now));
                applied.push(record.clone());
                changed = true;
            }
            if !prune.is_empty() {
                // One hash per pass, not one linear scan per record: this runs
                // on the lock a cancel waits for and rewrites the whole file.
                let pruned: std::collections::HashSet<&str> =
                    prune.iter().map(String::as_str).collect();
                let before = wakeups.len();
                wakeups.retain(|wakeup| !pruned.contains(wakeup.wakeup_id.as_str()));
                changed |= wakeups.len() != before;
            }
            Ok((changed, applied))
        })
        .await?;
        // One notice per record this pass settled, sent after the settle
        // commits. A `fired` notice is a committed-state notice, not ordered
        // against the input's `session.event`, which the Session emits when
        // it dispatches the input; a client relates the two by the input's
        // submission id `wakeup:<wakeup_id>`. Retention pruning changes no
        // readable record and announces nothing.
        for record in &applied {
            self.notice(record);
        }
        Ok(())
    }
}
