//! The process owner: the `schedule` management namespace over the `schedule`
//! settings document and the run history file ([design §1.3, A.3](../../../docs/design/plugins/Scheduler.md)).
use crate::{
    document::{
        DEFAULT_MAX_RUN_MINUTES, Job, Mode, ModelSelection, Origin, Schedule, ScheduleDocument,
        canonical_instant,
    },
    runs::{self, Run, SchedulerError, stamp},
};
use chrono::{DateTime, Duration, Utc};
use dim_agent_capabilities::{
    methods::{
        Caller as MethodCaller, Handler, Interruption, MethodCall, MethodDecl, Progress, RpcError,
        params,
    },
    settings::{SettingsChanged, SettingsScope, settings_changes},
};
use dim_agent_runtime::{Names, PartRegistry, PartScope};
use dim_agent_types::{PermissionPolicy, PermissionPreset};
use dim_app_server::{
    SessionPermissions, SessionRef, SessionsError,
    methods::ManagementMethods,
    plugin_files::PluginFileTransactions,
    settings::{BusinessDocument, SettingsError, SettingsService},
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{HostDispatch, host_key};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const NAMESPACE: &str = "schedule";
const DEFAULT_HISTORY_LIMIT: u32 = 20;
const MAX_HISTORY_LIMIT: u32 = 50;

/// Process owner of the `schedule` namespace.
pub struct SchedulerPlugin {
    settings: Key<SettingsService>,
    files: Key<PluginFileTransactions>,
    methods: Key<ManagementMethods>,
    permissions: Key<SessionPermissions>,
    sessions: Key<dyn HostDispatch>,
    parts: Key<PartRegistry>,
    texts: Arc<crate::SchedulerTexts>,
}

impl SchedulerPlugin {
    pub fn new(names: &Names<'_>, texts: Arc<crate::SchedulerTexts>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            files: names.key()?,
            methods: names.key()?,
            permissions: names.key()?,
            sessions: host_key("sessions"),
            parts: names.key()?,
            texts,
        })
    }
}

impl DynPlugin for SchedulerPlugin {
    fn name(&self) -> &str {
        "Scheduler"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.settings.erase(),
            self.files.erase(),
            self.methods.erase(),
            self.permissions.erase(),
            self.sessions.erase(),
            self.parts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            let permissions = ctx.require_at(&self.permissions)?;
            let sessions = ctx.require_at(&self.sessions)?;
            settings.register::<ScheduleDocument>(ctx)?;
            let shared = Arc::new(Shared {
                ctx: ctx.clone(),
                sessions,
                settings,
                files,
                permissions,
                pruning: tokio::sync::Mutex::new(()),
                wakeups: tokio::sync::Mutex::new(()),
                texts: self.texts.clone(),
                stop: ctx.cancellation_token(),
                tasks: TaskTracker::new(),
                retention: Default::default(),
                reporting: Default::default(),
                watch: crate::watch::Watchers::default(),
            });
            let cleanup = shared.clone();
            ctx.effect(move || async move {
                cleanup.stop.cancel();
                cleanup.tasks.close();
                cleanup.tasks.wait().await;
                Ok(())
            })?;
            // A committed `schedule` settings write is the jobs source
            // (design §11.2). The listener is withdrawn with this generation,
            // like every registration made from its apply.
            {
                let shared = shared.clone();
                ctx.listen::<SettingsChanged>()
                    .name(settings_changes(ScheduleDocument::NAMESPACE))
                    .on(move |_, _| {
                        let shared = shared.clone();
                        async move {
                            shared.watch.jobs();
                            Ok(None)
                        }
                    })?;
            }
            ctx.provide_as::<ScheduleJobs>(Arc::new(ScheduleJobs {
                shared: shared.clone(),
            }))?;
            let wakeups = Arc::new(crate::wakeups::Wakeups {
                shared: shared.clone(),
            });
            ctx.provide_as::<crate::wakeups::Wakeups>(wakeups.clone())?;
            methods.register(
                ctx,
                NAMESPACE,
                vec![
                    MethodDecl::new("schedule.list", false, Interruption::Report),
                    MethodDecl::new("schedule.save", true, Interruption::Drain),
                    MethodDecl::new("schedule.delete", true, Interruption::Drain),
                    MethodDecl::new("schedule.set_enabled", true, Interruption::Drain),
                    MethodDecl::new("schedule.history", false, Interruption::Report),
                    MethodDecl::new("schedule.run_now", true, Interruption::Drain),
                    MethodDecl {
                        progress: true,
                        ..MethodDecl::new("schedule.watch", false, Interruption::Report)
                    },
                ],
                handler(shared.clone()),
            )?;
            let active = runs::read(&shared.files).await.map_err(|error| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    format!("scheduler run history: {error}").into(),
                ))
            })?;
            // A wakeup file that is not valid v1 fails the row like an invalid
            // run history; the bytes stay unchanged (A.7's rule).
            crate::wakeups::read(&shared.files).await.map_err(|error| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    format!("scheduler wakeups: {error}").into(),
                ))
            })?;
            crate::engine::recover(
                &shared,
                active
                    .into_iter()
                    .filter(|run| !run.state.is_terminal())
                    .collect(),
            );
            shared.spawn(crate::engine::prune(shared.clone()));
            let texts = self.texts.clone();
            ctx.require_at(&self.parts)?.register(
                ctx,
                PartScope::Session,
                "SchedulerSession",
                move |mount| {
                    Ok(Some(Box::new(crate::SchedulerSession::new(
                        mount.names,
                        texts.description.clone(),
                    )?)))
                },
            )?;
            let timer = shared.clone();
            shared.spawn(async move {
                let mut ticks = 0u32;
                while timer.pause(std::time::Duration::from_secs(1)).await {
                    if let Err(error) = crate::engine::tick(&timer).await {
                        tracing::warn!(%error, "scheduler tick failed");
                    }
                    if let Err(error) = wakeups.evaluate().await {
                        tracing::warn!(%error, "scheduler wakeup pass failed");
                    }
                    ticks += 1;
                    if ticks.is_multiple_of(60) && timer.retention_pending() {
                        crate::engine::prune(timer.clone()).await;
                    }
                }
            });
            Ok(Effect::Done)
        })
    }
}

pub(crate) struct Shared {
    /// This generation's context, used to announce committed changes on a
    /// Session's channel (design §11).
    pub(crate) ctx: Ctx,
    pub(crate) sessions: Arc<dyn HostDispatch>,
    pub(crate) settings: Arc<SettingsService>,
    pub(crate) files: Arc<PluginFileTransactions>,
    pub(crate) permissions: Arc<SessionPermissions>,
    pub(crate) texts: Arc<crate::SchedulerTexts>,
    /// Serializes retention passes, so a pass never deletes what another reads.
    pub(crate) pruning: tokio::sync::Mutex<()>,
    /// Serializes a wakeup cancel and the whole timer pass — the snapshot
    /// read, the input writes and the settle batch — so the two order at the
    /// record level: a cancel that takes it first prevents the input, one
    /// that takes it after reports the settled state (design §10.3).
    pub(crate) wakeups: tokio::sync::Mutex<()>,
    /// Cancelled when this generation starts to unload.
    pub(crate) stop: CancellationToken,
    /// The timer, run tasks and retention passes; cleanup waits for them.
    pub(crate) tasks: TaskTracker,
    /// Whether the last retention pass left expired records or could not
    /// read; the timer retries it once a minute (A.7).
    pub(crate) retention: std::sync::atomic::AtomicBool,
    /// Runs whose report call or diagnostic write is still in flight; their
    /// records are not eligible for retention (A.7).
    pub(crate) reporting: std::sync::Mutex<HashSet<String>>,
    /// The open `schedule.watch` calls of this generation (design §11.2).
    pub(crate) watch: crate::watch::Watchers,
}

impl Shared {
    /// Starts owned background work, unless the owner is unloading.
    pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        if !self.stop.is_cancelled() {
            self.tasks.spawn(task);
        }
    }

    pub(crate) fn retention_pending(&self) -> bool {
        self.retention.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Waits `delay`; `false` once the owner is unloading, when the caller
    /// stops and leaves an active run to the next generation's recovery.
    pub(crate) async fn pause(&self, delay: std::time::Duration) -> bool {
        tokio::select! {
            _ = self.stop.cancelled() => false,
            _ = tokio::time::sleep(delay) => true,
        }
    }
}

fn handler(shared: Arc<Shared>) -> Handler {
    Arc::new(move |call, cancel, progress| {
        let MethodCall {
            method,
            params: value,
            origin,
        } = call;
        let shared = shared.clone();
        Box::pin(async move {
            dispatch(&shared, &method, value, origin.caller, progress, &cancel).await
        })
    })
}

impl From<SchedulerError> for RpcError {
    fn from(error: SchedulerError) -> Self {
        RpcError::service(error.code, error.message)
    }
}

/// A settings read failure. A document that does not validate is stored
/// data, so it is `invalid_data`.
fn read_failure(error: SettingsError) -> SchedulerError {
    match error {
        SettingsError::Invalid(_)
        | SettingsError::Document(_)
        | SettingsError::DocumentShape { .. } => SchedulerError::invalid_data(error.to_string()),
        error => SchedulerError::new("service_unavailable", error.to_string()),
    }
}

/// A settings write failure: a stale document is `schedule_conflict`, and a
/// rejected document is the request's own error.
fn write_failure(error: SettingsError) -> SchedulerError {
    match error {
        SettingsError::Conflict => SchedulerError::new("schedule_conflict", error.to_string()),
        SettingsError::Invalid(_)
        | SettingsError::Document(_)
        | SettingsError::DocumentShape { .. } => {
            SchedulerError::new("schedule_invalid", error.to_string())
        }
        error => SchedulerError::new("service_unavailable", error.to_string()),
    }
}

pub(crate) async fn read_document(
    shared: &Shared,
    cancel: &CancellationToken,
) -> Result<dim_app_server::settings::SettingsSnapshot<ScheduleDocument>, SchedulerError> {
    shared
        .settings
        .read::<ScheduleDocument>(SettingsScope::User, cancel)
        .await
        .map_err(read_failure)
}

/// The host's configured maximum, the authority a management save captures
/// (§3.3). Read before the settings write.
pub(crate) async fn host_bound(shared: &Shared) -> Result<PermissionPolicy, SchedulerError> {
    shared
        .permissions
        .permission_defaults(Default::default())
        .await
        .map_err(|error| SchedulerError::new("service_unavailable", error.to_string()))
}

async fn dispatch(
    shared: &Arc<Shared>,
    method: &str,
    value: Value,
    caller: Option<MethodCaller>,
    progress: Progress,
    cancel: &CancellationToken,
) -> Result<Value, RpcError> {
    match method {
        "schedule.watch" => {
            let _: ListParams = params(value)?;
            if let Some(caller) = caller {
                shared.watch.watch(caller, progress, cancel).await;
            }
            Ok(json!({}))
        }
        "schedule.list" => {
            let _: ListParams = params(value)?;
            Ok(list(shared, cancel).await?)
        }
        "schedule.save" => {
            let input: SaveParams = params(value)?;
            Ok(save(shared, input, cancel).await?)
        }
        "schedule.delete" => {
            let input: DeleteParams = params(value)?;
            Ok(delete(shared, input, cancel).await?)
        }
        "schedule.set_enabled" => {
            let input: SetEnabledParams = params(value)?;
            Ok(json!({ "job": set_enabled(shared, input, cancel).await? }))
        }
        "schedule.history" => {
            let input: HistoryParams = params(value)?;
            Ok(history(shared, input).await?)
        }
        "schedule.run_now" => {
            let input: RunNowParams = params(value)?;
            let run = crate::engine::accept(shared.clone(), &input.job_id).await?;
            Ok(serde_json::to_value(run).expect("runs serialize"))
        }
        _ => Err(RpcError::unknown_method()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveParams {
    job: ManagementPatch,
    #[serde(default)]
    revision: Option<u64>,
}

/// A management save (TS `managementJob`): a partial job, with the `id` of the
/// job it updates and its `enabled` state. An omitted field keeps the stored
/// value; a new job needs a name, schedule, prompt and cwd.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagementPatch {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule: Option<Schedule>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<ModelSelection>,
    #[serde(default)]
    mode: Option<Mode>,
    #[serde(default)]
    policy: Option<PermissionPreset>,
    #[serde(default)]
    max_run_minutes: Option<u32>,
    #[serde(default)]
    agent_id: Option<String>,
    /// Binds or rebinds a `reuse` job to an existing Session.
    #[serde(default)]
    session_id: Option<String>,
}

/// The fields of a job a save writes. The server owns `created_by`,
/// `policy_cap`, `enabled_at`, `revision` and the job id.
struct JobInput {
    name: String,
    schedule: Schedule,
    prompt: String,
    cwd: String,
    model: Option<ModelSelection>,
    agent_id: Option<String>,
    mode: Option<Mode>,
    /// A management save's explicit binding; `None` keeps a `reuse` job's
    /// binding while it stays `reuse`.
    session_id: Option<String>,
    policy: Option<PermissionPreset>,
    max_run_minutes: Option<u32>,
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteParams {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabledParams {
    job_id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunNowParams {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryParams {
    job_id: String,
    #[serde(default)]
    limit: Option<u32>,
}

fn not_found(job_id: &str) -> SchedulerError {
    SchedulerError::new("schedule_not_found", format!("no job {job_id}"))
}

/// The last window of a job's runs, or the enable anchor before its first
/// window: the first window after it is the job's next window. An `at` job
/// with no run has an open start, so a past instant is recorded `missed`.
pub(crate) fn window_after(job: &Job, runs: &[Run]) -> DateTime<Utc> {
    let last = runs
        .iter()
        .filter(|run| run.job_id == job.id)
        .filter_map(|run| canonical_instant(&run.window))
        .max();
    if last.is_none() && matches!(job.schedule, Schedule::At(_)) {
        return DateTime::<Utc>::MIN_UTC;
    }
    last.or_else(|| {
        job.enabled_at
            .as_deref()
            .and_then(canonical_instant)
            .map(|anchor| anchor - Duration::milliseconds(1))
    })
    .unwrap_or_else(Utc::now)
}

async fn list(shared: &Shared, cancel: &CancellationToken) -> Result<Value, SchedulerError> {
    let document = read_document(shared, cancel).await?.document;
    let runs = runs::read(&shared.files).await?;
    let jobs = document
        .jobs
        .iter()
        .map(|job| {
            let mut own: Vec<&Run> = runs.iter().filter(|run| run.job_id == job.id).collect();
            own.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then(b.run_id.cmp(&a.run_id))
            });
            let (next, issue) = match job.next_window(window_after(job, &runs)) {
                Ok(next) => (next.map(stamp), None),
                Err(error) => (None, Some(error)),
            };
            let mut value = serde_json::to_value(job).expect("jobs serialize");
            value["next_window"] = json!(next);
            if let Some(issue) = issue {
                value["schedule_error"] = json!(issue);
            }
            value["last_run"] = own
                .first()
                .map(|run| serde_json::to_value(run).expect("runs serialize"))
                .unwrap_or(Value::Null);
            value
        })
        .collect::<Vec<_>>();
    Ok(json!({ "jobs": jobs }))
}

/// Builds the job a save stores. The server sets the authority pair and the
/// enable anchor; the operator's preset may only narrow the host bound.
fn build_job(
    input: JobInput,
    previous: Option<&Job>,
    origin: Origin,
    cap: PermissionPolicy,
    now: DateTime<Utc>,
) -> Result<Job, SchedulerError> {
    if input
        .policy
        .as_ref()
        .is_some_and(|preset| preset.rank() > cap.preset.rank())
    {
        return Err(SchedulerError::new(
            "schedule_invalid",
            "Job policy exceeds the saving authority",
        ));
    }
    let enabled = input.enabled.unwrap_or(true);
    let mode = input.mode.unwrap_or(Mode::Create);
    let enabled_at = match (enabled, previous) {
        (false, _) => None,
        (true, Some(before)) if before.enabled => before.enabled_at.clone(),
        (true, _) => Some(stamp(now)),
    };
    let session_id = input.session_id.or_else(|| {
        previous
            .filter(|before| before.mode == mode && mode == Mode::Reuse)
            .and_then(|before| before.session_id.clone())
    });
    let job = Job {
        id: previous
            .map(|before| before.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: input.name,
        schedule: input.schedule,
        prompt: input.prompt,
        cwd: input.cwd,
        model: input.model,
        agent_id: input.agent_id,
        mode,
        session_id,
        policy: input.policy,
        policy_cap: cap,
        enabled,
        enabled_at,
        max_run_minutes: input.max_run_minutes.unwrap_or(DEFAULT_MAX_RUN_MINUTES),
        revision: previous.map_or(1, |before| before.revision + 1),
        created_by: origin,
    };
    job.validate()
        .map_err(|error| SchedulerError::new("schedule_invalid", error.to_string()))?;
    Ok(job)
}

/// Writes a job as one authorizing save (§3.3): the origin and the authority
/// cap are replaced together. `make` builds the saved input from the job it
/// replaces, if any. Returns the saved job and the one it replaced.
async fn save_job(
    shared: &Shared,
    cancel: &CancellationToken,
    id: Option<&str>,
    expected: Option<u64>,
    origin: Origin,
    cap: PermissionPolicy,
    make: impl FnOnce(Option<&Job>) -> Result<JobInput, SchedulerError>,
) -> Result<(Job, Option<Job>), SchedulerError> {
    let snapshot = read_document(shared, cancel).await?;
    let mut document = snapshot.document;
    let position = id
        .map(|id| {
            document
                .jobs
                .iter()
                .position(|job| job.id == id)
                .ok_or_else(|| not_found(id))
        })
        .transpose()?;
    if let (Some(index), Some(expected)) = (position, expected)
        && document.jobs[index].revision != expected
    {
        return Err(SchedulerError::new(
            "schedule_conflict",
            format!("job revision is {}", document.jobs[index].revision),
        ));
    }
    let previous = position.map(|index| document.jobs[index].clone());
    let input = make(previous.as_ref())?;
    let job = build_job(input, previous.as_ref(), origin, cap, Utc::now())?;
    match position {
        Some(index) => document.jobs[index] = job.clone(),
        None => document.jobs.push(job.clone()),
    }
    shared
        .settings
        .update(SettingsScope::User, &snapshot.revision, document, cancel)
        .await
        .map_err(write_failure)?;
    Ok((job, previous))
}

async fn save(
    shared: &Shared,
    input: SaveParams,
    cancel: &CancellationToken,
) -> Result<Value, SchedulerError> {
    let host = host_bound(shared).await?;
    let id = input.job.id.clone();
    let enabled = input.job.enabled;
    let agent_id = input.job.agent_id;
    let session_id = input.job.session_id;
    let patch = JobPatch {
        name: input.job.name,
        schedule: input.job.schedule,
        prompt: input.job.prompt,
        cwd: input.job.cwd,
        model: input.job.model,
        mode: input.job.mode,
        policy: input.job.policy,
        max_run_minutes: input.job.max_run_minutes,
    };
    let (job, previous) = save_job(
        shared,
        cancel,
        id.as_deref(),
        input.revision,
        Origin::User,
        host,
        move |previous| {
            if previous.is_none() && patch.cwd.is_none() {
                return Err(invalid("a new job needs name, schedule, prompt and cwd"));
            }
            let mut saved = merge(previous, patch, "")?;
            if enabled.is_some() {
                saved.enabled = enabled;
            }
            if agent_id.is_some() {
                saved.agent_id = agent_id;
            }
            saved.session_id = session_id;
            Ok(saved)
        },
    )
    .await?;
    Ok(json!({ "transition": transition(previous.as_ref(), &job), "job": job }))
}

/// Removes the job; its terminal history and owned Sessions are retired by
/// the retention pass that follows (A.7), which deletes an owned Session
/// before the last record naming it.
async fn delete(
    shared: &Arc<Shared>,
    input: DeleteParams,
    cancel: &CancellationToken,
) -> Result<Value, SchedulerError> {
    let snapshot = read_document(shared, cancel).await?;
    let mut document = snapshot.document;
    let index = document
        .jobs
        .iter()
        .position(|job| job.id == input.job_id)
        .ok_or_else(|| not_found(&input.job_id))?;
    document.jobs.remove(index);
    shared
        .settings
        .update(SettingsScope::User, &snapshot.revision, document, cancel)
        .await
        .map_err(write_failure)?;
    shared.spawn(crate::engine::prune(shared.clone()));
    Ok(json!({ "job_id": input.job_id }))
}

async fn set_enabled(
    shared: &Shared,
    input: SetEnabledParams,
    cancel: &CancellationToken,
) -> Result<Job, SchedulerError> {
    let snapshot = read_document(shared, cancel).await?;
    let mut document = snapshot.document;
    let index = document
        .jobs
        .iter()
        .position(|job| job.id == input.job_id)
        .ok_or_else(|| not_found(&input.job_id))?;
    let now = Utc::now();
    let job = &mut document.jobs[index];
    job.enabled_at = match (input.enabled, job.enabled) {
        (false, _) => None,
        (true, true) => job.enabled_at.take(),
        (true, false) => Some(stamp(now)),
    };
    job.enabled = input.enabled;
    job.revision += 1;
    let job = job.clone();
    shared
        .settings
        .update(SettingsScope::User, &snapshot.revision, document, cancel)
        .await
        .map_err(write_failure)?;
    Ok(job)
}

async fn history(shared: &Shared, input: HistoryParams) -> Result<Value, SchedulerError> {
    let limit = input.limit.unwrap_or(DEFAULT_HISTORY_LIMIT);
    if !(1..=MAX_HISTORY_LIMIT).contains(&limit) {
        return Err(SchedulerError::new(
            "schedule_invalid",
            format!("limit must be between 1 and {MAX_HISTORY_LIMIT}"),
        ));
    }
    let mut own: Vec<Run> = runs::read(&shared.files)
        .await?
        .into_iter()
        .filter(|run| run.job_id == input.job_id)
        .collect();
    own.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then(b.run_id.cmp(&a.run_id))
    });
    own.truncate(limit as usize);
    Ok(json!({ "runs": own }))
}

/// The `scheduleJobs` service the Session tool calls (design §1.1, A.2).
pub struct ScheduleJobs {
    shared: Arc<Shared>,
}

/// The Session tool's arguments, as the model writes them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    action: ToolAction,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    job: Option<JobPatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolAction {
    List,
    Get,
    Create,
    Update,
    Delete,
    SetEnabled,
    RunNow,
    History,
}

/// The fields a tool `create` or `update` may write.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule: Option<Schedule>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<ModelSelection>,
    #[serde(default)]
    mode: Option<Mode>,
    #[serde(default)]
    policy: Option<PermissionPreset>,
    #[serde(default)]
    max_run_minutes: Option<u32>,
}

/// The saving branch: the origin and authority of a tool save (§3.3).
pub struct Caller {
    pub session_id: String,
    pub branch_id: String,
    pub cwd: String,
}

/// What the model reads (`content`) and what the Session keeps (`records`).
pub struct JobReply {
    pub content: String,
    pub records: Value,
}

const PROMPT_PREVIEW_BYTES: usize = 4 * 1024;
const NAME_PREVIEW_BYTES: usize = 256;
const DETAIL_PREVIEW_BYTES: usize = 512;

fn invalid(message: &str) -> SchedulerError {
    SchedulerError::new("schedule_invalid", message)
}

/// The authority a tool save captures: the saving branch's current policy.
async fn branch_policy(
    shared: &Shared,
    caller: &Caller,
) -> Result<PermissionPolicy, SchedulerError> {
    session_policy(shared, &caller.session_id, &caller.branch_id).await
}

/// The live authority of a branch, read from `sessionPermissions` (§1.3).
pub(crate) async fn session_policy(
    shared: &Shared,
    session_id: &str,
    branch_id: &str,
) -> Result<PermissionPolicy, SchedulerError> {
    let state = shared
        .permissions
        .permission(SessionRef {
            session_id: session_id.to_owned(),
            branch_id: branch_id.to_owned(),
        })
        .await
        .map_err(|error| match error {
            SessionsError::SessionClosed { message } => {
                SchedulerError::new("session_closed", message)
            }
            SessionsError::BranchMissing { message } => {
                SchedulerError::new("branch_missing", message)
            }
            other => SchedulerError::new("service_unavailable", other.to_string()),
        })?;
    Ok(state.policy)
}

/// The result of a first-reuse binding CAS (A.5.1).
pub(crate) enum Binding {
    /// The job now binds the Session at this revision.
    Bound(u64),
    /// The job is no longer eligible; the code is the skip reason.
    Changed(&'static str),
}

/// Writes the job's Session binding under the document CAS. The bound job's
/// revision advances, so a run accepted at the old revision is rechecked.
/// A stale document is `schedule_conflict`.
pub(crate) async fn bind_job(
    shared: &Shared,
    job: &Job,
    session_id: &str,
) -> Result<Binding, SchedulerError> {
    let cancel = CancellationToken::new();
    let snapshot = read_document(shared, &cancel).await?;
    let mut document = snapshot.document;
    let Some(current) = document
        .jobs
        .iter_mut()
        .find(|current| current.id == job.id)
    else {
        return Ok(Binding::Changed("changed"));
    };
    if !current.enabled {
        return Ok(Binding::Changed("disabled"));
    }
    if current.revision != job.revision
        || current.mode != Mode::Reuse
        || current.session_id.is_some()
    {
        return Ok(Binding::Changed("changed"));
    }
    current.session_id = Some(session_id.to_owned());
    current.revision += 1;
    let revision = current.revision;
    #[cfg(test)]
    crate::binding_flow::pause(&job.id).await;
    shared
        .settings
        .update(SettingsScope::User, &snapshot.revision, document, &cancel)
        .await
        .map_err(write_failure)?;
    Ok(Binding::Bound(revision))
}

/// The saved input of a tool create or update. An omitted field keeps the
/// stored value; an omitted policy is kept as stored, and the enable state is
/// left to `set_enabled`.
fn merge(
    previous: Option<&Job>,
    patch: JobPatch,
    caller_cwd: &str,
) -> Result<JobInput, SchedulerError> {
    match previous {
        None => {
            let (Some(name), Some(schedule), Some(prompt)) =
                (patch.name, patch.schedule, patch.prompt)
            else {
                return Err(invalid("a new job needs name, schedule and prompt"));
            };
            Ok(JobInput {
                name,
                schedule,
                prompt,
                cwd: patch.cwd.unwrap_or_else(|| caller_cwd.to_owned()),
                model: patch.model,
                agent_id: None,
                mode: patch.mode,
                session_id: None,
                policy: patch.policy,
                max_run_minutes: patch.max_run_minutes,
                enabled: Some(true),
            })
        }
        Some(before) => Ok(JobInput {
            name: patch.name.unwrap_or_else(|| before.name.clone()),
            schedule: patch.schedule.unwrap_or_else(|| before.schedule.clone()),
            prompt: patch.prompt.unwrap_or_else(|| before.prompt.clone()),
            cwd: patch.cwd.unwrap_or_else(|| before.cwd.clone()),
            model: patch.model.or_else(|| before.model.clone()),
            agent_id: before.agent_id.clone(),
            mode: Some(patch.mode.unwrap_or_else(|| before.mode.clone())),
            session_id: None,
            policy: patch.policy.or(before.policy),
            max_run_minutes: Some(patch.max_run_minutes.unwrap_or(before.max_run_minutes)),
            enabled: Some(before.enabled),
        }),
    }
}

/// One side of a save's authority transition (§3.3): who saved the job, its
/// cap, the policy its runs request, and where their reports go.
fn authority(job: Option<&Job>) -> Value {
    job.map_or(Value::Null, |job| {
        json!({
            "origin": job.created_by,
            "cap": job.policy_cap,
            "effective_policy": crate::engine::requested_policy(job).cap(&job.policy_cap),
            "report_to": match &job.created_by {
                Origin::User => Value::Null,
                Origin::Tool { session_id, branch_id } =>
                    json!({ "session_id": session_id, "branch_id": branch_id }),
            },
        })
    })
}

/// The old-to-new authority transition a save returns (§3.3).
fn transition(previous: Option<&Job>, job: &Job) -> Value {
    json!({ "from": authority(previous), "to": authority(Some(job)) })
}

/// A run as the model reads it: the reason bounded (§A.3).
fn run_preview(run: &Value) -> Value {
    let mut run = run.clone();
    if let Some(detail) = run["detail"].as_str() {
        run["detail"] = json!(truncate(detail, DETAIL_PREVIEW_BYTES));
    }
    run
}

/// A job as the model reads it: the name and prompt bounded (§A.3).
fn job_preview(job: &Job) -> Value {
    let mut value = serde_json::to_value(job).expect("jobs serialize");
    let prompt = truncate(&job.prompt, PROMPT_PREVIEW_BYTES);
    value["prompt_truncated"] = json!(prompt.len() < job.prompt.len());
    value["prompt"] = json!(prompt);
    value["name"] = json!(truncate(&job.name, NAME_PREVIEW_BYTES));
    value
}

impl ScheduleJobs {
    /// Serves one tool call from the Session `caller` names.
    pub async fn call(
        &self,
        request: ToolRequest,
        caller: Caller,
    ) -> Result<JobReply, SchedulerError> {
        let shared = &*self.shared;
        let cancel = CancellationToken::new();
        match request.action {
            ToolAction::Create | ToolAction::Update => {
                let patch = request.job.ok_or_else(|| invalid("job is required"))?;
                let id = match request.action {
                    ToolAction::Update => Some(
                        request
                            .job_id
                            .clone()
                            .ok_or_else(|| invalid("job_id is required"))?,
                    ),
                    _ => None,
                };
                let cap = branch_policy(shared, &caller).await?;
                let origin = Origin::Tool {
                    session_id: caller.session_id.clone(),
                    branch_id: caller.branch_id.clone(),
                };
                let cwd = caller.cwd.clone();
                let (job, previous) = save_job(
                    shared,
                    &cancel,
                    id.as_deref(),
                    None,
                    origin,
                    cap,
                    move |previous| merge(previous, patch, &cwd),
                )
                .await?;
                let transition = transition(previous.as_ref(), &job);
                Ok(JobReply {
                    content: json!({ "job": job_preview(&job), "transition": transition })
                        .to_string(),
                    records: json!({ "job": job, "transition": transition }),
                })
            }
            ToolAction::Get => {
                let job_id = request
                    .job_id
                    .ok_or_else(|| invalid("job_id is required"))?;
                let document = read_document(shared, &cancel).await?.document;
                let job = document
                    .jobs
                    .iter()
                    .find(|job| job.id == job_id)
                    .ok_or_else(|| not_found(&job_id))?;
                Ok(JobReply {
                    content: json!({ "job": job_preview(job) }).to_string(),
                    records: json!({ "job": job }),
                })
            }
            ToolAction::List => {
                let value = list(shared, &cancel).await?;
                let jobs = value["jobs"].as_array().into_iter().flatten().map(|job| {
                    let mut job = job.clone();
                    if let Some(object) = job.as_object_mut() {
                        object.remove("prompt");
                    }
                    if let Some(name) = job["name"].as_str() {
                        job["name"] = json!(truncate(name, NAME_PREVIEW_BYTES));
                    }
                    if let Some(issue) = job["schedule_error"].as_str() {
                        job["schedule_error"] = json!(truncate(issue, DETAIL_PREVIEW_BYTES));
                    }
                    if !job["last_run"].is_null() {
                        job["last_run"] = run_preview(&job["last_run"]);
                    }
                    job
                });
                Ok(JobReply {
                    content: json!({ "jobs": jobs.collect::<Vec<_>>() }).to_string(),
                    records: value,
                })
            }
            ToolAction::Delete => {
                let job_id = request
                    .job_id
                    .ok_or_else(|| invalid("job_id is required"))?;
                let value = delete(&self.shared, DeleteParams { job_id }, &cancel).await?;
                Ok(JobReply {
                    content: value.to_string(),
                    records: value,
                })
            }
            ToolAction::SetEnabled => {
                let job_id = request
                    .job_id
                    .ok_or_else(|| invalid("job_id is required"))?;
                let enabled = request
                    .enabled
                    .ok_or_else(|| invalid("enabled is required"))?;
                let job =
                    set_enabled(shared, SetEnabledParams { job_id, enabled }, &cancel).await?;
                Ok(JobReply {
                    content: json!({ "job": job_preview(&job) }).to_string(),
                    records: json!({ "job": job }),
                })
            }
            ToolAction::History => {
                let job_id = request
                    .job_id
                    .ok_or_else(|| invalid("job_id is required"))?;
                let value = history(
                    shared,
                    HistoryParams {
                        job_id,
                        limit: request.limit,
                    },
                )
                .await?;
                let runs = value["runs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(run_preview);
                Ok(JobReply {
                    content: json!({ "runs": runs.collect::<Vec<_>>() }).to_string(),
                    records: value,
                })
            }
            ToolAction::RunNow => {
                let job_id = request
                    .job_id
                    .ok_or_else(|| invalid("job_id is required"))?;
                let run = crate::engine::accept(self.shared.clone(), &job_id).await?;
                let value = serde_json::to_value(&run).expect("runs serialize");
                Ok(JobReply {
                    content: run_preview(&value).to_string(),
                    records: value,
                })
            }
        }
    }
}

fn truncate(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
