//! The process owner: the `schedule` management namespace over the `schedule`
//! settings document and the delivery history file ([design §1.3, A.3](../../../docs/design/plugins/Scheduler.md)).
use crate::{
    clock::Clock,
    document::{Job, MAX_TOOL_JOBS_PER_SESSION, Mode, Origin, Schedule, Target, canonical_instant},
    loops::Lookup,
    runs::{self, Delivery, DeliveryState, SchedulerError, SessionOrigin, stamp},
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
use dim_agent_types::InputMode;
use dim_app_server::{
    ProcessActivity,
    methods::ManagementMethods,
    plugin_files::PluginFileTransactions,
    settings::{BusinessDocument, SettingsError, SettingsService},
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reference, Reply, Value as BridgeValue, host_key,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const NAMESPACE: &str = "schedule";
const DEFAULT_HISTORY_LIMIT: u32 = 20;
const MAX_HISTORY_LIMIT: u32 = 50;

pub(crate) type DeliveryHolds =
    Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<dim_app_server::ProcessHold>>>>;

/// Process owner of the `schedule` namespace.
pub struct SchedulerPlugin {
    activity: Key<ProcessActivity>,
    pub(crate) deliveries: DeliveryHolds,
    settings: Key<SettingsService>,
    files: Key<PluginFileTransactions>,
    methods: Key<ManagementMethods>,
    sessions: Key<dyn HostDispatch>,
    parts: Key<PartRegistry>,
    texts: Arc<crate::SchedulerTexts>,
    clock: Arc<dyn Clock>,
    #[cfg(feature = "testing")]
    holds: Option<Arc<crate::testing::Holds>>,
}

impl SchedulerPlugin {
    pub fn new(names: &Names<'_>, texts: Arc<crate::SchedulerTexts>) -> rutis::Result<Self> {
        Ok(Self {
            activity: names.key()?,
            deliveries: Default::default(),
            settings: names.key()?,
            files: names.key()?,
            methods: names.key()?,
            sessions: host_key("sessions"),
            parts: names.key()?,
            texts,
            clock: Arc::new(crate::clock::SystemClock),
            #[cfg(feature = "testing")]
            holds: None,
        })
    }

    /// [`SchedulerPlugin::new`] with the flow tests' holds (feature
    /// `testing`).
    #[cfg(feature = "testing")]
    pub fn new_with_testing(
        names: &Names<'_>,
        texts: Arc<crate::SchedulerTexts>,
        holds: Arc<crate::testing::Holds>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            holds: Some(holds),
            ..Self::new(names, texts)?
        })
    }
}

impl DynPlugin for SchedulerPlugin {
    fn name(&self) -> &str {
        "Scheduler"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.activity.erase(),
            self.settings.erase(),
            self.files.erase(),
            self.methods.erase(),
            self.sessions.erase(),
            self.parts.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let activity = ctx.require_at(&self.activity)?;
            let startup = Arc::new(activity.take_hold().ok_or_else(|| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    "Process admission is paused".into(),
                ))
            })?);
            let settings = ctx.require_at(&self.settings)?;
            let files = ctx.require_at(&self.files)?;
            let methods = ctx.require_at(&self.methods)?;
            let sessions = ctx.require_at(&self.sessions)?;
            settings.register::<crate::document::ScheduleDocument>(ctx)?;
            let shared = Arc::new(Shared {
                ctx: ctx.clone(),
                activity,
                clock: self.clock.clone(),
                deliveries: self.deliveries.clone(),
                sessions,
                settings,
                files,
                pruning: tokio::sync::Mutex::new(()),
                settling: tokio::sync::Mutex::new(()),
                wakeups: tokio::sync::Mutex::new(()),
                loops: tokio::sync::Mutex::new(()),
                live_loops: std::sync::atomic::AtomicBool::new(false),
                loop_lookup: std::sync::atomic::AtomicBool::new(false),
                stop: ctx.cancellation_token(),
                tasks: TaskTracker::new(),
                retention: Default::default(),
                watch: crate::watch::Watchers::default(),
                #[cfg(feature = "testing")]
                holds: self.holds.clone(),
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
                    .name(settings_changes(
                        crate::document::ScheduleDocument::NAMESPACE,
                    ))
                    .on(move |_, _| {
                        let shared = shared.clone();
                        async move {
                            shared.watch.jobs();
                            Ok(None)
                        }
                    })?;
            }
            let jobs = Arc::new(ScheduleJobs {
                shared: shared.clone(),
            });
            ctx.provide_as::<ScheduleJobs>(jobs.clone())?;
            ctx.provide_at(host_key("scheduleJobs"), Arc::new(ScheduleJobsFace(jobs)))?;
            let wakeups = Arc::new(crate::wakeups::Wakeups {
                shared: shared.clone(),
            });
            ctx.provide_as::<crate::wakeups::Wakeups>(wakeups.clone())?;
            ctx.provide_at(
                host_key("wakeups"),
                Arc::new(crate::wakeups::WakeupsFace(wakeups.clone())),
            )?;
            let loops = Arc::new(crate::loops::Loops {
                shared: shared.clone(),
            });
            ctx.provide_as::<crate::loops::Loops>(loops.clone())?;
            ctx.provide_at(
                host_key("loops"),
                Arc::new(crate::loops::LoopsFace(loops.clone())),
            )?;
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
                    format!("scheduler delivery history: {error}").into(),
                ))
            })?;
            {
                let mut holds = shared.deliveries.lock().expect("delivery admission");
                holds.retain(|id, _| {
                    active
                        .iter()
                        .any(|run| run.run_id == *id && run.state == DeliveryState::Delivering)
                });
                for run in active
                    .iter()
                    .filter(|run| run.state == DeliveryState::Delivering)
                {
                    holds
                        .entry(run.run_id.clone())
                        .or_insert_with(|| startup.clone());
                }
            }
            // A wakeup file that is not valid v1 fails the row like an invalid
            // delivery history; the bytes stay unchanged (A.7's rule).
            crate::wakeups::read(&shared.files).await.map_err(|error| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    format!("scheduler wakeups: {error}").into(),
                ))
            })?;
            // The same rule for a loop file: an invalid v1 fails the row and
            // the bytes stay unchanged. This read is also where the live-loop
            // flag starts: a daemon whose file holds no live loop takes no
            // loop work at all.
            let looped = crate::loops::read(&shared.files).await.map_err(|error| {
                rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    format!("scheduler loops: {error}").into(),
                ))
            })?;
            loops.note_live(&looped);
            // A delivery still in flight when the previous process stopped
            // keeps its record; only a launch record this design wrote has a
            // stop timer to resume (§3.2). A record written by an earlier
            // build in `launching`, `submitted` or `running` is left as
            // written (A.7).
            crate::engine::recover(
                &shared,
                active
                    .iter()
                    .filter(|run| run.has_stop_timer())
                    .cloned()
                    .collect(),
            );
            // Every `delivering` record is settled by lookup at the next
            // start, before any window writes again (A.6). A lookup that
            // answers nothing leaves the round unsettled for as long as the
            // answer is missing, so the timer repeats the pass on its next
            // tick rather than waiting for the minute-long one (§12.1).
            {
                let startup = shared.clone();
                let loops_startup = loops.clone();
                shared.spawn(async move {
                    crate::engine::settle_delivering(&startup).await;
                    note_loop_lookup(&startup, loops_startup.settle_delivering().await);
                });
            }
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
                    if let Err(error) = loops.evaluate().await {
                        tracing::warn!(%error, "scheduler loop pass failed");
                    }
                    ticks += 1;
                    let minute = ticks.is_multiple_of(60);
                    if minute {
                        crate::engine::settle_delivering(&timer).await;
                    }
                    if minute || timer.loop_lookup_pending() {
                        note_loop_lookup(&timer, loops.settle_delivering().await);
                    }
                    if minute && timer.retention_pending() {
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
    pub(crate) activity: Arc<ProcessActivity>,
    /// The instant this generation reads and the waits it crosses (#1536).
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) deliveries: DeliveryHolds,
    pub(crate) sessions: Arc<dyn HostDispatch>,
    pub(crate) settings: Arc<SettingsService>,
    pub(crate) files: Arc<PluginFileTransactions>,
    /// Serializes retention passes, so a pass never deletes what another reads.
    pub(crate) pruning: tokio::sync::Mutex<()>,
    /// Serializes lookup passes over `delivering` records (A.6).
    pub(crate) settling: tokio::sync::Mutex<()>,
    /// Serializes a wakeup cancel and the whole timer pass — the snapshot
    /// read, the input writes and the settle batch — so the two order at the
    /// record level: a cancel that takes it first prevents the input, one
    /// that takes it after reports the settled state (design §10.3).
    pub(crate) wakeups: tokio::sync::Mutex<()>,
    /// Serializes every read-modify-write of the loop records and every
    /// refresh of `live_loops`: the timer pass, recovery, `start` and `change`
    /// for `next` and `stop` hold it for the whole operation, so they are one
    /// serial order at the record level (design §12.3), and the flag can only
    /// ever store a snapshot no later write has superseded.
    ///
    /// A holder may await only these external calls, and none of them takes
    /// this lock: `sessions.pending` reads the branch's InputQueue,
    /// `sessions.lookup` reads the Timeline by submission id, and
    /// `sessions.input` in queue mode returns once the input is published.
    /// Reads and writes of the loop file through `pluginFiles` never take it
    /// either. A holder must never await a round's turn, or anything else
    /// that can take this lock; a new external call under it joins this list
    /// first.
    pub(crate) loops: tokio::sync::Mutex<()>,
    /// Whether any live loop exists. A daemon with none does no loop work, so
    /// the pass takes no process hold and reads no file (design 12.2). Every
    /// write under `loops` recomputes it.
    pub(crate) live_loops: std::sync::atomic::AtomicBool,
    /// Whether the last loop lookup pass left a round whose answer says the
    /// service is not ready yet. While it holds, the timer runs the pass again
    /// on its next tick instead of a minute later (§12.1).
    pub(crate) loop_lookup: std::sync::atomic::AtomicBool,
    /// Cancelled when this generation starts to unload.
    pub(crate) stop: CancellationToken,
    /// The timer, delivery tasks and retention passes; cleanup waits for them.
    pub(crate) tasks: TaskTracker,
    /// Whether the last retention pass left expired records or could not
    /// read; the timer retries it once a minute (A.7).
    pub(crate) retention: std::sync::atomic::AtomicBool,
    /// The open `schedule.watch` calls of this generation (design §11.2).
    pub(crate) watch: crate::watch::Watchers,
    /// The flow tests' holds (feature `testing`).
    #[cfg(feature = "testing")]
    pub(crate) holds: Option<Arc<crate::testing::Holds>>,
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

    /// Whether the last loop lookup pass settled on [`Lookup::NextTick`], so the
    /// timer repeats it on its next tick instead of a minute later.
    pub(crate) fn loop_lookup_pending(&self) -> bool {
        self.loop_lookup.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Waits `delay`; `false` once the owner is unloading, when the caller
    /// stops and leaves an unsettled delivery to the next generation's
    /// recovery.
    pub(crate) async fn pause(&self, delay: std::time::Duration) -> bool {
        self.clock
            .wait_until(self.now() + Duration::from_std(delay).unwrap(), &self.stop)
            .await
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }
}

/// Records the cadence the loop lookup pass settled on: only a round whose
/// answer says the service is not ready yet keeps the retry pending, so the
/// timer runs the pass on its next tick instead of a minute later (§12.1). Any
/// other unresolved answer waits for the minute pass, and a pass that failed as
/// a whole does the same.
fn note_loop_lookup(shared: &Shared, answer: Result<Lookup, SchedulerError>) {
    let tick = match answer {
        Ok(Lookup::NextTick) => true,
        Ok(Lookup::Settled | Lookup::NextMinute) => false,
        Err(error) => {
            tracing::warn!(%error, "scheduler loop lookup pass failed");
            false
        }
    };
    shared
        .loop_lookup
        .store(tick, std::sync::atomic::Ordering::Release);
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
) -> Result<
    dim_app_server::settings::SettingsSnapshot<crate::document::ScheduleDocument>,
    SchedulerError,
> {
    shared
        .settings
        .read::<crate::document::ScheduleDocument>(SettingsScope::User, cancel)
        .await
        .map_err(read_failure)
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
            Ok(json!({ "job": set_enabled(shared, input, cancel).await?.reported() }))
        }
        "schedule.history" => {
            let input: HistoryParams = params(value)?;
            Ok(history(shared, input).await?)
        }
        "schedule.run_now" => {
            let input: RunNowParams = params(value)?;
            let run = crate::engine::accept(shared.clone(), &input.job_id).await?;
            Ok(serde_json::to_value(run).expect("deliveries serialize"))
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

/// A management save: a partial job, with the `id` of the job it updates.
/// An omitted field keeps the stored value; a new job needs a name, schedule,
/// prompt and target (A.3). `target` replaces the stored target whole when
/// present.
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
    target: Option<Target>,
}

/// The fields of a job a save writes. The server owns `created_by`,
/// `disabled_reason`, `enabled_at`, `revision` and the job id.
struct JobInput {
    name: String,
    schedule: Schedule,
    prompt: String,
    /// `None` keeps the stored target (a legacy job stays legacy).
    target: Option<Target>,
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

/// The last window of a job's deliveries, or the enable anchor before its
/// first window: the first window after it is the job's next window. An `at`
/// job with no delivery has an open start, so a past instant is recorded
/// `missed`.
pub(crate) fn window_after(shared: &Shared, job: &Job, runs: &[Delivery]) -> DateTime<Utc> {
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
    .unwrap_or_else(|| shared.now())
}

async fn list(shared: &Shared, cancel: &CancellationToken) -> Result<Value, SchedulerError> {
    let document = read_document(shared, cancel).await?.document;
    let runs = runs::read(&shared.files).await?;
    let jobs = document
        .jobs
        .iter()
        .map(|job| {
            let mut own: Vec<&Delivery> = runs.iter().filter(|run| run.job_id == job.id).collect();
            own.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then(b.run_id.cmp(&a.run_id))
            });
            let (next, issue) = match job.next_window(window_after(shared, job, &runs)) {
                Ok(next) => (next.map(stamp), None),
                Err(error) => (None, Some(error)),
            };
            let mut value = job.reported();
            value["next_window"] = json!(next);
            if let Some(issue) = issue {
                value["schedule_error"] = json!(issue);
            }
            value["last_run"] = own
                .first()
                .map(|run| serde_json::to_value(run).expect("deliveries serialize"))
                .unwrap_or(Value::Null);
            value
        })
        .collect::<Vec<_>>();
    Ok(json!({ "jobs": jobs }))
}

/// Builds the job a save stores. The server sets the target, the origin and
/// the enable anchor; a save preserves the legacy fields of the job it
/// replaces and clears its disabled reason. No save captures authority.
fn build_job(
    input: JobInput,
    previous: Option<&Job>,
    origin: Origin,
    now: DateTime<Utc>,
) -> Result<Job, SchedulerError> {
    let target = input
        .target
        .or_else(|| previous.and_then(|before| before.target.clone()));
    // `policy_cap` is a legacy field no save writes: the launch route applies
    // the live host bound, so nothing is captured at save (§3.3, A.1).
    let policy_cap = previous.and_then(|before| before.policy_cap.clone());
    let enabled = input
        .enabled
        .unwrap_or_else(|| previous.is_none_or(|before| before.enabled));
    let enabled_at = match (enabled, previous) {
        (false, _) => None,
        (true, Some(before)) if before.enabled => before.enabled_at.clone(),
        (true, _) => Some(stamp(now)),
    };
    let job = Job {
        id: previous
            .map(|before| before.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: input.name,
        schedule: input.schedule,
        prompt: input.prompt,
        target,
        disabled_reason: None,
        enabled,
        enabled_at,
        revision: previous.map_or(1, |before| before.revision + 1),
        created_by: previous
            .map(|before| before.created_by.clone())
            .unwrap_or(origin),
        mode: previous.and_then(|before| before.mode.clone()),
        session_id: previous.and_then(|before| before.session_id.clone()),
        cwd: previous.and_then(|before| before.cwd.clone()),
        model: previous.and_then(|before| before.model.clone()),
        agent_id: previous.and_then(|before| before.agent_id.clone()),
        policy: previous.and_then(|before| before.policy),
        policy_cap,
        max_run_minutes: previous.and_then(|before| before.max_run_minutes),
    };
    job.validate()
        .map_err(|error| SchedulerError::new("schedule_invalid", error.to_string()))?;
    Ok(job)
}

/// A save that replaces a legacy `reuse` job's target, or the deletion of a
/// job whose target replaced that binding, releases the Session the binding
/// launched (§2.3, A.7): the job's compatibility field keeps its bytes, and
/// every delivery record keeps its place, with only the launch ownership
/// retired so retention never deletes the released Session.
async fn release_owned_runs(shared: &Shared, job: &Job) -> Result<(), SchedulerError> {
    if job.mode != Some(Mode::Reuse) {
        return Ok(());
    }
    let Some(session_id) = job.session_id.clone() else {
        return Ok(());
    };
    #[cfg(feature = "testing")]
    if shared
        .holds
        .as_ref()
        .is_some_and(|holds| holds.take_release_failure())
    {
        return Err(SchedulerError::new("io", "injected release failure"));
    }
    let job_id = job.id.clone();
    runs::update(&shared.files, move |history| {
        let retire: HashSet<String> = history
            .iter()
            .filter(|run| {
                run.job_id == job_id
                    && run.session_origin == Some(SessionOrigin::Launch)
                    && run.session_id.as_deref() == Some(session_id.as_str())
            })
            .map(|run| run.run_id.clone())
            .collect();
        Ok((runs::retire_launch_ownership(history, &retire), ()))
    })
    .await?;
    Ok(())
}

/// Writes a job as one authorizing save. `admit` runs on the document the
/// write is based on, so an admission bound and the write share one read.
/// `make` builds the saved input from the job it replaces, if any. Returns
/// the saved job and the one it replaced.
#[allow(clippy::too_many_arguments)]
async fn save_job(
    shared: &Shared,
    cancel: &CancellationToken,
    id: Option<&str>,
    expected: Option<u64>,
    origin: Origin,
    admit: impl FnOnce(&[Job]) -> Result<(), SchedulerError>,
    make: impl FnOnce(Option<&Job>) -> Result<JobInput, SchedulerError>,
) -> Result<(Job, Option<Job>), SchedulerError> {
    let snapshot = read_document(shared, cancel).await?;
    let mut document = snapshot.document;
    admit(&document.jobs)?;
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
    let job = build_job(input, previous.as_ref(), origin, shared.now())?;
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
    shared: &Arc<Shared>,
    input: SaveParams,
    cancel: &CancellationToken,
) -> Result<Value, SchedulerError> {
    let id = input.job.id.clone();
    let (job, previous) = save_job(
        shared,
        cancel,
        id.as_deref(),
        input.revision,
        Origin::User,
        |_| Ok(()),
        move |previous| merge(previous, input.job),
    )
    .await?;
    // Only the save that replaced the binding releases it: a later save that
    // keeps the stored target has nothing to retire, so it does not read the
    // delivery file or risk a release failure (A.7). A release that fails on
    // the replacing save still wakes a retention pass, which re-derives it
    // from the saved document. The settings write has committed, so the reply
    // reports that save even when the release fails.
    if job.replaced_reuse_binding()
        && previous
            .as_ref()
            .is_none_or(|before| before.target != job.target)
        && let Err(error) = release_owned_runs(shared, &job).await
    {
        tracing::warn!(%error, "scheduler ownership release failed");
        shared.spawn(crate::engine::prune(shared.clone()));
    }
    Ok(json!({ "job": job.reported() }))
}

/// The saved input of a management save: an omitted field keeps the stored
/// value, and a new job needs a name, schedule, prompt and target (A.3).
fn merge(previous: Option<&Job>, patch: ManagementPatch) -> Result<JobInput, SchedulerError> {
    let ManagementPatch {
        name,
        schedule,
        prompt,
        target,
        enabled,
        ..
    } = patch;
    match previous {
        None => {
            let (Some(name), Some(schedule), Some(prompt), Some(target)) =
                (name, schedule, prompt, target)
            else {
                return Err(invalid(
                    "a new job needs a name, a schedule, a prompt and a target",
                ));
            };
            Ok(JobInput {
                name,
                schedule,
                prompt,
                target: Some(target),
                enabled,
            })
        }
        Some(before) => Ok(JobInput {
            name: name.unwrap_or_else(|| before.name.clone()),
            schedule: schedule.unwrap_or_else(|| before.schedule.clone()),
            prompt: prompt.unwrap_or_else(|| before.prompt.clone()),
            target: target.or_else(|| before.target.clone()),
            enabled,
        }),
    }
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
    let job = document.jobs[index].clone();
    // A job whose target replaced its legacy reuse binding no longer owns
    // the Session that binding launched; releasing before the document goes
    // is correct even when this delete fails, since the target already
    // released the binding.
    if job.replaced_reuse_binding() {
        release_owned_runs(shared, &job).await?;
    }
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
    let now = shared.now();
    let job = &mut document.jobs[index];
    job.enabled_at = match (input.enabled, job.enabled) {
        (false, _) => None,
        (true, true) => job.enabled_at.take(),
        (true, false) => Some(stamp(now)),
    };
    job.enabled = input.enabled;
    if input.enabled {
        job.disabled_reason = None;
    }
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
    let mut own: Vec<Delivery> = runs::read(&shared.files)
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

/// The fields a tool `create` or `update` may write (A.2): no target, cwd,
/// model, Agent or policy. A create fills the target from the calling
/// Session and branch.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule: Option<Schedule>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    input_mode: Option<InputMode>,
}

/// The saving branch: the target a tool create saves (A.2, §2.1).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// The saved input of a tool create or update: a create targets the calling
/// Session and branch; an update keeps the stored target and may change a
/// session target's `input_mode` (A.2).
fn tool_merge(
    previous: Option<&Job>,
    patch: JobPatch,
    caller: &Caller,
) -> Result<JobInput, SchedulerError> {
    let JobPatch {
        name,
        schedule,
        prompt,
        input_mode,
    } = patch;
    match previous {
        None => {
            let (Some(name), Some(schedule), Some(prompt)) = (name, schedule, prompt) else {
                return Err(invalid("a new job needs a name, a schedule and a prompt"));
            };
            Ok(JobInput {
                name,
                schedule,
                prompt,
                target: Some(Target::Session {
                    session_id: caller.session_id.clone(),
                    branch_id: caller.branch_id.clone(),
                    input_mode: input_mode.unwrap_or_default(),
                }),
                enabled: Some(true),
            })
        }
        Some(before) => {
            let target = match (input_mode, before.effective_target()) {
                (
                    Some(input_mode),
                    Some(Target::Session {
                        session_id,
                        branch_id,
                        ..
                    }),
                ) => Some(Target::Session {
                    session_id,
                    branch_id,
                    input_mode,
                }),
                _ => before.target.clone(),
            };
            Ok(JobInput {
                name: name.unwrap_or_else(|| before.name.clone()),
                schedule: schedule.unwrap_or_else(|| before.schedule.clone()),
                prompt: prompt.unwrap_or_else(|| before.prompt.clone()),
                target,
                enabled: Some(before.enabled),
            })
        }
    }
}

/// A delivery as the model reads it: the reason bounded (A.2).
fn run_preview(run: &Value) -> Value {
    let mut run = run.clone();
    if let Some(detail) = run["detail"].as_str() {
        run["detail"] = json!(truncate(detail, DETAIL_PREVIEW_BYTES));
    }
    run
}

/// A job as the model reads it: the name and prompt bounded, no legacy
/// fields (A.2).
fn job_preview(job: &Job) -> Value {
    let mut value = job.reported();
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
        let _activity = shared.activity.take_hold().ok_or_else(|| {
            SchedulerError::new("service_reloading", "Process admission is paused")
        })?;
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
                let create = request.action == ToolAction::Create;
                let origin = Origin::Tool {
                    session_id: caller.session_id.clone(),
                    branch_id: caller.branch_id.clone(),
                };
                let session_id = caller.session_id.clone();
                let (job, _previous) = save_job(
                    shared,
                    &cancel,
                    id.as_deref(),
                    None,
                    origin,
                    move |jobs| {
                        if !create {
                            return Ok(());
                        }
                        let own = jobs
                            .iter()
                            .filter(|job| {
                                matches!(
                                    &job.created_by,
                                    Origin::Tool { session_id: saved, .. } if *saved == session_id
                                )
                            })
                            .count();
                        if own >= MAX_TOOL_JOBS_PER_SESSION {
                            return Err(invalid("a Session may save at most 16 jobs"));
                        }
                        Ok(())
                    },
                    move |previous| tool_merge(previous, patch, &caller),
                )
                .await?;
                Ok(JobReply {
                    content: json!({ "job": job_preview(&job) }).to_string(),
                    records: json!({ "job": job.reported() }),
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
                    records: json!({ "job": job.reported() }),
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
                    records: json!({ "job": job.reported() }),
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
                let value = serde_json::to_value(&run).expect("deliveries serialize");
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

pub(crate) struct ScheduleJobsFace(Arc<ScheduleJobs>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobRequestWire {
    action: ToolAction,
    #[serde(default, rename = "job_id")]
    job_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    job: Option<JobPatch>,
    caller: Caller,
}

impl HostDispatch for ScheduleJobsFace {
    fn methods(&self) -> Option<Value> {
        Some(json!({ "call": "async" }))
    }

    fn invoke(&self, method: &str, args: BridgeValue) -> Reply {
        if method != "call" {
            return Err(BridgeError::Value(format!(
                "unknown scheduleJobs method {method}"
            )));
        }
        let (request, cancelled) = match face_arguments::<JobRequestWire>(args, "schedule_invalid")
        {
            Ok(args) => args,
            Err(error) => return face_failure(error),
        };
        let jobs = self.0.clone();
        let interruptible = matches!(
            request.action,
            ToolAction::List | ToolAction::Get | ToolAction::History
        );
        Ok(face_call(
            &jobs.shared.clone(),
            cancelled,
            interruptible,
            async move {
                let reply = jobs
                    .call(
                        ToolRequest {
                            action: request.action,
                            job_id: request.job_id,
                            enabled: request.enabled,
                            limit: request.limit,
                            job: request.job,
                        },
                        request.caller,
                    )
                    .await?;
                Ok(json!({ "content": reply.content, "records": reply.records }))
            },
        ))
    }
}

pub(crate) fn face_arguments<T: DeserializeOwned>(
    args: BridgeValue,
    code: &'static str,
) -> Result<(T, Option<Reference>), SchedulerError> {
    let invalid = |error: String| SchedulerError::new(code, error);
    let mut args = args
        .list()
        .map_err(|error| invalid(error.to_string()))?
        .into_iter();
    let request = args
        .next()
        .ok_or_else(|| invalid("request is required".into()))?;
    let cancelled = match args.next() {
        None | Some(BridgeValue::Signal) => None,
        Some(BridgeValue::Reference(reference)) if reference.is_future() => Some(reference),
        Some(_) => return Err(invalid("expected a cancellation signal or promise".into())),
    };
    if args.next().is_some() {
        return Err(invalid("too many arguments".into()));
    }
    let request = request.json().map_err(|error| invalid(error.to_string()))?;
    Ok((
        serde_json::from_value(request).map_err(|error| invalid(error.to_string()))?,
        cancelled,
    ))
}

pub(crate) fn face_failure(error: SchedulerError) -> Reply {
    Ok(json!({"code": error.code, "message": error.message}).into())
}

pub(crate) fn face_call(
    shared: &Arc<Shared>,
    cancelled: Option<Reference>,
    interruptible: bool,
    operation: impl Future<Output = Result<serde_json::Value, SchedulerError>> + Send + 'static,
) -> BridgeValue {
    let cancel = shared.stop.child_token();
    let guard = cancel.clone().drop_guard();
    let stopped = cancel.clone();
    let shared = shared.clone();
    BridgeValue::cancellable_future(async move {
        let _guard = guard;
        let signal = async {
            match cancelled {
                Some(reference) => {
                    let _ = reference.wait_async().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let task = async move {
            let task = shared.tasks.spawn(async move {
                if stopped.is_cancelled() {
                    return Err(SchedulerError::new("cancelled", "call cancelled before handover"));
                }
                if interruptible {
                    tokio::select! {
                        biased;
                        _ = stopped.cancelled() => Err(SchedulerError::new("cancelled", "call cancelled")),
                        result = operation => result,
                    }
                } else {
                    operation.await
                }
            });
            task.await
        };
        let mut task = std::pin::pin!(task);
        let result = tokio::select! {
            biased;
            _ = signal => {
                cancel.cancel();
                task.await
            }
            result = &mut task => result,
        };
        match result {
            Ok(Ok(reply)) => Ok(reply.into()),
            Ok(Err(error)) => face_failure(error),
            Err(error) => Err(BridgeError::Value(error.to_string())),
        }
    })
}
