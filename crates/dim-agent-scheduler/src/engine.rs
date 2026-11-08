//! Scheduled and manual runs (design §3.2, A.4–A.6): windows, acceptance, the
//! pre-effect recheck, launch or reuse of a Session, the first-reuse binding,
//! input, tracking to a terminal outcome, the steer report of a tool-saved
//! job's outcome, and retention.
use crate::{
    document::{Job, Mode, Origin, Schedule, canonical_instant},
    methods::{Binding, Shared, bind_job, host_bound, read_document, session_policy, window_after},
    runs::{self, ReportTo, Run, RunState, SchedulerError, SessionOrigin, stamp},
};
use chrono::{DateTime, Duration, Utc};
use dim_agent_capabilities::settings::SettingsScope;
use dim_agent_types::{AgentEvent, EventKind, PermissionPolicy, PermissionPreset, TurnOutcome};
use dim_app_server::settings::SettingsError;
use rutis_bridge::session::{HostDispatch, settle};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration as StdDuration};
use tokio_util::sync::CancellationToken;

const POLL: StdDuration = StdDuration::from_secs(1);
const MAX_CONCURRENT_RUNS: usize = 8;

const MANUAL: &str = "manual";
/// Terminal runs kept per job, newest first (A.7).
const RETAINED_TERMINAL_RUNS: usize = 100;

/// A Session service code that asks the caller to retry later: the Agents index or
/// the capability set is still loading after a restart (`reloading`, `capability reloading`).
/// The error a run task returns when the owner unloads mid-run. The run stays
/// active in the file, and the next generation's recovery resumes it.
pub(crate) const UNLOADING: &str = "service_reloading";

fn unloading() -> SchedulerError {
    SchedulerError::new(UNLOADING, "Scheduler is unloading")
}

fn is_reloading(code: &str) -> bool {
    code.ends_with("reloading")
}

/// The codes that say the target can never accept the input: its Session or
/// branch is gone. The fire path and the stale lookup both settle the record
/// `cancelled` on these and never retry them (design §10.3, §10.4); any other
/// failure says nothing about the target.
pub(crate) fn target_gone(code: &str) -> bool {
    matches!(code, "session_closed" | "branch_missing" | "branch_ended")
}

fn is_launched(run: &Run) -> bool {
    run.session_origin == Some(SessionOrigin::Launch)
}
/// A window this far behind the clock is missed, not run (A.4). The wakeup
/// policy uses the same grace (design §10).
pub(crate) const GRACE_SECONDS: i64 = 60;
pub(crate) const REPORT_GUIDANCE: &str = "Inspect schedule.history for this job.";
/// A run detail set when a first-reuse binding conflicts twice (A.5.1).
const BINDING_CONFLICT: &str = "binding_conflict";

/// Records a run of `window` for the job, unless one exists for that window,
/// and starts it when it is accepted. A missed window is recorded `missed`;
/// an overlapping run or a full host records `skipped`.
async fn record(
    shared: &Arc<Shared>,
    job: &Job,
    window: String,
    missed: bool,
) -> Result<Option<Run>, SchedulerError> {
    let accepted = job.clone();
    let job = job.clone();
    let run = runs::update(&shared.files, move |history| {
        if window != MANUAL
            && history
                .iter()
                .any(|run| run.job_id == job.id && run.window == window)
        {
            return Ok((false, None));
        }
        let active = history
            .iter()
            .filter(|run| !run.state.is_terminal())
            .count();
        let overlapping = history
            .iter()
            .any(|run| !run.state.is_terminal() && run.job_id == job.id);
        let (state, detail) = if missed {
            (RunState::Missed, "missed")
        } else if overlapping {
            (RunState::Skipped, "overlap")
        } else if active >= MAX_CONCURRENT_RUNS {
            (RunState::Skipped, "capacity")
        } else {
            (RunState::Launching, "")
        };
        let now = Utc::now();
        let run_id = uuid::Uuid::new_v4().to_string();
        let terminal = state.is_terminal();
        let report_to = match (&job.created_by, state) {
            (
                Origin::Tool {
                    session_id,
                    branch_id,
                },
                RunState::Launching,
            ) => Some(ReportTo {
                session_id: session_id.clone(),
                branch_id: branch_id.clone(),
            }),
            _ => None,
        };
        let run = Run {
            run_id: run_id.clone(),
            job_id: job.id.clone(),
            window,
            state,
            deadline_at: (!terminal)
                .then(|| stamp(now + Duration::minutes(i64::from(job.max_run_minutes)))),
            session_id: None,
            session_origin: None,
            policy: None,
            input_event_id: None,
            submission_id: format!("scheduler:{run_id}"),
            report_to,
            created_at: stamp(now),
            ended_at: terminal.then(|| stamp(now)),
            detail: detail.into(),
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
        && run.state == RunState::Launching
    {
        shared.spawn(execute(shared.clone(), accepted, run.clone()));
    }
    if run.is_some() {
        shared.spawn(prune(shared.clone()));
    }
    Ok(run)
}

/// Accepts a manual run of the job (`schedule.run_now`).
pub async fn accept(shared: Arc<Shared>, job_id: &str) -> Result<Run, SchedulerError> {
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

/// The authority of a run's launch request: the saved preset, capped by the
/// saved authority (§3.3). A management job without a preset requests
/// read-only; a tool job requests its saved cap.
pub(crate) fn requested_policy(job: &Job) -> PermissionPolicy {
    match (&job.created_by, job.policy) {
        (_, Some(preset)) => PermissionPolicy {
            preset,
            ..PermissionPolicy::default()
        },
        (Origin::User, None) => PermissionPolicy {
            preset: PermissionPreset::ReadOnly,
            ..PermissionPolicy::default()
        },
        (Origin::Tool { .. }, None) => job.policy_cap.clone(),
    }
}

/// Writes one run's change under the file lock, and announces the state the
/// record took when the change moved it.
async fn update_run(
    shared: &Shared,
    run_id: &str,
    change: impl FnOnce(&mut Run) + Send + 'static,
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

/// Settles a run to a terminal state once, then reports it when it has a
/// report target. A run that already settled keeps its outcome.
async fn finish(
    shared: &Arc<Shared>,
    run_id: &str,
    state: RunState,
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
        run.detail = if run.detail == BINDING_CONFLICT {
            if detail.is_empty() {
                BINDING_CONFLICT.to_owned()
            } else {
                format!("{BINDING_CONFLICT}; {detail}")
            }
        } else {
            detail
        };
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

/// Steers a tool-saved run's outcome into the branch that saved the job, once.
/// `skipped` and `missed` runs only keep history (A.6). A failed send is
/// recorded in the run's detail and is not retried.
async fn report(shared: &Arc<Shared>, run: Run) -> Result<(), SchedulerError> {
    if matches!(run.state, RunState::Skipped | RunState::Missed) {
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

/// Retention (A.7): keeps the newest terminal runs of each job, every active run,
/// and the launch record of each Session that a kept run or a reuse job still
/// references. Expired launch-owned Sessions are deleted first, and their records
/// stay when the delete fails, so the next pass retries.
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
    let reporting = shared.reporting.lock().unwrap().clone();
    let mut keep: HashSet<String> = runs
        .iter()
        .filter(|run| !run.state.is_terminal() || reporting.contains(&run.run_id))
        .map(|run| run.run_id.clone())
        .collect();
    for job in &document.jobs {
        let mut own: Vec<&Run> = runs
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
    let mut references: HashSet<String> = document
        .jobs
        .iter()
        .filter(|job| job.mode == Mode::Reuse)
        .filter_map(|job| job.session_id.clone())
        .collect();
    references.extend(
        runs.iter()
            .filter(|run| keep.contains(&run.run_id))
            .filter_map(|run| run.session_id.clone()),
    );
    for session_id in &references {
        let owner = runs
            .iter()
            .filter(|run| {
                run.session_id.as_deref() == Some(session_id)
                    && run.session_origin == Some(SessionOrigin::Launch)
            })
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then(a.run_id.cmp(&b.run_id))
            });
        if let Some(owner) = owner {
            keep.insert(owner.run_id.clone());
        }
    }
    let expired: Vec<&Run> = runs
        .iter()
        .filter(|run| !keep.contains(&run.run_id))
        .collect();
    let deletions: HashSet<String> = expired
        .iter()
        .filter(|run| is_launched(run))
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
    if !remove.is_empty() {
        runs::update(&shared.files, move |history| {
            let before = history.len();
            history.retain(|run| !remove.contains(&run.run_id));
            Ok((history.len() != before, ()))
        })
        .await?;
    }
    Ok(pending)
}

/// Starts a tracker for each run that was active when the previous process
/// stopped (A.5). Nothing is launched or submitted again.
pub fn recover(shared: &Arc<Shared>, active: Vec<Run>) {
    for run in active {
        let owner = shared.clone();
        shared.spawn(async move {
            let shared = owner;
            if let Err(error) = resume(&shared, &run).await
                && error.code != UNLOADING
            {
                let _ = finish(&shared, &run.run_id, RunState::Lost, error.message).await;
            }
        });
    }
}

/// A `launching` run with a Session id is looked up by its submission id: an
/// accepted input resumes tracking, an unknown one is `lost`. A `launching` run
/// without a Session id is `lost`, because its launch cannot be confirmed.
async fn resume(shared: &Arc<Shared>, run: &Run) -> Result<(), SchedulerError> {
    let deadline = run
        .deadline_at
        .as_deref()
        .and_then(canonical_instant)
        .ok_or_else(|| SchedulerError::invalid_data("active run has no deadline"))?;
    let Some(session_id) = run.session_id.clone() else {
        return finish(
            shared,
            &run.run_id,
            RunState::Lost,
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
                    run.state = RunState::Submitted;
                    run.input_event_id = Some(accepted);
                })
                .await?;
                (input_id, Some(sequence))
            }
            Ok(None) => {
                return finish(shared, &run.run_id, RunState::Lost, "input_unknown".into()).await;
            }
            Err(code) if code == UNLOADING => return Err(unloading()),
            Err(code) => {
                return finish(
                    shared,
                    &run.run_id,
                    RunState::Lost,
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
/// reloading Session is retried until it answers. The run path resolves an
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

pub(crate) fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn execute(shared: Arc<Shared>, job: Job, run: Run) {
    if let Err(error) = drive(&shared, &job, &run).await
        && error.code != UNLOADING
    {
        let _ = finish(&shared, &run.run_id, RunState::Lost, error.message).await;
    }
}

/// The pre-effect recheck (A.5): the accepted job is still enabled at
/// `revision` with `binding`, and the run's deadline has not passed. A failed
/// check settles the run and returns `false`.
async fn still_eligible(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Run,
    revision: u64,
    binding: Option<&str>,
) -> Result<bool, SchedulerError> {
    let cancel = CancellationToken::new();
    let document = match read_document(shared, &cancel).await {
        Ok(snapshot) => snapshot.document,
        Err(_) => {
            finish(
                shared,
                &run.run_id,
                RunState::Skipped,
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
            RunState::TimedOut,
            "deadline_before_input".into(),
        )
        .await?;
        return Ok(false);
    }
    let current = document.jobs.iter().find(|current| current.id == job.id);
    if let Some(current) = current
        && current.enabled
        && current.revision == revision
        && current.mode == job.mode
        && current.session_id.as_deref() == binding
    {
        return Ok(true);
    }
    let reason = if current.is_some_and(|current| !current.enabled) {
        "disabled"
    } else {
        "changed"
    };
    finish(shared, &run.run_id, RunState::Skipped, reason.into()).await?;
    Ok(false)
}

/// Settles the run with `state` unless the recheck already settled it.
async fn settle_checked(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Run,
    state: RunState,
    detail: &str,
) -> Result<(), SchedulerError> {
    if still_eligible(shared, job, run, job.revision, job.session_id.as_deref()).await? {
        finish(shared, &run.run_id, state, detail.into()).await?;
    }
    Ok(())
}

/// Launches the job's Session with the requested authority and records it on
/// the run before any input. `None` means the run settled.
async fn launch(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Run,
    requested: &PermissionPolicy,
) -> Result<Option<String>, SchedulerError> {
    let mut args = json!({"cwd": job.cwd, "policy": requested});
    if let Some(agent) = &job.agent_id {
        args["agent"] = json!(agent);
    }
    if let Some(model) = &job.model {
        args["model"] = json!({"providerId": model.provider_id, "modelId": model.model_id});
    }
    if let Origin::Tool {
        session_id,
        branch_id,
    } = &job.created_by
    {
        args["starter"] = json!({"sessionId": session_id, "branchId": branch_id});
    }
    let launched = match sessions(shared.sessions.as_ref(), "launch", args).await {
        Err(_) => {
            finish(
                shared,
                &run.run_id,
                RunState::Lost,
                "launch_reply_unknown".into(),
            )
            .await?;
            return Ok(None);
        }
        Ok(Err(code)) => {
            finish(shared, &run.run_id, RunState::Failed, code).await?;
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

/// The live authority of the job's saving origin: the host bound for a
/// management save, or the saving branch's current policy for a tool save.
async fn live_policy(shared: &Shared, job: &Job) -> Result<PermissionPolicy, SchedulerError> {
    match &job.created_by {
        Origin::User => host_bound(shared).await,
        Origin::Tool {
            session_id,
            branch_id,
        } => session_policy(shared, session_id, branch_id).await,
    }
}

/// Uses the job's bound Session when its live authority is inside both the
/// requested policy and the live cap. `None` means the run settled.
async fn reuse(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Run,
    requested: &PermissionPolicy,
    bound: &str,
) -> Result<bool, SchedulerError> {
    let live = match live_policy(shared, job).await {
        Ok(live) => live,
        Err(_) => {
            settle_checked(shared, job, run, RunState::Skipped, "policy_read_failed").await?;
            return Ok(false);
        }
    };
    let actual = match session_policy(shared, bound, "main").await {
        Ok(actual) => actual,
        Err(error) if matches!(error.code, "session_closed" | "branch_missing") => {
            settle_checked(shared, job, run, RunState::Failed, error.code).await?;
            return Ok(false);
        }
        Err(_) => {
            settle_checked(shared, job, run, RunState::Skipped, "policy_read_failed").await?;
            return Ok(false);
        }
    };
    if !actual.within(requested) || !actual.within(&live) {
        settle_checked(shared, job, run, RunState::Skipped, "policy").await?;
        return Ok(false);
    }
    record_session(
        shared,
        &run.run_id,
        bound.to_owned(),
        SessionOrigin::Reuse,
        actual,
    )
    .await?;
    Ok(true)
}

enum Bound {
    Revision(u64),
    Conflict,
}

/// The first-reuse binding (A.5.1): one retry after a stale-document conflict.
/// A second conflict continues the run unbound with `binding_conflict` in its
/// detail. `None` means the run settled.
async fn bind(
    shared: &Arc<Shared>,
    job: &Job,
    run: &Run,
    session_id: &str,
) -> Result<Option<Bound>, SchedulerError> {
    for attempt in 0..2 {
        match bind_job(shared, job, session_id).await {
            Ok(Binding::Bound(revision)) => return Ok(Some(Bound::Revision(revision))),
            Ok(Binding::Changed(reason)) => {
                finish(shared, &run.run_id, RunState::Skipped, reason.into()).await?;
                return Ok(None);
            }
            Err(error) if error.code == "schedule_conflict" && attempt == 0 => continue,
            Err(error) if error.code == "schedule_conflict" => {
                update_run(shared, &run.run_id, |run| {
                    run.detail = BINDING_CONFLICT.into();
                })
                .await?;
                return Ok(Some(Bound::Conflict));
            }
            Err(error) if error.code == "schedule_invalid" => {
                finish(
                    shared,
                    &run.run_id,
                    RunState::Failed,
                    "binding_rejected: schedule_invalid".into(),
                )
                .await?;
                return Ok(None);
            }
            Err(_) => {
                finish(
                    shared,
                    &run.run_id,
                    RunState::Skipped,
                    "binding_unconfirmed".into(),
                )
                .await?;
                return Ok(None);
            }
        }
    }
    Ok(None)
}

async fn drive(shared: &Arc<Shared>, job: &Job, run: &Run) -> Result<(), SchedulerError> {
    if !still_eligible(shared, job, run, job.revision, job.session_id.as_deref()).await? {
        return Ok(());
    }
    let requested = requested_policy(job).cap(&job.policy_cap);
    let session_id = match (&job.mode, &job.session_id) {
        (Mode::Reuse, Some(bound)) => {
            if !reuse(shared, job, run, &requested, bound).await? {
                return Ok(());
            }
            bound.clone()
        }
        _ => match launch(shared, job, run, &requested).await? {
            Some(session_id) => session_id,
            None => return Ok(()),
        },
    };
    let (revision, binding) = match (&job.mode, &job.session_id) {
        (Mode::Reuse, None) => match bind(shared, job, run, &session_id).await? {
            Some(Bound::Revision(revision)) => (revision, Some(session_id.clone())),
            Some(Bound::Conflict) => (job.revision, None),
            None => return Ok(()),
        },
        _ => (job.revision, job.session_id.clone()),
    };
    if !still_eligible(shared, job, run, revision, binding.as_deref()).await? {
        return Ok(());
    }

    let marker = format!(
        "<dim-scheduler kind=\"scheduled-run\" job=\"{}\" run=\"{}\">{}</dim-scheduler>",
        escaped(&job.id),
        escaped(&run.run_id),
        escaped(&job.name)
    );
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
        Ok(Err(code))
            if !is_reloading(&code)
                && !matches!(code.as_str(), "publication_unknown" | "ambiguous") =>
        {
            return finish(shared, &run.run_id, RunState::Failed, code).await;
        }
        Ok(Err(_)) | Err(_) => {
            match lookup_input(shared, &session_id, "main", &run.submission_id).await {
                Ok(Some(accepted)) => accepted,
                Ok(None) => {
                    return finish(shared, &run.run_id, RunState::Lost, "input_unknown".into())
                        .await;
                }
                Err(code) if code == UNLOADING => return Err(unloading()),
                Err(code) => {
                    return finish(
                        shared,
                        &run.run_id,
                        RunState::Lost,
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
            run.state = RunState::Submitted;
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

/// Follows the run's Session history (§3.2), from the input's own sequence
/// when it is known. Each pass reads the whole published prefix through the
/// first page's `through` before deciding. The run is `running` from its own
/// consumption, and the first following `turn/end` supplies the outcome; its
/// own `input/deleted` settles `cancelled: withdrawn`. At the deadline an
/// input applied and interrupted before consumption settles `timed_out`
/// without a stop; an unconsumed input is withdrawn (`deleted` settles
/// `timed_out`, `notHeld` reads history again); consumed work is stopped and
/// settles `timed_out` at its `turn/end`. A `withdraw` or `stop` that does not
/// answer is retried on the next pass.
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
                    return finish(shared, run_id, RunState::Failed, code).await;
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
                            (RunState::TimedOut, "withdrawn_at_deadline")
                        } else {
                            (RunState::Cancelled, "withdrawn")
                        };
                        return finish(shared, run_id, state, detail.into()).await;
                    }
                    EventKind::InputApplied(data) if data.input_id == input_id => applied = true,
                    EventKind::StopInterrupt(_) if applied && !consumed => interrupted = true,
                    EventKind::InputConsumed(data) if data.input_id == input_id && !consumed => {
                        consumed = true;
                        update_run(shared, run_id, |run| {
                            if run.state == RunState::Submitted {
                                run.state = RunState::Running;
                            }
                        })
                        .await?;
                    }
                    EventKind::TurnEnd(end) if consumed => {
                        let (state, detail) = if stopped {
                            (RunState::TimedOut, "timed_out".to_owned())
                        } else {
                            match end.outcome {
                                TurnOutcome::Completed { .. } => {
                                    (RunState::Completed, "completed".into())
                                }
                                TurnOutcome::Failed { error } => (RunState::Failed, error),
                                TurnOutcome::Cancelled { reason } => (RunState::Cancelled, reason),
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
                return finish(shared, run_id, RunState::TimedOut, "timed_out".into()).await;
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
                        RunState::TimedOut,
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
