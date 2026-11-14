//! Scheduled and manual deliveries (design §2.2, §3.1, A.4–A.6): windows, the
//! pre-effect record, a branch's input, a launch target's Session and the
//! stop timer that ends it, bounded lookup settling, and retention.
use crate::{
    document::{Job, Mode, Schedule, Target, canonical_instant},
    methods::{Shared, read_document, window_after},
    runs,
    runs::{Delivery, DeliveryState, SchedulerError, SessionOrigin, stamp},
};
use chrono::{DateTime, Duration, Utc};
use dim_agent_capabilities::settings::SettingsScope;
use dim_agent_types::{InputMode, PermissionPolicy, PermissionPreset, markup::escaped};
use dim_app_server::settings::SettingsError;
use rutis_bridge::session::{HostDispatch, settle};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration as StdDuration};
use tokio_util::sync::CancellationToken;

const POLL: StdDuration = StdDuration::from_secs(1);
/// The interval a failed stop or an undeletable Session is retried at (§3.2,
/// A.7).
const RETRY: StdDuration = StdDuration::from_secs(60);

const MANUAL: &str = "manual";
/// Terminal deliveries kept per job, newest first (A.7).
const RETAINED_TERMINAL_RUNS: usize = 100;
/// The most `sessions.lookup` answers one delivery may take (A.6).
pub(crate) const LOOKUP_BOUND: u32 = 5;
/// A delivery that stored the bound without settling it (A.6, D21).
const LOOKUP_BUDGET_EXHAUSTED: &str = "lookup_budget_exhausted";
/// A window skipped because the job's previous delivery is unsettled (§3.1).
pub(crate) const PREVIOUS_DELIVERY_UNSETTLED: &str = "previous_delivery_unsettled";

/// A Session service code that asks the caller to retry later: the Agents index or
/// the capability set is still loading after a restart (`reloading`, `capability reloading`).
/// The error a delivery task returns when the owner unloads mid-delivery. The
/// delivery stays `delivering` in the file, and the next generation's recovery
/// settles it by lookup.
pub(crate) const UNLOADING: &str = "service_reloading";

fn is_reloading(code: &str) -> bool {
    code.ends_with("reloading")
}

/// A reply that carries no fact about the write: the record stays
/// `delivering` and the lookup settles it (A.5, A.6).
pub(crate) fn uncertain(code: &str) -> bool {
    is_reloading(code) || matches!(code, "publication_unknown" | "ambiguous")
}

/// Applies the flow tests' seam to one write's reply (A.5, A.6): it spends the
/// armed flag on the next reply, whatever that reply is, so the seam cannot
/// leak into a later write; and it turns only an accepting reply into the
/// transport failure of a write that reached the target whose answer was
/// lost. Every other reply passes through as it came, so the seam never masks
/// a refusal the target gave. Feature `testing` only.
fn lose_input_reply(
    shared: &Shared,
    reply: Result<Result<Value, String>, String>,
) -> Result<Result<Value, String>, String> {
    #[cfg(feature = "testing")]
    {
        let armed = shared
            .holds
            .as_ref()
            .is_some_and(|holds| holds.take_lost_input_reply());
        if armed && matches!(&reply, Ok(Ok(_))) {
            return Err("input reply lost".to_owned());
        }
    }
    #[cfg(not(feature = "testing"))]
    {
        let _ = shared;
    }
    reply
}

/// The codes that say the target can never accept the input: its Session or
/// branch is gone. A `session` delivery settles `failed` on these and the job
/// is disabled with the reason (A.3); any other failure says nothing about
/// the target.
pub(crate) fn target_gone(code: &str) -> bool {
    matches!(code, "session_closed" | "branch_missing" | "branch_ended")
}

/// Whether the record carries the launch ownership A.7's deletion reads: a
/// launch origin that has not been released (A.7).
fn is_launched(run: &Delivery) -> bool {
    run.session_origin == Some(SessionOrigin::Launch) && !run.released
}
/// A window this far behind the clock is missed, not run (A.4). The wakeup
/// policy uses the same grace (design §10).
pub(crate) const GRACE_SECONDS: i64 = 60;

/// The marker both targets write before the saved prompt (§5).
fn marker(job: &Job, run_id: &str) -> String {
    format!(
        "<dim-scheduler kind=\"scheduled-run\" job=\"{}\" run=\"{}\">{}</dim-scheduler>",
        escaped(&job.id),
        escaped(run_id),
        escaped(&job.name)
    )
}

/// Records a window of the job and starts what the target does. A missed
/// window is recorded `missed`; a `session` window is recorded `delivering`
/// before its write; a `launch` window is recorded `delivering` with its stop
/// time before the launch, and its Session is added to that record once the
/// launch answers (§3.1, A.5).
async fn record(
    shared: &Arc<Shared>,
    job: &Job,
    window: String,
    missed: bool,
) -> Result<Option<Delivery>, SchedulerError> {
    let activity = shared
        .activity
        .take_hold()
        .ok_or_else(|| SchedulerError::new("service_reloading", "Process admission is paused"))?;
    let _settling = shared.settling.lock().await;
    let accepted = job.clone();
    let job = job.clone();
    let target = job
        .effective_target()
        .ok_or_else(|| SchedulerError::invalid_data("job has no target"))?;
    let spawn_target = target.clone();
    let now = shared.now();
    let run = runs::update(&shared.files, move |history| {
        if window != MANUAL
            && history
                .iter()
                .any(|run| run.job_id == job.id && run.window == window)
        {
            return Ok((false, None));
        }
        let (state, detail) = if missed {
            (DeliveryState::Missed, "missed")
        } else if history
            .iter()
            .any(|run| run.job_id == job.id && run.state == DeliveryState::Delivering)
        {
            (DeliveryState::Missed, PREVIOUS_DELIVERY_UNSETTLED)
        } else {
            (DeliveryState::Delivering, "")
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        // A `session` target names its branch here; a launch fixes its stop
        // time here, so launch and admission time count towards the limit,
        // and names its Session once `sessions.launch` answers (§3.2, A.5).
        let (session_id, branch_id) = match (&state, &target) {
            (
                DeliveryState::Delivering,
                Target::Session {
                    session_id,
                    branch_id,
                    ..
                },
            ) => (Some(session_id.clone()), Some(branch_id.clone())),
            _ => (None, None),
        };
        let deadline_at = match (&state, &target) {
            (
                DeliveryState::Delivering,
                Target::Launch {
                    max_run_minutes, ..
                },
            ) => Some(stamp(now + Duration::minutes(i64::from(*max_run_minutes)))),
            _ => None,
        };
        let run = Delivery {
            run_id: run_id.clone(),
            job_id: job.id.clone(),
            window,
            state,
            deadline_at,
            session_id,
            branch_id,
            session_origin: None,
            policy: None,
            input_event_id: None,
            submission_id: format!("scheduler:{run_id}"),
            report_to: None,
            created_at: stamp(now),
            ended_at: state.is_terminal().then(|| stamp(now)),
            detail: detail.into(),
            attempts: None,
            released: false,
            job_revision: Some(job.revision),
        };
        run.validate().map_err(SchedulerError::invalid_data)?;
        history.push(run.clone());
        Ok((true, Some(run)))
    })
    .await?;
    if let Some(run) = &run {
        shared.watch.run(run);
    }
    if let Some(run) = &run
        && run.state == DeliveryState::Delivering
    {
        let activity = Arc::new(activity);
        shared
            .deliveries
            .lock()
            .expect("delivery admission")
            .insert(run.run_id.clone(), activity.clone());
        let owner = shared.clone();
        let run = run.clone();
        shared.spawn(async move {
            let _activity = activity;
            match spawn_target {
                Target::Session { .. } => deliver(owner, accepted, run).await,
                Target::Launch { .. } => execute(owner, accepted, run).await,
            }
        });
    }
    if run.is_some() {
        shared.spawn(prune(shared.clone()));
    }
    Ok(run)
}

/// Accepts a manual window of the job (`schedule.run_now`). It records its
/// own delivery, with its own `run_id` and `window`, and never returns the
/// timer path's record (§3.1, A.3).
pub async fn accept(shared: Arc<Shared>, job_id: &str) -> Result<Delivery, SchedulerError> {
    let cancel = CancellationToken::new();
    let document = read_document(&shared, &cancel).await?.document;
    let job = document
        .jobs
        .iter()
        .find(|job| job.id == job_id)
        .cloned()
        .ok_or_else(|| SchedulerError::new("schedule_not_found", format!("no job {job_id}")))?;
    record(&shared, &job, MANUAL.into(), false)
        .await?
        .ok_or_else(|| SchedulerError::invalid_data("a manual run is always recorded"))
}

/// One pass of the timer (A.4). When a job's next window is due, only its
/// latest due window is considered: it starts within 60 s of its instant,
/// and otherwise one `missed` record covers the gap. An `at` job is disabled
/// once its instant has passed.
pub async fn tick(shared: &Arc<Shared>) -> Result<(), SchedulerError> {
    let Some(_activity) = shared.activity.take_hold() else {
        return Ok(());
    };
    let cancel = CancellationToken::new();
    let document = read_document(shared, &cancel).await?.document;
    let history = runs::read(&shared.files).await?;
    let now = shared.now();
    for job in document.jobs.iter().filter(|job| job.enabled) {
        // A disable whose write did not land is still required state: read
        // the claim back from the job's newest terminal delivery (A.3).
        if let Some(run) = latest_terminal(&history, &job.id)
            && run.state == DeliveryState::Failed
            && target_gone(&run.detail)
            && claim_current(job, run)
        {
            let at = run
                .ended_at
                .clone()
                .unwrap_or_else(|| run.created_at.clone());
            disable_gone(shared, run, &run.detail, at).await;
            continue;
        }
        let Ok(next) = job.next_window(window_after(shared, job, &history)) else {
            continue;
        };
        if let Some(due) = next
            && due <= now
        {
            let latest = job
                .latest_window(now)
                .ok()
                .flatten()
                .unwrap_or(due)
                .max(due);
            let missed = now - latest >= Duration::seconds(GRACE_SECONDS);
            record(shared, job, stamp(latest), missed).await?;
            continue;
        }
        if matches!(job.schedule, Schedule::At(_))
            && next.is_none()
            && !history
                .iter()
                .any(|run| run.job_id == job.id && !run.state.is_terminal())
        {
            disable(shared, &job.id).await?;
        }
    }
    Ok(())
}

/// Disables a job whose `at` instant has passed. A conflicting write is left
/// for the next tick, which repeats the check.
async fn disable(shared: &Arc<Shared>, job_id: &str) -> Result<(), SchedulerError> {
    let cancel = CancellationToken::new();
    let snapshot = read_document(shared, &cancel).await?;
    let mut document = snapshot.document;
    let Some(job) = document
        .jobs
        .iter_mut()
        .find(|job| job.id == job_id && job.enabled)
    else {
        return Ok(());
    };
    job.enabled = false;
    job.enabled_at = None;
    job.revision += 1;
    match shared
        .settings
        .update(SettingsScope::User, &snapshot.revision, document, &cancel)
        .await
    {
        Ok(_) | Err(SettingsError::Conflict) => Ok(()),
        Err(error) => Err(SchedulerError::new(
            "service_unavailable",
            error.to_string(),
        )),
    }
}

/// Whether `run` names the Session branch `job` delivers to now: the claim a
/// target-gone failure disables the job under, which a save that replaces
/// the target ends.
fn same_target(job: &Job, run: &Delivery) -> bool {
    match job.effective_target() {
        Some(Target::Session {
            session_id,
            branch_id,
            ..
        }) => {
            run.session_id.as_deref() == Some(session_id.as_str())
                && run.branch_id.as_deref() == Some(branch_id.as_str())
        }
        _ => false,
    }
}

/// The job's newest terminal delivery, which the timer pass reads a
/// target-gone claim back from.
fn latest_terminal<'a>(history: &'a [Delivery], job_id: &str) -> Option<&'a Delivery> {
    history
        .iter()
        .filter(|run| run.job_id == job_id && run.state.is_terminal())
        .max_by(|a, b| a.ended_at.cmp(&b.ended_at).then(a.run_id.cmp(&b.run_id)))
}

/// Whether `run`'s target-gone claim still applies to `job` as read for the
/// write: the job is enabled, still names the delivery's effective target,
/// and carries the revision the delivery was due under (A.3). A record
/// without the revision cannot prove its claim is current and never disables.
fn claim_current(job: &Job, run: &Delivery) -> bool {
    job.enabled && same_target(job, run) && run.job_revision == Some(job.revision)
}

/// A `session` delivery settled `failed` with a target-gone code disables the
/// job and stores why (A.3). The write is required state, not a note: it
/// decides on the document it is about to change, on every attempt including
/// a CAS retry, and disables only while the claim is current; any other
/// failure leaves the claim to the timer pass, which reads it back from the
/// delivery record.
async fn disable_gone(shared: &Arc<Shared>, run: &Delivery, code: &str, at: String) {
    for attempt in 0..3 {
        let cancel = CancellationToken::new();
        let Ok(snapshot) = read_document(shared, &cancel).await else {
            return;
        };
        #[cfg(feature = "testing")]
        if let Some(holds) = &shared.holds {
            holds.before_disable_write().await;
        }
        let mut document = snapshot.document;
        let Some(job) = document.jobs.iter_mut().find(|job| job.id == run.job_id) else {
            return;
        };
        if !claim_current(job, run) {
            return;
        }
        job.enabled = false;
        job.enabled_at = None;
        job.disabled_reason = Some(crate::document::DisabledReason {
            code: code.into(),
            at: at.clone(),
        });
        job.revision += 1;
        match shared
            .settings
            .update(SettingsScope::User, &snapshot.revision, document, &cancel)
            .await
        {
            Ok(_) => return,
            Err(SettingsError::Conflict) if attempt < 2 => continue,
            Err(_) => return,
        }
    }
}

/// The authority a launch target requests: its stored preset, which
/// `sessions.launch` caps against the live host bound. A job without a preset
/// requests read-only. The preset comes from the effective target: a job's
/// top-level legacy `policy` never applies once the job has a target of its
/// own.
pub(crate) fn requested_policy(policy: Option<PermissionPreset>) -> PermissionPolicy {
    match policy {
        Some(preset) => PermissionPolicy {
            preset,
            ..PermissionPolicy::default()
        },
        None => PermissionPolicy {
            preset: PermissionPreset::ReadOnly,
            ..PermissionPolicy::default()
        },
    }
}

/// Writes one delivery's change under the file lock, and announces the state
/// the record took when the change moved it.
async fn update_run(
    shared: &Shared,
    run_id: &str,
    change: impl FnOnce(&mut Delivery) + Send + 'static,
) -> Result<(), SchedulerError> {
    let run_id = run_id.to_owned();
    let moved = std::sync::Arc::new(std::sync::Mutex::new(None));
    let observe = moved.clone();
    runs::update(&shared.files, move |history| {
        let run = history
            .iter_mut()
            .find(|run| run.run_id == run_id)
            .ok_or_else(|| SchedulerError::invalid_data(format!("run {run_id} is missing")))?;
        let before = run.state;
        change(run);
        run.validate().map_err(SchedulerError::invalid_data)?;
        if run.state != before {
            *observe.lock().unwrap() = Some(run.clone());
        }
        Ok((true, ()))
    })
    .await?;
    if let Some(run) = moved.lock().unwrap().clone() {
        shared.watch.run(&run);
    }
    Ok(())
}

async fn record_session(
    shared: &Shared,
    run_id: &str,
    session_id: String,
    policy: PermissionPolicy,
) -> Result<(), SchedulerError> {
    update_run(shared, run_id, move |run| {
        run.session_id = Some(session_id);
        run.branch_id = Some("main".into());
        run.session_origin = Some(SessionOrigin::Launch);
        run.policy = Some(policy);
    })
    .await
}

/// Settles a delivery to a terminal state once. A delivery that already
/// settled keeps its outcome.
async fn finish(
    shared: &Arc<Shared>,
    run_id: &str,
    state: DeliveryState,
    detail: String,
) -> Result<(), SchedulerError> {
    let run_id = run_id.to_owned();
    let now = shared.now();
    let settled = runs::update(&shared.files, move |history| {
        let run = history
            .iter_mut()
            .find(|run| run.run_id == run_id)
            .ok_or_else(|| SchedulerError::invalid_data(format!("run {run_id} is missing")))?;
        if run.state.is_terminal() {
            return Ok((false, None));
        }
        run.state = state;
        run.ended_at = Some(stamp(now));
        run.detail = detail;
        run.validate().map_err(SchedulerError::invalid_data)?;
        Ok((true, Some(run.clone())))
    })
    .await?;
    if let Some(run) = settled {
        shared.watch.run(&run);
        shared
            .deliveries
            .lock()
            .expect("delivery admission")
            .remove(&run.run_id);
        shared.spawn(prune(shared.clone()));
    }
    Ok(())
}

/// Whether a launch record's stop time has passed, so its stop timer has
/// finished and its record is eligible for cleanup (§3.2, A.7). A `session`
/// delivery has no stop time and nothing to wait for.
fn stop_time_passed(run: &Delivery, now: DateTime<Utc>) -> bool {
    match run.deadline_at.as_deref().and_then(canonical_instant) {
        Some(deadline) => now >= deadline,
        None => true,
    }
}

/// Retention (A.7): keeps the newest terminal deliveries of each job, every
/// unsettled delivery, and the launch record of each Session that a kept
/// record or a legacy reuse job still references. Expired launch-owned
/// Sessions are deleted first, and their records stay when the delete fails,
/// so the next pass retries.
pub async fn prune(shared: Arc<Shared>) {
    let Some(_activity) = shared.activity.take_hold() else {
        shared
            .retention
            .store(true, std::sync::atomic::Ordering::Relaxed);
        return;
    };
    let _pass = shared.pruning.lock().await;
    let pending = match prune_once(&shared).await {
        Ok(pending) => pending,
        Err(error) => {
            tracing::warn!(%error, "scheduler retention failed");
            true
        }
    };
    shared
        .retention
        .store(pending, std::sync::atomic::Ordering::Relaxed);
    // A pass that took no write still signals: a flow test that asserts a
    // release did not happen waits on "the pass ended", not on a change.
    #[cfg(feature = "testing")]
    if let Some(signals) = &shared.signals {
        signals.retention_done();
    }
}

/// One retention pass (A.7). Returns whether expired records remain, so the
/// timer retries.
async fn prune_once(shared: &Arc<Shared>) -> Result<bool, SchedulerError> {
    let cancel = CancellationToken::new();
    let runs = runs::read(&shared.files).await?;
    let document = read_document(shared, &cancel).await?.document;
    // A job whose stored target replaced the Session its legacy `reuse`
    // binding names releases that binding's launch ownership, and a pass
    // re-derives it from the document so a save whose own release write
    // failed heals here (§2.3, A.7).
    let releases: HashSet<(String, String)> = document
        .jobs
        .iter()
        .filter(|job| job.replaced_reuse_binding())
        .filter_map(|job| {
            job.session_id
                .clone()
                .map(|session| (job.id.clone(), session))
        })
        .collect();
    let retire: HashSet<String> = runs
        .iter()
        .filter(|run| {
            run.session_origin == Some(SessionOrigin::Launch)
                && run.session_id.as_ref().is_some_and(|session| {
                    releases.contains(&(run.job_id.clone(), session.clone()))
                })
        })
        .map(|run| run.run_id.clone())
        .collect();
    let mut keep: HashSet<String> = runs
        .iter()
        .filter(|run| !run.state.is_terminal() || !stop_time_passed(run, shared.now()))
        .map(|run| run.run_id.clone())
        .collect();
    for job in &document.jobs {
        let mut own: Vec<&Delivery> = runs
            .iter()
            .filter(|run| run.job_id == job.id && run.state.is_terminal())
            .collect();
        own.sort_by(|a, b| b.ended_at.cmp(&a.ended_at).then(b.run_id.cmp(&a.run_id)));
        keep.extend(
            own.iter()
                .take(RETAINED_TERMINAL_RUNS)
                .map(|run| run.run_id.clone()),
        );
    }
    // A legacy `reuse` binding is a reference only while the job has no
    // target of its own: a save that changed the target releases the
    // launched Session it used to own (§2.3, A.7).
    let mut references: HashSet<String> = document
        .jobs
        .iter()
        .filter(|job| job.target.is_none() && job.mode == Some(Mode::Reuse))
        .filter_map(|job| job.session_id.clone())
        .collect();
    references.extend(
        document
            .jobs
            .iter()
            .filter_map(|job| job.effective_target())
            .filter_map(|target| match target {
                Target::Session { session_id, .. } => Some(session_id),
                Target::Launch { .. } => None,
            }),
    );
    references.extend(
        runs.iter()
            .filter(|run| keep.contains(&run.run_id))
            .filter_map(|run| run.session_id.clone()),
    );
    for session_id in &references {
        let owner = runs
            .iter()
            .filter(|run| run.session_id.as_deref() == Some(session_id) && is_launched(run))
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then(a.run_id.cmp(&b.run_id))
            });
        if let Some(owner) = owner {
            keep.insert(owner.run_id.clone());
        }
    }
    let expired: Vec<&Delivery> = runs
        .iter()
        .filter(|run| !keep.contains(&run.run_id))
        .collect();
    let deletions: HashSet<String> = expired
        .iter()
        .filter(|run| is_launched(run) && !retire.contains(&run.run_id))
        .filter_map(|run| run.session_id.clone())
        .filter(|session_id| !references.contains(session_id))
        .collect();
    let mut pending = false;
    for session_id in deletions {
        match sessions(
            shared.sessions.as_ref(),
            "delete",
            json!({"sessionId": session_id}),
        )
        .await
        {
            Ok(Ok(_)) => {}
            _ => {
                pending = true;
                keep.extend(
                    expired
                        .iter()
                        .filter(|run| {
                            is_launched(run)
                                && !retire.contains(&run.run_id)
                                && run.session_id.as_deref() == Some(session_id.as_str())
                        })
                        .map(|run| run.run_id.clone()),
                );
            }
        }
    }
    let remove: HashSet<String> = runs
        .iter()
        .filter(|run| !keep.contains(&run.run_id))
        .map(|run| run.run_id.clone())
        .collect();
    if !remove.is_empty() || !retire.is_empty() {
        runs::update(&shared.files, move |history| {
            let before = history.len();
            let released = runs::retire_launch_ownership(history, &retire);
            history.retain(|run| !remove.contains(&run.run_id));
            Ok((released || history.len() != before, ()))
        })
        .await?;
    }
    Ok(pending)
}

/// Starts the stop timer of every launch delivery this process inherits
/// (§3.2). A record written before this change in `launching`, `submitted` or
/// `running` is left as written: nothing settles it and its Session is not
/// stopped, because a stop timer belongs to the record a window of this
/// design wrote (A.7). A launch this design wrote keeps its stop timer in any
/// state: a delivery that settled `lost` or `failed` after the launch still
/// owns the Session its stop time ends. Nothing is launched or submitted
/// again.
pub fn recover(shared: &Arc<Shared>, launches: Vec<Delivery>) {
    for run in launches {
        let Some(deadline) = run.deadline_at.as_deref().and_then(canonical_instant) else {
            continue;
        };
        let Some(session_id) = run.session_id.clone() else {
            continue;
        };
        let owner = shared.clone();
        shared.spawn(async move {
            stop_at(owner, run.run_id, session_id, deadline).await;
        });
    }
}

/// Stops a launched Session at its stop time, and keeps retrying once a
/// minute while the call fails and the record still exists (§3.2, A.7). The
/// delivery's state is not touched: a stop is a bound on the launched work,
/// not a run outcome.
async fn stop_at(shared: Arc<Shared>, run_id: String, session_id: String, deadline: DateTime<Utc>) {
    #[cfg(feature = "testing")]
    if let Some(signals) = &shared.signals {
        signals.stop_parked(deadline);
    }
    if !shared.pause_until(deadline).await {
        return;
    }
    loop {
        let Some(_activity) = shared.activity.take_hold() else {
            if !shared.pause(std::time::Duration::from_secs(1)).await {
                return;
            }
            continue;
        };
        if !record_exists(&shared, &run_id).await {
            ended(&shared);
            return;
        }
        match sessions(
            shared.sessions.as_ref(),
            "stop",
            json!({"sessionId": session_id, "branchId": "main",
                "reason": "scheduled run reached its stop time"}),
        )
        .await
        {
            Ok(Ok(_)) => {
                ended(&shared);
                return;
            }
            Ok(Err(code)) if target_gone(&code) => {
                ended(&shared);
                return;
            }
            _ => {
                if !shared.pause(RETRY).await {
                    return;
                }
            }
        }
    }
}

/// Publishes that a stop timer ended, whichever check ended it. Feature
/// `testing` only: a flow row that asserts the timer wrote nothing waits on
/// "the timer ended" rather than on a bound.
fn ended(shared: &Shared) {
    #[cfg(feature = "testing")]
    if let Some(signals) = &shared.signals {
        signals.stop_done();
    }
    #[cfg(not(feature = "testing"))]
    let _ = shared;
}

/// Whether the delivery's record is still in the store, which bounds the stop
/// timer: A.7 words it as running "for as long as the record exists". A store
/// that cannot be read is no evidence of removal, so the timer keeps going.
async fn record_exists(shared: &Shared, run_id: &str) -> bool {
    runs::read(&shared.files)
        .await
        .map(|history| history.iter().any(|run| run.run_id == run_id))
        .unwrap_or(true)
}

/// `Ok(None)` when the Session never accepted the submission. A service error
/// other than `reloading` is returned as its code; a transport failure or a
/// reloading Session is retried until it answers. The launch path resolves an
/// uncertain write with it, and the wakeup pass settles a record past the
/// grace with the same exact submission-id check (design §10.4).
pub(crate) async fn lookup_input(
    shared: &Arc<Shared>,
    session_id: &str,
    branch_id: &str,
    submission_id: &str,
) -> Result<Option<(String, u64)>, String> {
    let args =
        json!({"sessionId": session_id, "branchId": branch_id, "submissionId": submission_id});
    loop {
        match sessions(shared.sessions.as_ref(), "lookup", args.clone()).await {
            Ok(Ok(value)) => {
                return match value["status"].as_str() {
                    Some("accepted") => {
                        match (value["inputId"].as_str(), value["sequence"].as_u64()) {
                            (Some(input_id), Some(sequence)) => {
                                Ok(Some((input_id.to_owned(), sequence)))
                            }
                            _ => Err("invalid_data".to_owned()),
                        }
                    }
                    _ => Ok(None),
                };
            }
            Ok(Err(code)) if !is_reloading(&code) => return Err(code),
            Ok(Err(_)) | Err(_) => {
                if !shared.pause(POLL).await {
                    return Err(UNLOADING.to_owned());
                }
            }
        }
    }
}

/// A `sessions` call. The outer error is a transport failure, whose outcome
/// is unknown; the inner error is the service's own code.
pub(crate) async fn sessions(
    service: &dyn HostDispatch,
    method: &str,
    args: Value,
) -> Result<Result<Value, String>, String> {
    let call = service
        .invoke(method, json!([args]).into())
        .map_err(|error| error.to_string())?;
    let envelope = settle(call)
        .await
        .map_err(|error| error.to_string())?
        .json()
        .map_err(|error| error.to_string())?;
    if envelope["ok"] == json!(true) {
        Ok(Ok(envelope["value"].clone()))
    } else {
        Ok(Err(envelope["error"]["code"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned()))
    }
}

/// Writes one `session` delivery: one input on the target branch, with the
/// marker and the saved prompt in the job's mode (§3.1, A.5).
async fn deliver(shared: Arc<Shared>, job: Job, run: Delivery) {
    let Some(Target::Session { session_id, .. }) = job.effective_target() else {
        let _ = finish(
            &shared,
            &run.run_id,
            DeliveryState::Lost,
            "a session delivery names a non-session target".into(),
        )
        .await;
        return;
    };
    if let Err(error) = write_input(&shared, &job, &run, &session_id).await
        && error.code != UNLOADING
    {
        let _ = finish(&shared, &run.run_id, DeliveryState::Lost, error.message).await;
    }
}

/// Settles a `session` delivery `failed` with the code; a target-gone code
/// also disables the job (A.3, A.6).
async fn failed(shared: &Arc<Shared>, run: &Delivery, code: &str) -> Result<(), SchedulerError> {
    if target_gone(code) {
        return settle_target_gone(shared, run, code).await;
    }
    finish(shared, &run.run_id, DeliveryState::Failed, code.into()).await
}

/// The one rule a target-gone answer takes on both paths: the delivery
/// settles `failed` and the job is disabled with the code (A.3). The write
/// path reaches it through `failed`; the lookup path calls it directly.
async fn settle_target_gone(
    shared: &Arc<Shared>,
    run: &Delivery,
    code: &str,
) -> Result<(), SchedulerError> {
    finish(shared, &run.run_id, DeliveryState::Failed, code.into()).await?;
    disable_gone(shared, run, code, stamp(shared.now())).await;
    Ok(())
}

/// One settling pass over every `delivering` record: recovery at start and
/// the timer's once-a-minute retry (A.6). Each record gets at most one lookup
/// attempt per pass; a record whose stored count is already at the bound
/// settles `lost` without one (D21).
pub async fn settle_delivering(shared: &Arc<Shared>) {
    let Some(_activity) = shared.activity.take_hold() else {
        return;
    };
    let _pass = shared.settling.lock().await;
    let deliveries = match runs::read(&shared.files).await {
        Ok(deliveries) => deliveries,
        Err(error) => {
            tracing::warn!(%error, "scheduler lookup pass failed");
            return;
        }
    };
    for run in deliveries
        .iter()
        .filter(|run| run.state == DeliveryState::Delivering)
    {
        lookup_now(shared, &run.run_id).await;
    }
}

/// The delivery task's own settling after an uncertain input reply: one
/// pass, so a healthy service settles the record now.
enum LookupStep {
    /// The stored count was already at the bound; the record settled `lost`.
    Exhausted(Box<Delivery>),
    /// This pass persisted the next attempt on the record it returns, whose
    /// identity is the one the lookup uses; `attempt` is the count it wrote.
    Attempt { run: Box<Delivery>, attempt: u32 },
    /// The record settled or moved meanwhile; nothing to look up.
    Settled,
}

/// One lookup attempt for one `delivering` record (A.6): persist the
/// incremented `attempts` before the lookup, settle `delivered` on
/// `accepted`, and settle `lost` on the fifth answer without a fact. The
/// lookup's target is the identity the record holds under this update's lock,
/// not the caller's snapshot: a launch persists its Session after the reply
/// and before the input, so the caller's own copy may predate it.
async fn lookup_once(shared: &Arc<Shared>, run_id: &str) -> Result<(), SchedulerError> {
    let found = run_id.to_owned();
    let now = shared.now();
    let step = runs::update(&shared.files, move |history| {
        let Some(run) = history.iter_mut().find(|run| run.run_id == found) else {
            return Ok((false, LookupStep::Settled));
        };
        if run.state != DeliveryState::Delivering {
            return Ok((false, LookupStep::Settled));
        }
        let attempts = run.attempts.unwrap_or(0);
        if attempts >= LOOKUP_BOUND {
            run.state = DeliveryState::Lost;
            run.ended_at = Some(stamp(now));
            run.detail = LOOKUP_BUDGET_EXHAUSTED.into();
            run.validate().map_err(SchedulerError::invalid_data)?;
            return Ok((true, LookupStep::Exhausted(Box::new(run.clone()))));
        }
        run.attempts = Some(attempts + 1);
        run.validate().map_err(SchedulerError::invalid_data)?;
        Ok((
            true,
            LookupStep::Attempt {
                run: Box::new(run.clone()),
                attempt: attempts + 1,
            },
        ))
    })
    .await?;
    match step {
        LookupStep::Settled => {
            shared
                .deliveries
                .lock()
                .expect("delivery admission")
                .remove(run_id);
            Ok(())
        }
        LookupStep::Exhausted(settled) => {
            shared.watch.run(&settled);
            shared
                .deliveries
                .lock()
                .expect("delivery admission")
                .remove(run_id);
            shared.spawn(prune(shared.clone()));
            Ok(())
        }
        LookupStep::Attempt { run, attempt } => {
            let (Some(session_id), Some(branch_id)) =
                (run.session_id.clone(), run.branch_id.clone())
            else {
                // A launch whose reply never arrived names no Session: no
                // lookup can reach it, so it is `lost` at the first attempt
                // (Appendix F).
                return settle_lost(shared, &run.run_id, "launch_reply_unknown".into()).await;
            };
            let answer = match sessions(
                shared.sessions.as_ref(),
                "lookup",
                json!({"sessionId": session_id, "branchId": branch_id,
                    "submissionId": run.submission_id}),
            )
            .await
            {
                Ok(Ok(value)) if value["status"] == json!("accepted") => {
                    let input_id = value["inputId"].as_str().ok_or_else(|| {
                        SchedulerError::invalid_data("lookup reply has no inputId")
                    })?;
                    return settle_delivered(shared, &run.run_id, input_id).await;
                }
                Ok(Ok(_)) => "unknown".to_owned(),
                Ok(Err(code)) if target_gone(&code) => {
                    return settle_target_gone(shared, &run, &code).await;
                }
                Ok(Err(code)) => code,
                Err(_) => "transport".to_owned(),
            };
            if attempt >= LOOKUP_BOUND {
                return settle_lost(shared, &run.run_id, answer).await;
            }
            Ok(())
        }
    }
}

/// Settles a `delivering` delivery `delivered` with its receipt.
async fn settle_delivered(
    shared: &Arc<Shared>,
    run_id: &str,
    input_id: &str,
) -> Result<(), SchedulerError> {
    let run_id = run_id.to_owned();
    let input_id = input_id.to_owned();
    let now = shared.now();
    let moved = std::sync::Arc::new(std::sync::Mutex::new(None));
    let observe = moved.clone();
    runs::update(&shared.files, move |history| {
        let run = history
            .iter_mut()
            .find(|run| run.run_id == run_id)
            .ok_or_else(|| SchedulerError::invalid_data(format!("run {run_id} is missing")))?;
        if run.state != DeliveryState::Delivering {
            return Ok((false, ()));
        }
        run.state = DeliveryState::Delivered;
        run.input_event_id = Some(input_id);
        run.ended_at = Some(stamp(now));
        run.validate().map_err(SchedulerError::invalid_data)?;
        *observe.lock().unwrap() = Some(run.clone());
        Ok((true, ()))
    })
    .await?;
    if let Some(run) = moved.lock().unwrap().clone() {
        shared.watch.run(&run);
        shared
            .deliveries
            .lock()
            .expect("delivery admission")
            .remove(&run.run_id);
        shared.spawn(prune(shared.clone()));
    }
    Ok(())
}

/// Settles a `delivering` delivery `lost` with the answer that left it
/// undetermined (A.6).
async fn settle_lost(
    shared: &Arc<Shared>,
    run_id: &str,
    answer: String,
) -> Result<(), SchedulerError> {
    let run_id = run_id.to_owned();
    let now = shared.now();
    let moved = std::sync::Arc::new(std::sync::Mutex::new(None));
    let observe = moved.clone();
    runs::update(&shared.files, move |history| {
        let run = history
            .iter_mut()
            .find(|run| run.run_id == run_id)
            .ok_or_else(|| SchedulerError::invalid_data(format!("run {run_id} is missing")))?;
        if run.state != DeliveryState::Delivering {
            return Ok((false, ()));
        }
        run.state = DeliveryState::Lost;
        run.ended_at = Some(stamp(now));
        run.detail = answer;
        run.validate().map_err(SchedulerError::invalid_data)?;
        *observe.lock().unwrap() = Some(run.clone());
        Ok((true, ()))
    })
    .await?;
    if let Some(run) = moved.lock().unwrap().clone() {
        shared.watch.run(&run);
        shared
            .deliveries
            .lock()
            .expect("delivery admission")
            .remove(&run.run_id);
        shared.spawn(prune(shared.clone()));
    }
    Ok(())
}

/// Runs one launch window: one launch, one input, and the stop timer that
/// ends the Session at its deadline (§3.1, §3.2, A.5).
async fn execute(shared: Arc<Shared>, job: Job, run: Delivery) {
    #[cfg(feature = "testing")]
    if let Some(holds) = &shared.holds {
        holds.before_launch().await;
    }
    if let Err(error) = launch_target(&shared, &job, &run).await
        && error.code != UNLOADING
    {
        let _ = finish(&shared, &run.run_id, DeliveryState::Lost, error.message).await;
    }
}

/// Launches the job's Session with the job's preset, records the Session on
/// the delivery, starts its stop timer and writes the input. The preset is
/// the job's own: `sessions.launch` caps it against the live host bound, and
/// the reply's policy is what the delivery records (§3.3).
async fn launch_target(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Delivery,
) -> Result<(), SchedulerError> {
    let Some(Target::Launch {
        cwd,
        agent_id,
        policy,
        ..
    }) = job.effective_target()
    else {
        return Err(SchedulerError::invalid_data(
            "a launch delivery names a non-launch target",
        ));
    };
    // The launch declares what the Session is for and the label a picker
    // shows: the job's own name (#1421).
    let mut args = json!({"cwd": cwd, "policy": requested_policy(policy), "origin": "automation", "title": job.name});
    if let Some(agent) = agent_id.as_ref() {
        args["agent"] = json!(agent);
    }
    let launched = match sessions(shared.sessions.as_ref(), "launch", args).await {
        Err(_) => {
            return finish(
                shared,
                &run.run_id,
                DeliveryState::Lost,
                "launch_reply_unknown".into(),
            )
            .await;
        }
        Ok(Err(code)) => return finish(shared, &run.run_id, DeliveryState::Failed, code).await,
        Ok(Ok(value)) => value,
    };
    let session_id = launched["sessionId"]
        .as_str()
        .ok_or_else(|| SchedulerError::invalid_data("launch reply has no sessionId"))?
        .to_owned();
    let received: PermissionPolicy = serde_json::from_value(launched["policy"].clone())
        .map_err(|error| SchedulerError::invalid_data(error.to_string()))?;
    record_session(shared, &run.run_id, session_id.clone(), received).await?;
    let deadline = run
        .deadline_at
        .as_deref()
        .and_then(canonical_instant)
        .ok_or_else(|| SchedulerError::invalid_data("a launch delivery has no stop time"))?;
    let owner = shared.clone();
    let stopped = session_id.clone();
    let run_id = run.run_id.clone();
    shared.spawn(async move {
        stop_at(owner, run_id, stopped, deadline).await;
    });
    write_input(shared, job, run, &session_id).await
}

/// Writes one delivery's input on the job's target shape and settles it
/// (§3.1, A.5): a `session` target's own branch and mode, or the `main` branch
/// of the Session a `launch` created, which takes a Queue input. An uncertain
/// reply is looked up by its submission id; a definite refusal settles
/// `failed`, and a target-gone code also disables the job (A.3).
async fn write_input(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Delivery,
    session_id: &str,
) -> Result<(), SchedulerError> {
    let (branch_id, mode) = match job.effective_target() {
        Some(Target::Session {
            branch_id,
            input_mode,
            ..
        }) => (branch_id, input_mode),
        Some(Target::Launch { .. }) => ("main".to_owned(), InputMode::Queue),
        None => return Err(SchedulerError::invalid_data("job has no target")),
    };
    let input = json!({
        "sessionId": session_id,
        "branchId": branch_id,
        "mode": mode,
        "submissionId": run.submission_id,
        "rawBlocks": [{"text": marker(job, &run.run_id)}],
        "content": [{"type": "text", "text": job.prompt}],
    });
    let reply = lose_input_reply(
        shared,
        sessions(shared.sessions.as_ref(), "input", input).await,
    );
    match reply {
        Ok(Ok(receipt)) => {
            let input_id = receipt["inputId"]
                .as_str()
                .ok_or_else(|| SchedulerError::invalid_data("input reply has no inputId"))?;
            settle_delivered(shared, &run.run_id, input_id).await
        }
        Ok(Err(code)) if uncertain(&code) => {
            lookup_now(shared, &run.run_id).await;
            Ok(())
        }
        Ok(Err(code)) => failed(shared, run, &code).await,
        Err(_) => {
            lookup_now(shared, &run.run_id).await;
            Ok(())
        }
    }
}

/// One settling pass for a record whose write answered with no fact, so a
/// healthy service settles it now. A failed pass leaves the record to the
/// next generation's or the timer's pass (A.6).
async fn lookup_now(shared: &Arc<Shared>, run_id: &str) {
    if let Err(error) = lookup_once(shared, run_id).await {
        tracing::warn!(%error, "scheduler lookup failed");
    }
}
