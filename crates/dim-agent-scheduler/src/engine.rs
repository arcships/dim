//! Scheduled and manual deliveries (design §2.2, §3.1, A.4–A.6): windows, the
//! pre-effect record, a session branch's input, a launch target's Session and
//! its tracking, bounded lookup settling, and retention.
use crate::{
    document::{Job, Mode, Origin, Schedule, Target, canonical_instant},
    methods::{Shared, read_document, window_after},
    runs,
    runs::{Delivery, DeliveryState, ReportTo, SchedulerError, SessionOrigin, stamp},
};
use chrono::{DateTime, Duration, Utc};
use dim_agent_capabilities::settings::SettingsScope;
use dim_agent_types::{
    AgentEvent, EventKind, PermissionPolicy, PermissionPreset, TurnOutcome, markup::escaped,
};
use dim_app_server::settings::SettingsError;
use rutis_bridge::session::{HostDispatch, settle};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration as StdDuration};
use tokio_util::sync::CancellationToken;

const POLL: StdDuration = StdDuration::from_secs(1);
const MAX_CONCURRENT_RUNS: usize = 8;

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

fn unloading() -> SchedulerError {
    SchedulerError::new(UNLOADING, "Scheduler is unloading")
}

fn is_reloading(code: &str) -> bool {
    code.ends_with("reloading")
}

/// A reply that carries no fact about the write: the record stays
/// `delivering` and the lookup settles it (A.5, A.6).
pub(crate) fn uncertain(code: &str) -> bool {
    is_reloading(code) || matches!(code, "publication_unknown" | "ambiguous")
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
pub(crate) const REPORT_GUIDANCE: &str = "Inspect schedule.history for this job.";

/// The marker both targets write before the saved prompt (§5).
fn marker(job: &Job, run_id: &str) -> String {
    format!(
        "<dim-scheduler kind=\"scheduled-run\" job=\"{}\" run=\"{}\">{}</dim-scheduler>",
        escaped(&job.id),
        escaped(run_id),
        escaped(&job.name)
    )
}

/// Records a window of the job, unless one exists for it, and starts what
/// the target does. A missed window is recorded `missed`; a `session` window
/// is recorded `delivering` before its write; a `launch` window follows the
/// launch target's old admission, which skips an overlapping or over-capacity
/// window.
async fn record(
    shared: &Arc<Shared>,
    job: &Job,
    window: String,
    missed: bool,
) -> Result<Option<Delivery>, SchedulerError> {
    let accepted = job.clone();
    let job = job.clone();
    let target = job
        .effective_target()
        .ok_or_else(|| SchedulerError::invalid_data("job has no target"))?;
    let run = runs::update(&shared.files, move |history| {
        if window != MANUAL
            && history
                .iter()
                .any(|run| run.job_id == job.id && run.window == window)
        {
            return Ok((false, None));
        }
        let now = Utc::now();
        let (state, detail) = if missed {
            (DeliveryState::Missed, "missed")
        } else {
            match &target {
                Target::Session { .. } => {
                    if history
                        .iter()
                        .any(|run| run.job_id == job.id && run.state == DeliveryState::Delivering)
                    {
                        (DeliveryState::Missed, PREVIOUS_DELIVERY_UNSETTLED)
                    } else {
                        (DeliveryState::Delivering, "")
                    }
                }
                Target::Launch { .. } => {
                    let active = history
                        .iter()
                        .filter(|run| !run.state.is_terminal())
                        .count();
                    let overlapping = history
                        .iter()
                        .any(|run| !run.state.is_terminal() && run.job_id == job.id);
                    if overlapping {
                        (DeliveryState::Skipped, "overlap")
                    } else if active >= MAX_CONCURRENT_RUNS {
                        (DeliveryState::Skipped, "capacity")
                    } else {
                        (DeliveryState::Launching, "")
                    }
                }
            }
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        let (deadline_at, session_id, branch_id, session_origin, policy, report_to) =
            match (&state, &target) {
                (
                    DeliveryState::Delivering,
                    Target::Session {
                        session_id,
                        branch_id,
                        ..
                    },
                ) => (
                    None,
                    Some(session_id.clone()),
                    Some(branch_id.clone()),
                    None,
                    None,
                    None,
                ),
                (
                    DeliveryState::Launching,
                    Target::Launch {
                        max_run_minutes, ..
                    },
                ) => (
                    Some(stamp(now + Duration::minutes(i64::from(*max_run_minutes)))),
                    None,
                    None,
                    None,
                    None,
                    match &job.created_by {
                        Origin::Tool {
                            session_id,
                            branch_id,
                        } => Some(ReportTo {
                            session_id: session_id.clone(),
                            branch_id: branch_id.clone(),
                        }),
                        Origin::User => None,
                    },
                ),
                _ => (None, None, None, None, None, None),
            };
        let run = Delivery {
            run_id: run_id.clone(),
            job_id: job.id.clone(),
            window,
            state,
            deadline_at,
            session_id,
            branch_id,
            session_origin,
            policy,
            input_event_id: None,
            submission_id: format!("scheduler:{run_id}"),
            report_to,
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
    if let Some(run) = &run {
        match run.state {
            DeliveryState::Delivering => {
                shared.spawn(deliver(shared.clone(), accepted, run.clone()))
            }
            DeliveryState::Launching => {
                shared.spawn(execute(shared.clone(), accepted, run.clone()))
            }
            _ => {}
        }
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
    let cancel = CancellationToken::new();
    let document = read_document(shared, &cancel).await?.document;
    let history = runs::read(&shared.files).await?;
    let now = Utc::now();
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
        let Ok(next) = job.next_window(window_after(job, &history)) else {
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

/// The authority a launch target requests: its saved preset, capped by the
/// saved cap. A management job without a preset requests read-only. The
/// preset comes from the effective target: a job's top-level legacy
/// `policy` never applies once the job has a target of its own.
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
    origin: SessionOrigin,
    policy: PermissionPolicy,
) -> Result<(), SchedulerError> {
    update_run(shared, run_id, move |run| {
        run.session_id = Some(session_id);
        run.session_origin = Some(origin);
        run.policy = Some(policy);
    })
    .await
}

/// Settles a delivery to a terminal state once, then reports it when it has a
/// report target. A delivery that already settled keeps its outcome.
async fn finish(
    shared: &Arc<Shared>,
    run_id: &str,
    state: DeliveryState,
    detail: String,
) -> Result<(), SchedulerError> {
    let run_id = run_id.to_owned();
    shared.reporting.lock().unwrap().insert(run_id.clone());
    let marked = run_id.clone();
    let settled = runs::update(&shared.files, move |history| {
        let run = history
            .iter_mut()
            .find(|run| run.run_id == run_id)
            .ok_or_else(|| SchedulerError::invalid_data(format!("run {run_id} is missing")))?;
        if run.state.is_terminal() {
            return Ok((false, None));
        }
        run.state = state;
        run.ended_at = Some(stamp(Utc::now()));
        run.detail = detail;
        run.validate().map_err(SchedulerError::invalid_data)?;
        Ok((true, Some(run.clone())))
    })
    .await;
    let settled = match settled {
        Ok(settled) => settled,
        Err(error) => {
            shared.reporting.lock().unwrap().remove(&marked);
            return Err(error);
        }
    };
    let Some(run) = settled else {
        shared.reporting.lock().unwrap().remove(&marked);
        return Ok(());
    };
    shared.watch.run(&run);
    let outcome = report(shared, run).await;
    shared.reporting.lock().unwrap().remove(&marked);
    shared.spawn(prune(shared.clone()));
    outcome
}

/// Steers a launch delivery's outcome into the branch that saved the job,
/// once. `skipped` and `missed` deliveries only keep history (A.6). A failed
/// send is recorded in the delivery's detail and is not retried. This is the
/// launch target's behaviour until it is reduced; a `session` delivery has no
/// report target.
async fn report(shared: &Arc<Shared>, run: Delivery) -> Result<(), SchedulerError> {
    if matches!(run.state, DeliveryState::Skipped | DeliveryState::Missed) {
        return Ok(());
    }
    let Some(target) = run.report_to.clone() else {
        return Ok(());
    };
    let mut body = format!(
        "Scheduled job {}, run {}: {} ({}).",
        run.job_id,
        run.run_id,
        run.state.name(),
        run.detail
    );
    if let Some(session_id) = &run.session_id {
        body += &format!(" Session {session_id}.");
    }
    body += &format!("\n{}", shared.texts.report_guidance);
    let text = format!(
        "<dim-scheduler kind=\"report\" job=\"{}\" run=\"{}\">{}</dim-scheduler>",
        escaped(&run.job_id),
        escaped(&run.run_id),
        escaped(&body)
    );
    let args = json!({
        "sessionId": target.session_id,
        "branchId": target.branch_id,
        "mode": "steer",
        "submissionId": format!("scheduler-report:{}", run.run_id),
        "rawBlocks": [{"text": text}],
        "content": [],
    });
    let failure = match sessions(shared.sessions.as_ref(), "input", args).await {
        Err(_) => "bridge_error".to_owned(),
        Ok(Err(code)) => code,
        Ok(Ok(_)) => return Ok(()),
    };
    let run_id = run.run_id.clone();
    update_run(shared, &run_id, move |run| {
        run.detail += &format!("; report_failed: {failure}");
    })
    .await
}

/// Retention (A.7): keeps the newest terminal deliveries of each job, every
/// unsettled delivery, and the launch record of each Session that a kept
/// record or a legacy reuse job still references. Expired launch-owned
/// Sessions are deleted first, and their records stay when the delete fails,
/// so the next pass retries.
pub async fn prune(shared: Arc<Shared>) {
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
    let reporting = shared.reporting.lock().unwrap().clone();
    let mut keep: HashSet<String> = runs
        .iter()
        .filter(|run| !run.state.is_terminal() || reporting.contains(&run.run_id))
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

/// Starts a tracker for each launch delivery that was active when the
/// previous process stopped (A.5). Nothing is launched or submitted again.
pub fn recover(shared: &Arc<Shared>, active: Vec<Delivery>) {
    for run in active {
        let owner = shared.clone();
        shared.spawn(async move {
            let shared = owner;
            if let Err(error) = resume(&shared, &run).await
                && error.code != UNLOADING
            {
                let _ = finish(&shared, &run.run_id, DeliveryState::Lost, error.message).await;
            }
        });
    }
}

/// A `launching` delivery with a Session id is looked up by its submission
/// id: an accepted input resumes tracking, an unknown one is `lost`. A
/// `launching` delivery without a Session id is `lost`, because its launch
/// cannot be confirmed. A `submitted` or `running` delivery resumes tracking.
async fn resume(shared: &Arc<Shared>, run: &Delivery) -> Result<(), SchedulerError> {
    let deadline = run
        .deadline_at
        .as_deref()
        .and_then(canonical_instant)
        .ok_or_else(|| SchedulerError::invalid_data("active run has no deadline"))?;
    let Some(session_id) = run.session_id.clone() else {
        return finish(
            shared,
            &run.run_id,
            DeliveryState::Lost,
            "no_session_after_restart".into(),
        )
        .await;
    };
    let (input_id, start) = match &run.input_event_id {
        Some(input_id) => (input_id.clone(), None),
        None => match lookup_input(shared, &session_id, "main", &run.submission_id).await {
            Ok(Some((input_id, sequence))) => {
                let accepted = input_id.clone();
                update_run(shared, &run.run_id, move |run| {
                    run.state = DeliveryState::Submitted;
                    run.input_event_id = Some(accepted);
                })
                .await?;
                (input_id, Some(sequence))
            }
            Ok(None) => {
                return finish(
                    shared,
                    &run.run_id,
                    DeliveryState::Lost,
                    "input_unknown".into(),
                )
                .await;
            }
            Err(code) if code == UNLOADING => return Err(unloading()),
            Err(code) => {
                return finish(
                    shared,
                    &run.run_id,
                    DeliveryState::Lost,
                    format!("lookup_failed: {code}"),
                )
                .await;
            }
        },
    };
    track(shared, &run.run_id, &session_id, &input_id, start, deadline).await
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
/// marker and the saved prompt in the job's mode (§3.1, A.5). There is no
/// launch, tracking, history read, deadline or report.
async fn deliver(shared: Arc<Shared>, job: Job, run: Delivery) {
    if let Err(error) = write_session(&shared, &job, &run).await
        && error.code != UNLOADING
    {
        let _ = finish(&shared, &run.run_id, DeliveryState::Lost, error.message).await;
    }
}

async fn write_session(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Delivery,
) -> Result<(), SchedulerError> {
    let Some(Target::Session {
        session_id,
        branch_id,
        input_mode,
    }) = job.effective_target()
    else {
        return Err(SchedulerError::invalid_data(
            "a session delivery names a non-session target",
        ));
    };
    let input = json!({
        "sessionId": session_id,
        "branchId": branch_id,
        "mode": input_mode,
        "submissionId": run.submission_id,
        "rawBlocks": [{"text": marker(job, &run.run_id)}],
        "content": [{"type": "text", "text": job.prompt}],
    });
    match sessions(shared.sessions.as_ref(), "input", input).await {
        Ok(Ok(receipt)) => {
            let input_id = receipt["inputId"]
                .as_str()
                .ok_or_else(|| SchedulerError::invalid_data("input reply has no inputId"))?;
            settle_delivered(shared, &run.run_id, input_id).await
        }
        Ok(Err(code)) if uncertain(&code) => {
            // Only this delivery's reply was uncertain: look it up now, and
            // leave other records to the timer's once-a-minute pass (A.6).
            if let Err(error) = lookup_once(shared, run).await {
                tracing::warn!(%error, "scheduler lookup failed");
            }
            Ok(())
        }
        Ok(Err(code)) => failed(shared, run, &code).await,
        Err(_) => {
            if let Err(error) = lookup_once(shared, run).await {
                tracing::warn!(%error, "scheduler lookup failed");
            }
            Ok(())
        }
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
    disable_gone(shared, run, code, stamp(Utc::now())).await;
    Ok(())
}

/// One settling pass over every `delivering` record: recovery at start and
/// the timer's once-a-minute retry (A.6). Each record gets at most one lookup
/// attempt per pass; a record whose stored count is already at the bound
/// settles `lost` without one (D21).
pub async fn settle_delivering(shared: &Arc<Shared>) {
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
        if let Err(error) = lookup_once(shared, run).await {
            tracing::warn!(%error, "scheduler lookup failed");
        }
    }
}

/// The delivery task's own settling after an uncertain input reply: one
/// pass, so a healthy service settles the record now.
enum LookupStep {
    /// The stored count was already at the bound; the record settled `lost`.
    Exhausted(Box<Delivery>),
    /// This pass persisted attempt `n` before its lookup.
    Attempt(u32),
    /// The record settled or moved meanwhile; nothing to look up.
    Settled,
}

/// One lookup attempt for one `delivering` record (A.6): persist the
/// incremented `attempts` before the lookup, settle `delivered` on
/// `accepted`, and settle `lost` on the fifth answer without a fact.
async fn lookup_once(shared: &Arc<Shared>, run: &Delivery) -> Result<(), SchedulerError> {
    let run_id = run.run_id.clone();
    let step = runs::update(&shared.files, move |history| {
        let Some(run) = history.iter_mut().find(|run| run.run_id == run_id) else {
            return Ok((false, LookupStep::Settled));
        };
        if run.state != DeliveryState::Delivering {
            return Ok((false, LookupStep::Settled));
        }
        let attempts = run.attempts.unwrap_or(0);
        if attempts >= LOOKUP_BOUND {
            run.state = DeliveryState::Lost;
            run.ended_at = Some(stamp(Utc::now()));
            run.detail = LOOKUP_BUDGET_EXHAUSTED.into();
            run.validate().map_err(SchedulerError::invalid_data)?;
            return Ok((true, LookupStep::Exhausted(Box::new(run.clone()))));
        }
        run.attempts = Some(attempts + 1);
        let attempt = run.attempts.unwrap_or(attempts + 1);
        run.validate().map_err(SchedulerError::invalid_data)?;
        Ok((true, LookupStep::Attempt(attempt)))
    })
    .await?;
    match step {
        LookupStep::Settled => Ok(()),
        LookupStep::Exhausted(settled) => {
            shared.watch.run(&settled);
            shared.spawn(prune(shared.clone()));
            Ok(())
        }
        LookupStep::Attempt(attempt) => {
            let (Some(session_id), Some(branch_id)) =
                (run.session_id.clone(), run.branch_id.clone())
            else {
                return Err(SchedulerError::invalid_data(
                    "a delivering record names no session branch",
                ));
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
                    return settle_target_gone(shared, run, &code).await;
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
        run.ended_at = Some(stamp(Utc::now()));
        run.validate().map_err(SchedulerError::invalid_data)?;
        *observe.lock().unwrap() = Some(run.clone());
        Ok((true, ()))
    })
    .await?;
    if let Some(run) = moved.lock().unwrap().clone() {
        shared.watch.run(&run);
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
        run.ended_at = Some(stamp(Utc::now()));
        run.detail = answer;
        run.validate().map_err(SchedulerError::invalid_data)?;
        *observe.lock().unwrap() = Some(run.clone());
        Ok((true, ()))
    })
    .await?;
    if let Some(run) = moved.lock().unwrap().clone() {
        shared.watch.run(&run);
        shared.spawn(prune(shared.clone()));
    }
    Ok(())
}

async fn execute(shared: Arc<Shared>, job: Job, run: Delivery) {
    if let Err(error) = drive(&shared, &job, &run).await
        && error.code != UNLOADING
    {
        let _ = finish(&shared, &run.run_id, DeliveryState::Lost, error.message).await;
    }
}

/// The launch target's pre-effect recheck (A.5): the accepted job is still
/// enabled at `revision`, and the delivery's deadline has not passed. A
/// failed check settles the delivery and returns `false`.
async fn still_eligible(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Delivery,
    revision: u64,
) -> Result<bool, SchedulerError> {
    let cancel = CancellationToken::new();
    let document = match read_document(shared, &cancel).await {
        Ok(snapshot) => snapshot.document,
        Err(_) => {
            finish(
                shared,
                &run.run_id,
                DeliveryState::Skipped,
                "settings_read_failed".into(),
            )
            .await?;
            return Ok(false);
        }
    };
    let deadline = run
        .deadline_at
        .as_deref()
        .and_then(canonical_instant)
        .ok_or_else(|| SchedulerError::invalid_data("accepted run has no deadline"))?;
    if Utc::now() >= deadline {
        finish(
            shared,
            &run.run_id,
            DeliveryState::TimedOut,
            "deadline_before_input".into(),
        )
        .await?;
        return Ok(false);
    }
    let current = document.jobs.iter().find(|current| current.id == job.id);
    if let Some(current) = current
        && current.enabled
        && current.revision == revision
    {
        return Ok(true);
    }
    let reason = if current.is_some_and(|current| !current.enabled) {
        "disabled"
    } else {
        "changed"
    };
    finish(shared, &run.run_id, DeliveryState::Skipped, reason.into()).await?;
    Ok(false)
}

/// Launches the job's Session with the requested authority and records it on
/// the delivery before any input. `None` means the delivery settled.
async fn launch(
    shared: &Arc<Shared>,
    run: &Delivery,
    requested: &PermissionPolicy,
    target: &Target,
) -> Result<Option<String>, SchedulerError> {
    let Target::Launch { cwd, agent_id, .. } = target else {
        return Err(SchedulerError::invalid_data(
            "launch of a non-launch target",
        ));
    };
    let mut args = json!({"cwd": cwd, "policy": requested});
    if let Some(agent) = agent_id.as_ref() {
        args["agent"] = json!(agent);
    }
    let launched = match sessions(shared.sessions.as_ref(), "launch", args).await {
        Err(_) => {
            finish(
                shared,
                &run.run_id,
                DeliveryState::Lost,
                "launch_reply_unknown".into(),
            )
            .await?;
            return Ok(None);
        }
        Ok(Err(code)) => {
            finish(shared, &run.run_id, DeliveryState::Failed, code).await?;
            return Ok(None);
        }
        Ok(Ok(value)) => value,
    };
    let session_id = launched["sessionId"]
        .as_str()
        .ok_or_else(|| SchedulerError::invalid_data("launch reply has no sessionId"))?
        .to_owned();
    let effective: PermissionPolicy = serde_json::from_value(launched["policy"].clone())
        .map_err(|error| SchedulerError::invalid_data(error.to_string()))?;
    record_session(
        shared,
        &run.run_id,
        session_id.clone(),
        SessionOrigin::Launch,
        effective,
    )
    .await?;
    Ok(Some(session_id))
}

async fn drive(shared: &Arc<Shared>, job: &Job, run: &Delivery) -> Result<(), SchedulerError> {
    if !still_eligible(shared, job, run, job.revision).await? {
        return Ok(());
    }
    let Some(target) = job.effective_target() else {
        return Err(SchedulerError::invalid_data("job has no target"));
    };
    let Target::Launch { policy, .. } = &target else {
        return Err(SchedulerError::invalid_data(
            "a launch delivery names a non-launch target",
        ));
    };
    let requested = requested_policy(*policy).cap(&job.policy_cap.clone().unwrap_or_default());
    let Some(session_id) = launch(shared, run, &requested, &target).await? else {
        return Ok(());
    };
    if !still_eligible(shared, job, run, job.revision).await? {
        return Ok(());
    }

    let marker = marker(job, &run.run_id);
    let input = json!({
        "sessionId": session_id,
        "branchId": "main",
        "mode": "queue",
        "submissionId": run.submission_id,
        "rawBlocks": [{"text": marker}],
        "content": [{"type": "text", "text": job.prompt}],
    });
    let (input_id, start) = match sessions(shared.sessions.as_ref(), "input", input).await {
        Ok(Ok(receipt)) => match (receipt["inputId"].as_str(), receipt["sequence"].as_u64()) {
            (Some(input_id), Some(sequence)) => (input_id.to_owned(), sequence),
            _ => {
                return Err(SchedulerError::invalid_data(
                    "input reply has no inputId and sequence",
                ));
            }
        },
        Ok(Err(code)) if !uncertain(&code) => {
            return finish(shared, &run.run_id, DeliveryState::Failed, code).await;
        }
        Ok(Err(_)) | Err(_) => {
            match lookup_input(shared, &session_id, "main", &run.submission_id).await {
                Ok(Some(accepted)) => accepted,
                Ok(None) => {
                    return finish(
                        shared,
                        &run.run_id,
                        DeliveryState::Lost,
                        "input_unknown".into(),
                    )
                    .await;
                }
                Err(code) if code == UNLOADING => return Err(unloading()),
                Err(code) => {
                    return finish(
                        shared,
                        &run.run_id,
                        DeliveryState::Lost,
                        format!("lookup_failed: {code}"),
                    )
                    .await;
                }
            }
        }
    };
    {
        let input_id = input_id.clone();
        update_run(shared, &run.run_id, move |run| {
            run.state = DeliveryState::Submitted;
            run.input_event_id = Some(input_id);
        })
        .await?;
    }
    let deadline = run
        .deadline_at
        .as_deref()
        .and_then(canonical_instant)
        .ok_or_else(|| SchedulerError::invalid_data("accepted run has no deadline"))?;
    track(
        shared,
        &run.run_id,
        &session_id,
        &input_id,
        Some(start),
        deadline,
    )
    .await
}

/// Follows the launched delivery's Session history (§3.2), from the input's
/// own sequence when it is known. Each pass reads the whole published prefix
/// through the first page's `through` before deciding. The delivery is
/// `running` from its own consumption, and the first following `turn/end`
/// supplies the outcome; its own `input/deleted` settles `cancelled:
/// withdrawn`. At the deadline an input applied and interrupted before
/// consumption settles `timed_out` without a stop; an unconsumed input is
/// withdrawn (`deleted` settles `timed_out`, `notHeld` reads history again);
/// consumed work is stopped and settles `timed_out` at its `turn/end`. A
/// `withdraw` or `stop` that does not answer is retried on the next pass.
async fn track(
    shared: &Arc<Shared>,
    run_id: &str,
    session_id: &str,
    input_id: &str,
    start: Option<u64>,
    deadline: DateTime<Utc>,
) -> Result<(), SchedulerError> {
    let mut after = start.map(|sequence| sequence.saturating_sub(1));
    let mut consumed = false;
    let mut applied = false;
    let mut interrupted = false;
    let mut stopped = false;
    loop {
        let mut cutoff: Option<u64> = None;
        loop {
            let mut args = json!({"sessionId": session_id, "branchId": "main"});
            if let Some(after) = after {
                args["after"] = json!(after);
            }
            if let Some(through) = cutoff {
                args["through"] = json!(through);
            }
            let page = match sessions(shared.sessions.as_ref(), "history", args).await {
                Ok(Ok(value)) => value,
                Ok(Err(code)) if !is_reloading(&code) => {
                    return finish(shared, run_id, DeliveryState::Failed, code).await;
                }
                Ok(Err(_)) | Err(_) => {
                    if !shared.pause(POLL).await {
                        return Err(unloading());
                    }
                    continue;
                }
            };
            let events: Vec<AgentEvent> = serde_json::from_value(page["events"].clone())
                .map_err(|error| SchedulerError::invalid_data(error.to_string()))?;
            let through = page["through"].as_u64();
            cutoff = cutoff.or(through);
            if let Some(last) = events.last() {
                after = Some(last.sequence);
            }
            for event in events {
                match event.kind {
                    EventKind::InputDeleted(data) if data.input_id == input_id && !consumed => {
                        let (state, detail) = if stopped {
                            (DeliveryState::TimedOut, "withdrawn_at_deadline")
                        } else {
                            (DeliveryState::Cancelled, "withdrawn")
                        };
                        return finish(shared, run_id, state, detail.into()).await;
                    }
                    EventKind::InputApplied(data) if data.input_id == input_id => applied = true,
                    EventKind::StopInterrupt(_) if applied && !consumed => interrupted = true,
                    EventKind::InputConsumed(data) if data.input_id == input_id && !consumed => {
                        consumed = true;
                        update_run(shared, run_id, |run| {
                            if run.state == DeliveryState::Submitted {
                                run.state = DeliveryState::Running;
                            }
                        })
                        .await?;
                    }
                    EventKind::TurnEnd(end) if consumed => {
                        let (state, detail) = if stopped {
                            (DeliveryState::TimedOut, "timed_out".to_owned())
                        } else {
                            match end.outcome {
                                TurnOutcome::Completed { .. } => {
                                    (DeliveryState::Completed, "completed".into())
                                }
                                TurnOutcome::Failed { error } => (DeliveryState::Failed, error),
                                TurnOutcome::Cancelled { reason } => {
                                    (DeliveryState::Cancelled, reason)
                                }
                            }
                        };
                        return finish(shared, run_id, state, detail).await;
                    }
                    _ => {}
                }
            }
            if !(through.is_some_and(|through| after.is_some_and(|after| after < through))) {
                break;
            }
        }
        if Utc::now() >= deadline && !stopped {
            if !consumed && interrupted {
                return finish(shared, run_id, DeliveryState::TimedOut, "timed_out".into()).await;
            }
            if !consumed {
                let withdrawn = sessions(
                    shared.sessions.as_ref(),
                    "withdraw",
                    json!({"sessionId": session_id, "branchId": "main", "inputId": input_id,
                        "reason": "scheduled run timed out"}),
                )
                .await;
                if let Ok(Ok(value)) = &withdrawn
                    && value["status"] == json!("deleted")
                {
                    return finish(
                        shared,
                        run_id,
                        DeliveryState::TimedOut,
                        "withdrawn_at_deadline".into(),
                    )
                    .await;
                }
            } else if matches!(
                sessions(
                    shared.sessions.as_ref(),
                    "stop",
                    json!({"sessionId": session_id, "branchId": "main", "reason": "scheduled run timed out"}),
                )
                .await,
                Ok(Ok(_))
            ) {
                stopped = true;
            }
        }
        if !shared.pause(POLL).await {
            return Err(unloading());
        }
    }
}
