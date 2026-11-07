//! The task layer of Modality (Modality §2, §3.1, A.3–A.6): submission,
//! the per-category scheduler, `modality.task.*` and the `modalityTasks`
//! service.
//!
//! A task's record in `pluginFiles` is its only durable state; this module
//! keeps a mirror of it in memory for the methods, and every write goes
//! through the record's file lock comparing `revision`. Running work belongs
//! to the plugin's generation: the scheduler and the running tasks are
//! `ctx.effect` work that stops with the plugin.
use crate::ModalityAdapterService;
use crate::adapter::{AdapterOutput, AdapterProgress, Job, JobFile, ProgressSink, RemoteCancel};
use crate::task::{
    self, Executor, Expect, InputFile, ResultFile, TaskError, TaskRecord, TaskResult, TaskState,
    Write,
};
use crate::{Evaluation, catalog_category, is_understand};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use dim_agent_capabilities::file_storage::EntryKind;
use dim_agent_capabilities::host::{
    ModalityConnection, ModalityConnectionHost, ModalityTarget, ModelSelectionHost,
    SelectableModelsHost,
};
use dim_agent_capabilities::methods::{Progress, RpcError};
use dim_agent_types::ModelId;
use dim_app_server::plugin_files::PluginFileTransactions;
use dim_app_server::settings::SettingsService;
use rutis::Ctx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// At most this many tasks run per category; the rest stay queued in
/// submission order (A6).
pub const MAX_RUNNING_PER_CATEGORY: usize = 4;
/// The inline inputs of one submission, in raw bytes (decision 1): their
/// base64 fits one 16 MiB frame.
pub const MAX_INLINE_BYTES: u64 = 8 * 1024 * 1024;
/// Every file of one submission, in bytes (A5).
pub const MAX_SUBMISSION_BYTES: u64 = 100 * 1024 * 1024;
/// The largest `modality.task.list` page (A.3).
pub const MAX_LIST_LIMIT: u64 = 100;
/// The default `modality.task.list` page size (A.3).
pub const DEFAULT_LIST_LIMIT: u64 = 50;
/// The largest `modality.task.result` chunk (A.3).
pub const MAX_RESULT_BYTES: u64 = 4 * 1024 * 1024;
/// The largest `modality.task.wait` timeout (A.3).
pub const MAX_WAIT_MS: u64 = 600_000;
/// Progress is written to the record at most once per second (A10).
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// How long a drive keeps waiting for `submit` after a cancel, and how long a
/// cancel waits for the submit phase to end. It stays below rutis's unload
/// bound (`RUTIS_SHUTDOWN_TIMEOUT`, 10 s by default, rutis 0.19.3
/// `fiber.rs:1193`), leaving room for the cancel's own write and the rest of
/// an unload; past it a stuck adapter holds up neither.
pub const SUBMIT_CANCEL_GRACE: Duration = Duration::from_secs(3);
/// The name this build records for its single process adapter; the per-task
/// lookup (#861) will record the resolved adapter's name instead.
pub const ADAPTER_NAME: &str = "modalityAdapter";
/// Terminal tasks older than this many days are deleted (A.4, A2).
const RETENTION_DAYS: i64 = 30;
/// At most this many terminal tasks are kept; older ones are deleted (A.4,
/// A2).
const MAX_TERMINAL_TASKS: usize = 200;

/// The clock a generation reads: the wall clock, or a test's.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}
/// The wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

fn rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn invalid_input(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32602, "invalid_input", detail)
}
fn not_found(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "not_found", detail)
}
fn unavailable(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "unavailable", detail)
}
fn storage_error(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "storage_error", detail)
}
fn model_unavailable(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "model_unavailable", detail)
}
fn default_unavailable(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "default_unavailable", detail)
}
fn no_default_model(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "no_default_model", detail)
}

fn empty_object() -> Value {
    json!({})
}

/// The model a task uses, resolved like the capabilities snapshot (§3.4):
/// the explicit model, then the operation default, then automatic resolution.
/// Nothing ever falls back to another model.
pub(crate) fn resolve_task_model(
    evaluation: &Evaluation,
    category: &str,
    row: &crate::OperationRow,
    explicit: Option<&ModelId>,
) -> Result<ModelId, RpcError> {
    let snapshot = evaluation.operation(category, row);
    if let Some(model) = explicit {
        if !evaluation.known(model) {
            return Err(model_unavailable("Model is not selectable"));
        }
        let option = snapshot
            .options
            .iter()
            .find(|option| option.model() == (model.provider_id.as_str(), model.model_id.as_str()));
        return match option {
            Some(option) if option.available => Ok(model.clone()),
            Some(option) => Err(unavailable(
                option
                    .reason
                    .as_ref()
                    .map(|reason| reason.detail.clone())
                    .unwrap_or_else(|| "No available adapter serves the configured model".into()),
            )),
            None => Err(unavailable(
                evaluation
                    .unavailable_reason(model, category, row.operation, &row.id(category))
                    .detail,
            )),
        };
    }
    if let Some(default) = &snapshot.default {
        if default.available {
            return Ok(ModelId {
                provider_id: default.provider_id.clone(),
                model_id: default.model_id.clone(),
            });
        }
        return Err(default_unavailable(
            default
                .reason
                .as_ref()
                .map(|reason| reason.detail.clone())
                .unwrap_or_else(|| "The configured default is unavailable".into()),
        ));
    }
    match &snapshot.effective {
        Some(effective) => Ok(ModelId {
            provider_id: effective.provider_id.clone(),
            model_id: effective.model_id.clone(),
        }),
        None => Err(no_default_model(format!(
            "No model serves {}",
            row.id(category)
        ))),
    }
}

/// `modality.task.submit` (A.3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSubmit {
    pub category: String,
    pub operation: String,
    #[serde(default = "empty_object")]
    pub params: Value,
    #[serde(default)]
    pub files: Vec<TaskInput>,
    #[serde(default)]
    pub model: Option<ModelId>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub client_task_id: Option<String>,
}

/// One input of a submission (decision 1): inline bytes, or the caller's
/// absolute `node_path`, which Modality reads once and never copies.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    pub name: String,
    pub media_type: String,
    #[serde(default)]
    pub data_base64: Option<String>,
    #[serde(default)]
    pub node_path: Option<String>,
}

/// `modality.task.status` (A.3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStatus {
    pub task_id: String,
}

/// `modality.task.list` (A.3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskList {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub limit: Option<u64>,
    /// The last task id of the previous page.
    #[serde(default)]
    pub after: Option<String>,
}

/// `modality.task.wait` (A.3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWait {
    pub task_id: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `modality.task.cancel` (A.3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCancel {
    pub task_id: String,
}

/// `modality.task.result` (A.3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskResultQuery {
    pub task_id: String,
    pub name: String,
    #[serde(default)]
    pub offset: u64,
    pub length: u64,
}

/// `{ task }` (A.3).
#[derive(Debug, Serialize)]
pub struct TaskReply {
    pub task: TaskRecord,
}
/// `{ tasks, next? }`, newest first (A.3).
#[derive(Debug, Serialize)]
pub struct TaskListReply {
    pub tasks: Vec<TaskRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}
/// `{ data_base64, eof }` (A.3).
#[derive(Debug, Serialize)]
pub struct TaskResultReply {
    pub data_base64: String,
    pub eof: bool,
}

/// The plugin's shared state: everything the scheduler, the methods and the
/// running tasks read.
pub(crate) struct Shared {
    pub(crate) files: Arc<PluginFileTransactions>,
    pub(crate) settings: Arc<SettingsService>,
    pub(crate) selection: Arc<ModelSelectionHost>,
    pub(crate) selectable: Arc<SelectableModelsHost>,
    pub(crate) ctx: Ctx,
    /// The generation's stop signal; running work and the scheduler stop with
    /// it.
    pub(crate) stop: CancellationToken,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) tasks: TaskTracker,
    pub(crate) wake: Notify,
    pub(crate) state: Mutex<State>,
}

impl Shared {
    pub(crate) fn new(
        files: Arc<PluginFileTransactions>,
        settings: Arc<SettingsService>,
        selection: Arc<ModelSelectionHost>,
        selectable: Arc<SelectableModelsHost>,
        ctx: Ctx,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            files,
            settings,
            selection,
            selectable,
            ctx,
            stop: CancellationToken::new(),
            clock,
            tasks: TaskTracker::new(),
            wake: Notify::new(),
            state: Mutex::new(State::default()),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    fn now_text(&self) -> String {
        rfc3339(self.now())
    }

    /// The in-memory record of `slot`.
    fn record(&self, slot: &Slot) -> TaskRecord {
        slot.record.lock().unwrap().clone()
    }

    /// Publishes `record` to the mirror when it is newer than what the mirror
    /// holds; writes complete out of order relative to each other, and the
    /// highest revision is the one on disk.
    fn observe(&self, slot: &Slot, record: TaskRecord) {
        let latest = {
            let mut mirror = slot.record.lock().unwrap();
            if record.revision >= mirror.revision {
                *mirror = record;
            }
            mirror.clone()
        };
        slot.changed.send_replace(latest);
    }

    /// The slot of `task_id`, if this generation knows it.
    fn slot(&self, task_id: &str) -> Option<Arc<Slot>> {
        self.state.lock().unwrap().slots.get(task_id).cloned()
    }

    /// A write that does not change the terminal outcome (the running state,
    /// a handle, progress): built from the current record and retried while a
    /// concurrent write moves the revision, stopping once it is terminal.
    async fn put(
        &self,
        slot: &Arc<Slot>,
        expect: ExpectKind,
        change: impl Fn(&mut TaskRecord),
    ) -> Write {
        for _ in 0..8 {
            let current = self.record(slot);
            let accepted = match expect {
                ExpectKind::Queued => current.state == TaskState::Queued,
                ExpectKind::NonTerminal => !current.state.is_terminal(),
            };
            if !accepted {
                return Write::Discarded(Some(current));
            }
            let mut next = current.clone();
            change(&mut next);
            next.updated_at = self.now_text();
            let expected = current.revision;
            let expect = match expect {
                ExpectKind::Queued => Expect::Queued(expected),
                ExpectKind::NonTerminal => Expect::NonTerminal(expected),
            };
            match task::write(&self.files, next, expect).await {
                Ok(Write::Written(record)) => {
                    self.observe(slot, record.clone());
                    return Write::Written(record);
                }
                Ok(Write::Discarded(latest)) => {
                    if let Some(latest) = latest {
                        let terminal = latest.state.is_terminal();
                        self.observe(slot, latest);
                        if terminal {
                            return Write::Discarded(Some(self.record(slot)));
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, task = %slot.id, "task record write failed");
                    return Write::Discarded(Some(current));
                }
            }
        }
        Write::Discarded(Some(self.record(slot)))
    }

    /// The terminal write (§4): it applies only while the record is still at
    /// `base`, the revision the outcome was decided at; the first terminal
    /// write wins and a later outcome is discarded with the files it wrote.
    async fn finish(
        &self,
        slot: &Arc<Slot>,
        mut base: u64,
        change: impl Fn(&mut TaskRecord),
    ) -> Write {
        for _ in 0..8 {
            let current = self.record(slot);
            let mut next = current.clone();
            change(&mut next);
            next.updated_at = self.now_text();
            match task::write(&self.files, next, Expect::NonTerminal(base)).await {
                Ok(Write::Written(record)) => {
                    // Retention runs after every terminal write, before the
                    // terminal state becomes visible to `wait`/`status`
                    // (A.4).
                    if record.state.is_terminal() {
                        self.retain(Some(&record)).await;
                    }
                    self.observe(slot, record.clone());
                    return Write::Written(record);
                }
                Ok(Write::Discarded(latest)) => {
                    let Some(latest) = latest else {
                        return Write::Discarded(None);
                    };
                    let terminal = latest.state.is_terminal();
                    base = latest.revision;
                    self.observe(slot, latest);
                    if terminal {
                        return Write::Discarded(Some(self.record(slot)));
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, task = %slot.id, "task record write failed");
                    return Write::Discarded(Some(current));
                }
            }
        }
        Write::Discarded(Some(self.record(slot)))
    }

    /// Fails a task whose outcome is still open.
    async fn fail(&self, slot: &Arc<Slot>, code: impl Into<String>, message: impl Into<String>) {
        let current = self.record(slot);
        if current.state.is_terminal() {
            return;
        }
        let error = TaskError {
            code: code.into(),
            message: message.into(),
        };
        let finished = self.now_text();
        let _ = self
            .finish(slot, current.revision, |record| {
                record.state = TaskState::Failed;
                record.error = Some(error.clone());
                record.finished_at = Some(finished.clone());
            })
            .await;
    }

    /// Writes `cancelled`, if this generation's cancel gets there first.
    async fn settled_cancelled(
        &self,
        slot: &Arc<Slot>,
        base: u64,
        remote: Option<RemoteCancel>,
    ) -> Write {
        let finished = self.now_text();
        let remote = remote.map(|remote| remote.as_str().to_owned());
        self.finish(slot, base, move |record| {
            record.state = TaskState::Cancelled;
            record.remote_cancel = remote.clone();
            record.finished_at = Some(finished.clone());
        })
        .await
    }

    /// Resolves one connection for one adapter call (§3.4, #799): never
    /// stored, logged or returned.
    async fn connection(
        &self,
        record: &TaskRecord,
        cancel: &CancellationToken,
    ) -> Result<ModalityConnection, String> {
        let Some(host) = self.ctx.get::<ModalityConnectionHost>() else {
            return Err("Provider does not serve a non-conversational connection".into());
        };
        let provider = match &record.executor {
            Executor::Provider { provider, .. } => provider.clone(),
            Executor::Host { .. } => return Err("A host task has no provider connection".into()),
        };
        let target = ModalityTarget {
            target: provider,
            operation: format!("{}.{}", record.category, record.operation),
        };
        host.0
            .resolve_for_modality(target, cancel.child_token())
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy)]
enum ExpectKind {
    Queued,
    NonTerminal,
}

/// The tasks this generation knows: the record mirror, the live progress and
/// the signals a waiter reads.
struct Slot {
    id: String,
    /// The submission order within this generation (A6).
    sequence: u64,
    record: Mutex<TaskRecord>,
    /// Persisted changes, for `wait`.
    changed: watch::Sender<TaskRecord>,
    /// The adapter's latest progress, before it is persisted (A10).
    progress: watch::Sender<Option<AdapterProgress>>,
    progress_written: Mutex<Option<Instant>>,
    /// The local abort of this task's running work.
    cancel: CancellationToken,
    cancel_requested: AtomicBool,
    /// The submit phase: `false` from just after `running` until the handle
    /// is written (or `submit` ended without one), so a cancel can wait for
    /// the handle an accepted job produces (A.6).
    submit_ended: watch::Sender<bool>,
}

impl Slot {
    fn new(sequence: u64, record: TaskRecord) -> Arc<Self> {
        let changed = watch::channel(record.clone()).0;
        let progress = watch::channel(None).0;
        Arc::new(Self {
            id: record.task_id.clone(),
            sequence,
            record: Mutex::new(record),
            changed,
            progress,
            progress_written: Mutex::new(None),
            cancel: CancellationToken::new(),
            cancel_requested: AtomicBool::new(false),
            submit_ended: watch::channel(true).0,
        })
    }

    fn cancel_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }
    fn begin_submit(&self) {
        self.submit_ended.send_replace(false);
    }
    fn end_submit(&self) {
        self.submit_ended.send_replace(true);
    }
}

#[derive(Default)]
pub(crate) struct State {
    slots: BTreeMap<String, Arc<Slot>>,
    /// `client_task_id` to task id, rebuilt on `apply` (A.3).
    client_index: HashMap<String, String>,
    /// Tasks the scheduler has picked but not yet written `running` for.
    starting: BTreeSet<String>,
    /// One lock per client task id, so duplicate submissions serialize; the
    /// entry goes away when the last submit using it ends.
    client_locks: HashMap<String, ClientLock>,
    sequence: u64,
}

/// The submission lock of one `client_task_id` and the number of submits
/// holding or waiting on it.
#[derive(Default)]
struct ClientLock {
    lock: Arc<tokio::sync::Mutex<()>>,
    users: usize,
}

/// Holds a `client_task_id`'s submission lock; dropping the last guard
/// removes the entry.
struct ClientGuard {
    shared: Arc<Shared>,
    client_task_id: String,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl Drop for ClientGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut state = self.shared.state.lock().unwrap();
        if let Some(entry) = state.client_locks.get_mut(&self.client_task_id) {
            entry.users = entry.users.saturating_sub(1);
            if entry.users == 0 {
                state.client_locks.remove(&self.client_task_id);
            }
        }
    }
}

/// The `modalityTasks` process service and the `modality.task.*` methods:
/// one implementation for both entries (A.3, Appendix G).
pub struct ModalityTasks {
    shared: Arc<Shared>,
}

impl ModalityTasks {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// `modality.task.submit` (A.3): validates the boundary, resolves the
    /// model, writes `queued` and the inputs, and leaves the scheduler to
    /// start it.
    pub async fn submit(
        &self,
        params: TaskSubmit,
        cancel: &CancellationToken,
    ) -> Result<TaskReply, RpcError> {
        let shared = self.shared.clone();
        let category = catalog_category(&params.category)
            .ok_or_else(|| invalid_input(format!("Unknown category {}", params.category)))?;
        let row = category
            .rows
            .iter()
            .find(|row| row.operation == params.operation)
            .ok_or_else(|| {
                invalid_input(format!(
                    "Unknown {} operation {}",
                    category.name, params.operation
                ))
            })?;
        // Operations that are not tasks (decision 6).
        if is_understand(category.name, row.operation)
            || row.id(category.name) == "realtime.connect"
        {
            return Err(crate::unknown_operation(format!(
                "{} is not a task",
                row.id(category.name)
            )));
        }
        if category.name == "ocr" {
            return Err(RpcError::app(
                -32000,
                "modality_unavailable",
                "Not in this build",
            ));
        }
        if params.node.is_some() {
            return Err(invalid_input(format!(
                "node is only for host-executed categories, not {}",
                category.name
            )));
        }
        if !params.params.is_object() {
            return Err(invalid_input("params must be an object"));
        }
        // `params.files` is Modality's own channel in the record (A.4); a
        // caller supplies files at the top level only. This runs before the
        // adapter's schema, so the answer never depends on it.
        if params.params.get("files").is_some() {
            return Err(invalid_input(
                "params.files is reserved; pass input files in files",
            ));
        }
        // Duplicate `client_task_id`: the lock serializes concurrent submits,
        // and the index answers with the task that already exists.
        let _client_guard = self.client_guard(params.client_task_id.as_deref()).await;
        let existing = {
            let state = shared.state.lock().unwrap();
            params
                .client_task_id
                .as_ref()
                .and_then(|client_id| state.client_index.get(client_id).cloned())
        };
        if let Some(existing) = existing {
            let slot = shared
                .slot(&existing)
                .ok_or_else(|| not_found(format!("Task {existing} is no longer available")))?;
            return Ok(TaskReply {
                task: shared.record(&slot),
            });
        }
        let (_, evaluation) = crate::evaluate(
            &shared.settings,
            &shared.selectable,
            &shared.selection,
            &shared.ctx,
            cancel,
        )
        .await?;
        let model = resolve_task_model(&evaluation, category.name, row, params.model.as_ref())?;
        let spec = evaluation.operation_spec(&model, category.name, row.operation);
        let Some(spec) = spec else {
            return Err(unavailable(format!(
                "No available adapter serves {} for this model",
                row.id(category.name)
            )));
        };
        let validator = jsonschema::draft202012::new(&spec.params_schema).map_err(|error| {
            RpcError::app(
                -32000,
                "provider_error",
                format!("The adapter's parameter schema is invalid: {error}"),
            )
        })?;
        if let Err(error) = validator.validate(&params.params) {
            return Err(invalid_input(format!(
                "Invalid {} parameters: {error}",
                row.id(category.name)
            )));
        }
        // Boundary: names, media types and sizes (decision 1, decision 2).
        let allowed = allowed_media_types(category.name, row.operation);
        let mut inline_bytes: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut names = BTreeSet::new();
        let mut inputs: Vec<ResolvedInput> = Vec::new();
        for file in &params.files {
            if !valid_file_name(&file.name) {
                return Err(invalid_input(format!(
                    "{} is not a valid file name",
                    file.name
                )));
            }
            if !names.insert(file.name.clone()) {
                return Err(invalid_input(format!("Duplicate file name {}", file.name)));
            }
            if !allowed
                .iter()
                .any(|prefix| file.media_type.starts_with(prefix))
            {
                return Err(invalid_input(format!(
                    "{} is not an accepted media type for {}",
                    file.media_type,
                    row.id(category.name)
                )));
            }
            match (&file.data_base64, &file.node_path) {
                (Some(_), Some(_)) => {
                    return Err(invalid_input(format!(
                        "{} must be inline or a node_path, not both",
                        file.name
                    )));
                }
                (None, None) => {
                    return Err(invalid_input(format!(
                        "{} has neither data_base64 nor node_path",
                        file.name
                    )));
                }
                (Some(data), None) => {
                    let bytes = STANDARD
                        .decode(data)
                        .map_err(|_| invalid_input(format!("{} is not valid base64", file.name)))?;
                    inline_bytes += bytes.len() as u64;
                    total_bytes += bytes.len() as u64;
                    inputs.push(ResolvedInput {
                        name: file.name.clone(),
                        media_type: file.media_type.clone(),
                        data: Some(bytes),
                        node_path: None,
                    });
                }
                (None, Some(path)) => {
                    let remaining = MAX_SUBMISSION_BYTES.saturating_sub(total_bytes);
                    let (bytes, sha256, canonical) = node_path_meta(path, remaining).await?;
                    total_bytes += bytes;
                    inputs.push(ResolvedInput {
                        name: file.name.clone(),
                        media_type: file.media_type.clone(),
                        data: None,
                        node_path: Some((canonical, bytes, sha256)),
                    });
                }
            }
        }
        if inline_bytes > MAX_INLINE_BYTES {
            return Err(invalid_input(format!(
                "Inline inputs are at most {MAX_INLINE_BYTES} bytes per submission"
            )));
        }
        if total_bytes > MAX_SUBMISSION_BYTES {
            return Err(invalid_input(format!(
                "Files are at most {MAX_SUBMISSION_BYTES} bytes per submission"
            )));
        }
        let task_id = format!("mt_{}", uuid::Uuid::new_v4());
        let created = shared.now_text();
        let mut record_params = params.params.clone();
        let metadata: Vec<InputFile> = inputs
            .iter()
            .map(|input| InputFile {
                name: input.name.clone(),
                media_type: input.media_type.clone(),
                bytes: input.bytes(),
                sha256: input.sha256(),
                node_path: input.node_path_path().map(|path| path.to_owned()),
            })
            .collect();
        // The record's `params.files` is always the validated metadata, an
        // empty array when the submission has no files (A.4).
        record_params["files"] =
            serde_json::to_value(&metadata).expect("input metadata is serializable");
        let record = TaskRecord {
            version: task::FORMAT_VERSION,
            task_id: task_id.clone(),
            client_task_id: params.client_task_id.clone(),
            revision: 0,
            category: category.name.to_owned(),
            operation: row.operation.to_owned(),
            executor: Executor::Provider {
                provider: model,
                adapter: ADAPTER_NAME.to_owned(),
            },
            params: record_params,
            state: TaskState::Queued,
            remote_handle: None,
            progress: None,
            result: None,
            error: None,
            remote_cancel: None,
            created_at: created.clone(),
            updated_at: created,
            finished_at: None,
        };
        // The inputs and the record are written before the method returns.
        // The task directory exists first: a submission without inline files
        // has no input directory to create it as a side effect.
        if let Err(error) = shared.files.directory(&task::task_dir(&task_id)).await {
            return Err(storage_error(error.to_string()));
        }
        if !inputs.is_empty() {
            let _ = shared
                .files
                .directory(&task::input_dir_name(&task_id))
                .await;
        }
        for input in &inputs {
            if let Some(data) = &input.data
                && let Err(error) =
                    task::write_input(&shared.files, &task_id, &input.name, data.clone()).await
            {
                let _ = task::remove_task(&shared.files, &task_id).await;
                return Err(storage_error(error.to_string()));
            }
        }
        let record = match task::create(&shared.files, record).await {
            Ok(record) => record,
            Err(error) => {
                let _ = task::remove_task(&shared.files, &task_id).await;
                return Err(storage_error(error.to_string()));
            }
        };
        {
            let mut state = shared.state.lock().unwrap();
            state.sequence += 1;
            let slot = Slot::new(state.sequence, record.clone());
            if let Some(client_id) = &record.client_task_id {
                state
                    .client_index
                    .insert(client_id.clone(), task_id.clone());
            }
            state.slots.insert(task_id.clone(), slot);
        }
        shared.wake.notify_one();
        Ok(TaskReply { task: record })
    }

    /// `modality.task.status` (A.3).
    pub async fn status(&self, params: TaskStatus) -> Result<TaskReply, RpcError> {
        let slot = self
            .shared
            .slot(&params.task_id)
            .ok_or_else(|| not_found(format!("Unknown task {}", params.task_id)))?;
        Ok(TaskReply {
            task: self.shared.record(&slot),
        })
    }

    /// `modality.task.list` (A.3): newest first, one page at a time.
    pub async fn list(&self, params: TaskList) -> Result<TaskListReply, RpcError> {
        let limit = match params.limit {
            None => DEFAULT_LIST_LIMIT,
            Some(limit) if limit == 0 || limit > MAX_LIST_LIMIT => {
                return Err(invalid_input(format!(
                    "limit must be between 1 and {MAX_LIST_LIMIT}"
                )));
            }
            Some(limit) => limit,
        };
        let state = match &params.state {
            Some(state) => Some(
                serde_json::from_value::<TaskState>(Value::String(state.clone()))
                    .map_err(|_| invalid_input(format!("Unknown state {state}")))?,
            ),
            None => None,
        };
        let mut tasks: Vec<TaskRecord> = {
            let state = self.shared.state.lock().unwrap();
            state
                .slots
                .values()
                .map(|slot| self.shared.record(slot))
                .collect()
        };
        tasks.retain(|task| {
            state.is_none_or(|state| task.state == state)
                && params
                    .category
                    .as_ref()
                    .is_none_or(|category| &task.category == category)
        });
        tasks.sort_by(|left, right| {
            (right.created_at.as_str(), right.task_id.as_str())
                .cmp(&(left.created_at.as_str(), left.task_id.as_str()))
        });
        if let Some(after) = &params.after {
            match tasks.iter().position(|task| &task.task_id == after) {
                Some(index) => {
                    tasks = tasks.into_iter().skip(index + 1).collect();
                }
                None => {
                    return Err(invalid_input(format!("Unknown after task {after}")));
                }
            }
        }
        let next = if tasks.len() as u64 > limit {
            tasks.truncate(limit as usize);
            tasks.last().map(|task| task.task_id.clone())
        } else {
            None
        };
        Ok(TaskListReply { tasks, next })
    }

    /// `modality.task.wait` (A.3): returns the record when it reaches a
    /// terminal state or the timeout; progress goes to the caller's sink.
    pub async fn wait(
        &self,
        params: TaskWait,
        cancel: &CancellationToken,
        progress: Option<&Progress>,
    ) -> Result<TaskReply, RpcError> {
        let timeout = match params.timeout_ms {
            None => MAX_WAIT_MS,
            Some(timeout) if timeout > MAX_WAIT_MS => {
                return Err(invalid_input(format!(
                    "timeout_ms is at most {MAX_WAIT_MS}"
                )));
            }
            Some(timeout) => timeout,
        };
        let slot = self
            .shared
            .slot(&params.task_id)
            .ok_or_else(|| not_found(format!("Unknown task {}", params.task_id)))?;
        let deadline = Instant::now() + Duration::from_millis(timeout);
        let mut changed = slot.changed.subscribe();
        let mut live = slot.progress.subscribe();
        let mut sent: Option<AdapterProgress> = None;
        loop {
            let record = changed.borrow_and_update().clone();
            if record.state.is_terminal() {
                return Ok(TaskReply { task: record });
            }
            let latest = live.borrow_and_update().clone();
            if let Some(latest) = latest
                && sent.as_ref() != Some(&latest)
            {
                if let Some(sink) = progress {
                    sink.send(serde_json::to_value(&latest).unwrap_or(Value::Null));
                }
                sent = Some(latest);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(TaskReply {
                    task: self.shared.record(&slot),
                });
            }
            tokio::select! {
                _ = changed.changed() => {}
                _ = live.changed() => {}
                _ = tokio::time::sleep(remaining) => {}
                _ = cancel.cancelled() => return Err(RpcError::interrupted()),
            }
        }
    }

    /// `modality.task.cancel` (A.3): idempotent; the first terminal write
    /// wins.
    pub async fn cancel(
        &self,
        params: TaskCancel,
        cancel: &CancellationToken,
    ) -> Result<TaskReply, RpcError> {
        let slot = self
            .shared
            .slot(&params.task_id)
            .ok_or_else(|| not_found(format!("Unknown task {}", params.task_id)))?;
        let record = self.shared.record(&slot);
        if record.state.is_terminal() {
            return Ok(TaskReply { task: record });
        }
        slot.cancel_requested.store(true, Ordering::SeqCst);
        slot.cancel.cancel();
        // A cancel waits for the submit phase to end (A.6): a job the
        // executor accepted yields its handle first, so the terminal write
        // records the executor's answer. The wait is bounded by the same
        // grace the drive uses, and by this call's token and the
        // generation's stop.
        let mut ended = slot.submit_ended.subscribe();
        if !*ended.borrow_and_update() {
            tokio::select! {
                _ = ended.changed() => {}
                _ = tokio::time::sleep(SUBMIT_CANCEL_GRACE) => {}
                _ = cancel.cancelled() => {}
                _ = self.shared.stop.cancelled() => {}
            }
        }
        let record = self.shared.record(&slot);
        if record.state.is_terminal() {
            return Ok(TaskReply { task: record });
        }
        let mut remote = None;
        if let Some(handle) = &record.remote_handle
            && let Ok(connection) = self.shared.connection(&record, cancel).await
            && let Some(adapter) = self.shared.ctx.get::<ModalityAdapterService>()
        {
            // A hung provider must not keep the client or an unload waiting.
            remote = tokio::select! {
                answer = adapter.0.cancel_remote(handle, connection) => answer.ok(),
                _ = cancel.cancelled() => None,
                _ = self.shared.stop.cancelled() => None,
            };
        }
        self.shared
            .settled_cancelled(&slot, record.revision, remote)
            .await;
        Ok(TaskReply {
            task: self.shared.record(&slot),
        })
    }

    /// `modality.task.result` (A.3): a chunk of one result file, read
    /// through the streaming read.
    pub async fn result(
        &self,
        params: TaskResultQuery,
        cancel: &CancellationToken,
    ) -> Result<TaskResultReply, RpcError> {
        if params.length > MAX_RESULT_BYTES {
            return Err(invalid_input(format!(
                "length is at most {MAX_RESULT_BYTES} bytes"
            )));
        }
        let slot = self
            .shared
            .slot(&params.task_id)
            .ok_or_else(|| not_found(format!("Unknown task {}", params.task_id)))?;
        let record = self.shared.record(&slot);
        let Some(result) = &record.result else {
            return Err(not_found(format!("Task {} has no result", params.task_id)));
        };
        let Some(file) = result.files.iter().find(|file| file.name == params.name) else {
            return Err(not_found(format!("Unknown result file {}", params.name)));
        };
        let Some(reader) = self
            .shared
            .files
            .open_read(&task::result_name(&params.task_id, &params.name))
            .await
            .map_err(|error| storage_error(error.to_string()))?
        else {
            return Err(not_found(format!("Unknown result file {}", params.name)));
        };
        let length = reader.len().min(file.bytes);
        let mut cursor = params.offset;
        let mut data = Vec::new();
        while cursor < length && (data.len() as u64) < params.length {
            let want = (params.length - data.len() as u64).min(length - cursor);
            let chunk = reader
                .read_at(cursor, want as usize, cancel)
                .await
                .map_err(|error| storage_error(error.to_string()))?;
            if chunk.is_empty() {
                break;
            }
            cursor += chunk.len() as u64;
            data.extend(chunk);
        }
        Ok(TaskResultReply {
            data_base64: STANDARD.encode(&data),
            eof: cursor >= length,
        })
    }

    /// The per-client submission lock.
    async fn client_guard(&self, client_task_id: Option<&str>) -> Option<ClientGuard> {
        let client_task_id = client_task_id?;
        let lock = {
            let mut state = self.shared.state.lock().unwrap();
            let entry = state
                .client_locks
                .entry(client_task_id.to_owned())
                .or_default();
            entry.users += 1;
            entry.lock.clone()
        };
        // The guard exists before the lock is awaited, so a cancelled call
        // still gives its reference back.
        let mut client_guard = ClientGuard {
            shared: self.shared.clone(),
            client_task_id: client_task_id.to_owned(),
            guard: None,
        };
        client_guard.guard = Some(lock.lock_owned().await);
        Some(client_guard)
    }
}

/// One validated input of a submission.
struct ResolvedInput {
    name: String,
    media_type: String,
    data: Option<Vec<u8>>,
    node_path: Option<(String, u64, String)>,
}
impl ResolvedInput {
    fn bytes(&self) -> u64 {
        match (&self.data, &self.node_path) {
            (Some(data), None) => data.len() as u64,
            (None, Some((_, bytes, _))) => *bytes,
            _ => 0,
        }
    }
    fn sha256(&self) -> String {
        match (&self.data, &self.node_path) {
            (Some(data), None) => task::sha256(data),
            (None, Some((_, _, sha256))) => sha256.clone(),
            _ => String::new(),
        }
    }
    fn node_path_path(&self) -> Option<&str> {
        self.node_path.as_ref().map(|(path, _, _)| path.as_str())
    }
}

/// The last component of a listed task directory name.
fn entry_task_id(name: &str) -> Option<String> {
    name.rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .map(str::to_owned)
}

/// When a terminal record was finished, for retention (A.4).
fn record_time(record: &TaskRecord) -> Option<DateTime<Utc>> {
    let text = record.finished_at.as_deref().unwrap_or(&record.updated_at);
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// The terminal tasks retention deletes (A.4, A2): everything older than 30
/// days and everything beyond the newest 200, oldest first.
fn retention_expired(
    mut terminal: Vec<(String, DateTime<Utc>)>,
    now: DateTime<Utc>,
) -> Vec<String> {
    terminal.sort_by_key(|(_, time)| *time);
    let over = terminal.len().saturating_sub(MAX_TERMINAL_TASKS);
    terminal
        .into_iter()
        .enumerate()
        .filter(|(index, (_, time))| {
            *index < over || now.signed_duration_since(*time) > ChronoDuration::days(RETENTION_DAYS)
        })
        .map(|(_, (task_id, _))| task_id)
        .collect()
}

/// The media type prefixes an operation accepts (decision 2); an empty list
/// refuses files.
fn allowed_media_types(category: &str, operation: &str) -> &'static [&'static str] {
    match (category, operation) {
        ("image", _) => &["image/"],
        ("video", _) => &["image/", "video/"],
        ("speech", "transcribe") => &["audio/"],
        ("asset", _) => &["image/"],
        _ => &[],
    }
}

/// Whether `name` is one valid `pluginFiles` component (decision 2).
fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// The `bytes`, `sha256` and canonical path of a caller's `node_path`
/// (decision 1). The path is absolute, points at an existing regular file and
/// its last component is not a link; its length is compared with what is
/// left of the submission budget before anything is read, and the file is
/// opened without following a link so a last component swapped after the
/// check is refused. Modality never copies it.
async fn node_path_meta(path: &str, remaining: u64) -> Result<(u64, String, String), RpcError> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(invalid_input("node_path must be absolute"));
    }
    let probe = path.clone();
    let (canonical, length) = tokio::task::spawn_blocking(move || {
        let metadata = std::fs::symlink_metadata(&probe)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the last component is a link",
            ));
        }
        if !metadata.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok::<_, std::io::Error>((std::fs::canonicalize(&probe)?, metadata.len()))
    })
    .await
    .map_err(|error| invalid_input(format!("node_path could not be read: {error}")))?
    .map_err(|error| invalid_input(format!("node_path could not be read: {error}")))?;
    if length > remaining {
        return Err(invalid_input(format!(
            "node_path is {length} bytes, over the {remaining} bytes left of the submission budget"
        )));
    }
    let canonical_text = canonical
        .to_str()
        .ok_or_else(|| invalid_input("node_path is not valid UTF-8"))?
        .to_owned();
    let read = canonical.clone();
    let (bytes, sha256) = tokio::task::spawn_blocking(move || {
        let mut file = open_without_following(&read)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut bytes = 0u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            if bytes > length {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the file grew while it was read",
                ));
            }
            hasher.update(&buffer[..count]);
        }
        Ok::<_, std::io::Error>((bytes, task::hex(&hasher.finalize())))
    })
    .await
    .map_err(|error| invalid_input(format!("node_path could not be read: {error}")))?
    .map_err(|error| invalid_input(format!("node_path could not be read: {error}")))?;
    Ok((bytes, sha256, canonical_text))
}

/// Opens `path` for reading without following a link in its last component;
/// the canonical path it is given has no link there, and this keeps a swap
/// between the check and the open from being followed.
fn open_without_following(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::File::open(path)
    }
}

impl Shared {
    /// Runs the scheduler for this generation: `ctx.effect` work that starts
    /// queued tasks as category slots free (A.6).
    pub(crate) fn spawn_scheduler(self: &Arc<Self>) {
        let shared = self.clone();
        self.tasks.spawn(async move {
            loop {
                tokio::select! {
                    _ = shared.stop.cancelled() => break,
                    _ = shared.wake.notified() => {}
                }
                shared.start_ready().await;
            }
        });
    }

    /// Startup on every `apply` (A.6): rebuild the slots and the client
    /// index from the records, apply retention, settle what this generation
    /// cannot continue, and observe the running tasks a stored handle can
    /// still resume. A `queued` task never starts after a restart (A11).
    pub(crate) async fn recover(self: &Arc<Self>) {
        let _ = self.files.directory(task::TASKS_DIR).await;
        let entries = match self.files.list(task::TASKS_DIR).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, "task records could not be listed");
                return;
            }
        };
        let mut records = Vec::new();
        for entry in entries {
            if entry.kind != EntryKind::Directory {
                continue;
            }
            let Some(task_id) = entry_task_id(&entry.name) else {
                continue;
            };
            match task::load(&self.files, &task_id).await {
                Ok(Some(record)) if record.task_id == task_id => records.push(record),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, task = %task_id, "task record could not be read")
                }
            }
        }
        // Slots are ordered by creation, so `list` stays newest first.
        records.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        // Settle before retention, so a task this `apply` interrupts counts
        // toward the retained total (A.4).
        let mut settled = Vec::with_capacity(records.len());
        for record in records {
            settled.push(self.startup_settle(record).await);
        }
        let expired: BTreeSet<String> = retention_expired(
            settled
                .iter()
                .filter(|record| record.state.is_terminal())
                .filter_map(|record| Some((record.task_id.clone(), record_time(record)?)))
                .collect(),
            self.now(),
        )
        .into_iter()
        .collect();
        for task_id in &expired {
            let _ = task::remove_task(&self.files, task_id).await;
        }
        settled.retain(|record| !expired.contains(&record.task_id));
        let mut slots = Vec::new();
        for (index, record) in settled.into_iter().enumerate() {
            slots.push(Slot::new(index as u64 + 1, record));
        }
        let mut observe = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            for slot in &slots {
                let record = slot.record.lock().unwrap().clone();
                if let Some(client_id) = &record.client_task_id {
                    state
                        .client_index
                        .insert(client_id.clone(), record.task_id.clone());
                }
                if record.state == TaskState::Running {
                    observe.push(slot.clone());
                }
                state.slots.insert(record.task_id.clone(), slot.clone());
            }
            state.sequence = slots.len() as u64;
        }
        for slot in observe {
            let shared = self.clone();
            self.tasks.spawn(async move {
                shared.observe_resumed(slot).await;
            });
        }
    }

    /// The startup rule for one record (A.6): `queued` and host `running`
    /// tasks are interrupted; a provider task with a handle its adapter
    /// still serves stays `running` for observation; anything else is
    /// interrupted as not having survived recovery.
    async fn startup_settle(&self, record: TaskRecord) -> TaskRecord {
        if record.state.is_terminal() {
            return record;
        }
        if record.state == TaskState::Running {
            if let Executor::Provider { provider, .. } = &record.executor
                && record.remote_handle.is_some()
                && self.adapter_serves(provider, &record.category, &record.operation)
            {
                return record;
            }
            return self
                .write_interrupted(record, Some("did not survive recovery"))
                .await;
        }
        self.write_interrupted(record, None).await
    }

    /// Whether the process adapter is available and serves the operation.
    fn adapter_serves(&self, provider: &ModelId, category: &str, operation: &str) -> bool {
        let Some(adapter) = self.ctx.get::<ModalityAdapterService>() else {
            return false;
        };
        adapter.0.available()
            && adapter
                .0
                .operations(provider)
                .iter()
                .any(|served| served.category == category && served.operation == operation)
    }

    /// Writes `interrupted` over a record loaded at startup.
    async fn write_interrupted(&self, record: TaskRecord, message: Option<&str>) -> TaskRecord {
        let finished = self.now_text();
        let mut next = record.clone();
        next.state = TaskState::Interrupted;
        next.finished_at = Some(finished.clone());
        next.updated_at = finished;
        if let Some(message) = message {
            next.error = Some(TaskError {
                code: "interrupted".into(),
                message: message.into(),
            });
        }
        let expected = record.revision;
        match task::write(&self.files, next, Expect::Revision(expected)).await {
            Ok(Write::Written(record)) => record,
            Ok(Write::Discarded(Some(current))) => current,
            _ => {
                let mut fallback = record;
                fallback.state = TaskState::Interrupted;
                fallback
            }
        }
    }

    /// Continues watching a task this generation did not start (A12): waits
    /// on the recorded handle, never submits again.
    async fn observe_resumed(self: &Arc<Self>, slot: Arc<Slot>) {
        let record = self.record(&slot);
        if record.state != TaskState::Running || slot.cancel_requested() {
            return;
        }
        let handle = record.remote_handle.clone();
        self.wait_and_settle(&slot, record, handle).await;
    }

    /// The unload rule (A.6): stop the scheduler and the local work, wait for
    /// it, then write `interrupted` for every task without a handle. A task
    /// with a handle stays `running` for the next generation's startup.
    pub(crate) async fn shutdown(self: &Arc<Self>) {
        self.stop.cancel();
        let slots: Vec<Arc<Slot>> = self.state.lock().unwrap().slots.values().cloned().collect();
        for slot in &slots {
            slot.cancel_requested.store(true, Ordering::SeqCst);
            slot.cancel.cancel();
        }
        self.tasks.close();
        self.tasks.wait().await;
        for slot in slots {
            let record = self.record(&slot);
            if record.state.is_terminal() || record.remote_handle.is_some() {
                continue;
            }
            let finished = self.now_text();
            let _ = self
                .finish(&slot, record.revision, move |record| {
                    record.state = TaskState::Interrupted;
                    record.finished_at = Some(finished.clone());
                })
                .await;
        }
    }

    /// Deletes terminal tasks older than 30 days or beyond the newest 200,
    /// oldest first, with their files (A.4, A2). `extra` is the terminal
    /// record this write just produced; it participates in the count before
    /// it becomes visible through its slot.
    async fn retain(&self, extra: Option<&TaskRecord>) {
        let mut terminal: Vec<(String, DateTime<Utc>)> = {
            let state = self.state.lock().unwrap();
            state
                .slots
                .values()
                .filter_map(|slot| {
                    let record = slot.record.lock().unwrap();
                    if !record.state.is_terminal() {
                        return None;
                    }
                    Some((record.task_id.clone(), record_time(&record)?))
                })
                .collect()
        };
        if let Some(record) = extra
            && record.state.is_terminal()
            && let Some(time) = record_time(record)
        {
            terminal.push((record.task_id.clone(), time));
        }
        let keep = extra.map(|record| record.task_id.as_str());
        for task_id in retention_expired(terminal, self.now()) {
            if keep == Some(task_id.as_str()) {
                continue;
            }
            let _ = task::remove_task(&self.files, &task_id).await;
            let mut state = self.state.lock().unwrap();
            if let Some(slot) = state.slots.remove(&task_id)
                && let Some(client_id) = &slot.record.lock().unwrap().client_task_id
            {
                state.client_index.remove(client_id);
            }
        }
    }

    /// Picks queued tasks in submission order while a category has a free
    /// slot, and starts each one.
    async fn start_ready(self: &Arc<Self>) {
        let picks = {
            let mut state = self.state.lock().unwrap();
            let mut running: BTreeMap<String, usize> = BTreeMap::new();
            for slot in state.slots.values() {
                let record = slot.record.lock().unwrap();
                if record.state == TaskState::Running || state.starting.contains(&slot.id) {
                    *running.entry(record.category.clone()).or_default() += 1;
                }
            }
            let mut queued: Vec<Arc<Slot>> = state
                .slots
                .values()
                .filter(|slot| slot.record.lock().unwrap().state == TaskState::Queued)
                .cloned()
                .collect();
            queued.sort_by_key(|slot| slot.sequence);
            let mut picks = Vec::new();
            for slot in queued {
                let category = slot.record.lock().unwrap().category.clone();
                let count = running.entry(category).or_default();
                if *count >= MAX_RUNNING_PER_CATEGORY {
                    continue;
                }
                *count += 1;
                state.starting.insert(slot.id.clone());
                picks.push(slot);
            }
            picks
        };
        for slot in picks {
            let shared = self.clone();
            self.tasks.spawn(async move {
                shared.drive(slot.clone()).await;
                shared.state.lock().unwrap().starting.remove(&slot.id);
                shared.wake.notify_one();
            });
        }
    }

    /// Runs one task: `running`, `submit`, then `wait`, and one terminal
    /// write.
    async fn drive(self: &Arc<Self>, slot: Arc<Slot>) {
        let record = self.record(&slot);
        if record.state != TaskState::Queued {
            return;
        }
        let record = match self
            .put(&slot, ExpectKind::Queued, |record| {
                record.state = TaskState::Running;
            })
            .await
        {
            Write::Written(record) => record,
            Write::Discarded(_) => return,
        };
        // The submit phase opens before the cancel check: a cancel that
        // arrives now waits for it to end and then sees the handle a job the
        // executor accepted produced (A.6).
        slot.begin_submit();
        if slot.cancel_requested() {
            slot.end_submit();
            return;
        }
        let submitted = self.submit_phase(&slot, &record).await;
        slot.end_submit();
        let Some(handle) = submitted else {
            return;
        };
        // A cancel request stops the drive; `modality.task.cancel` owns the
        // terminal `cancelled` write, because only it can ask the executor
        // for the remote answer the record keeps.
        if slot.cancel_requested() {
            return;
        }
        self.wait_and_settle(&slot, record, handle).await;
    }

    /// One submit phase (A.6): the adapter checks, the connection, `submit`
    /// and the handle write. It returns the handle to wait on, or `None`
    /// when the task must not continue and a cancel owns its terminal write.
    /// After a cancel the phase keeps waiting for `submit` up to
    /// [`SUBMIT_CANCEL_GRACE`], so a job the executor accepted still yields
    /// its handle.
    async fn submit_phase(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        record: &TaskRecord,
    ) -> Option<Option<String>> {
        let Some(adapter) = self.ctx.get::<ModalityAdapterService>() else {
            if !slot.cancel_requested() {
                self.fail(slot, "unavailable", "No adapter in this build")
                    .await;
            }
            return None;
        };
        if !adapter.0.available() {
            if !slot.cancel_requested() {
                self.fail(slot, "unavailable", "The adapter is unavailable")
                    .await;
            }
            return None;
        }
        let Executor::Provider { provider, .. } = &record.executor else {
            if !slot.cancel_requested() {
                self.fail(slot, "unavailable", "A host task has no executor")
                    .await;
            }
            return None;
        };
        if !adapter.0.operations(provider).iter().any(|served| {
            served.category == record.category && served.operation == record.operation
        }) {
            if !slot.cancel_requested() {
                self.fail(
                    slot,
                    "unavailable",
                    "No available adapter serves the configured model",
                )
                .await;
            }
            return None;
        }
        let files = match self.job_files(record).await {
            Ok(files) => files,
            Err(error) => {
                if !slot.cancel_requested() {
                    self.fail(slot, "storage_error", error).await;
                }
                return None;
            }
        };
        let work_dir = match self
            .files
            .directory(&task::work_name(&record.task_id))
            .await
        {
            Ok(path) => path,
            Err(error) => {
                if !slot.cancel_requested() {
                    self.fail(slot, "storage_error", error.to_string()).await;
                }
                return None;
            }
        };
        let job = Job {
            task_id: record.task_id.clone(),
            category: record.category.clone(),
            operation: record.operation.clone(),
            model: provider.clone(),
            params: record.params.clone(),
            files,
            work_dir,
        };
        // A missing or refused connection before the task starts fails it
        // `unavailable` with the provider's reason (decision 5).
        let connection = match self.connection(record, &slot.cancel).await {
            Ok(connection) => connection,
            Err(error) => {
                if !slot.cancel_requested() {
                    self.fail(slot, "unavailable", error).await;
                }
                return None;
            }
        };
        // From here the adapter may already hold the job; a cancel must not
        // drop `submit`, because the handle it returns is the only way to
        // reach that job.
        let mut submit = Box::pin(adapter.0.submit(job, connection, slot.cancel.child_token()));
        let mut submitted = None;
        tokio::select! {
            result = &mut submit => submitted = Some(result),
            _ = slot.cancel.cancelled() => {
                tokio::select! {
                    result = &mut submit => submitted = Some(result),
                    _ = tokio::time::sleep(SUBMIT_CANCEL_GRACE) => {}
                }
            }
        }
        let handle = match submitted {
            Some(Ok(submitted)) => submitted.remote_handle,
            Some(Err(error)) => {
                if !slot.cancel_requested() {
                    self.fail(slot, error.code.as_str(), error.message).await;
                }
                return None;
            }
            None => return None,
        };
        if let Some(handle) = &handle {
            // The handle is written before any running progress (A.4), even
            // after a cancel: the cancel asks the executor about it.
            if let Write::Discarded(_) = self
                .put(slot, ExpectKind::NonTerminal, |record| {
                    record.remote_handle = Some(handle.clone());
                })
                .await
            {
                return None;
            }
        }
        Some(handle)
    }

    /// Waits for an already submitted task and settles its one outcome. Used
    /// by a normal run and, after a restart, by observation: a task with a
    /// stored handle is only ever waited for, never submitted again (A6).
    async fn wait_and_settle(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        record: TaskRecord,
        handle: Option<String>,
    ) {
        let Some(adapter) = self.ctx.get::<ModalityAdapterService>() else {
            if !slot.cancel_requested() {
                self.fail(slot, "unavailable", "No adapter in this build")
                    .await;
            }
            return;
        };
        if !adapter.0.available() {
            if !slot.cancel_requested() {
                self.fail(slot, "unavailable", "The adapter is unavailable")
                    .await;
            }
            return;
        }
        let connection = match self.connection(&record, &slot.cancel).await {
            Ok(connection) => connection,
            Err(error) => {
                if !slot.cancel_requested() {
                    self.fail(slot, "provider_error", error).await;
                }
                return;
            }
        };
        let progress = {
            let shared = self.clone();
            let slot = slot.clone();
            ProgressSink::new(move |progress| shared.report_progress(&slot, progress))
        };
        let output = tokio::select! {
            output = adapter.0.wait(&record.task_id, handle.as_deref(), connection, progress, slot.cancel.child_token()) => output,
            _ = slot.cancel.cancelled() => return,
        };
        if slot.cancel_requested() {
            return;
        }
        match output {
            Ok(output) => self.settle_output(slot, output).await,
            Err(error) => {
                if slot.cancel_requested() {
                    return;
                }
                self.fail(slot, error.code.as_str(), error.message).await;
            }
        }
    }

    /// Streams the adapter's output files into `result/` and writes the
    /// terminal `succeeded` record (decision 7). A file outside the work
    /// directory fails the task with `provider_error` and writes no result.
    async fn settle_output(self: &Arc<Self>, slot: &Arc<Slot>, output: AdapterOutput) {
        let record = self.record(slot);
        if record.state.is_terminal() {
            return;
        }
        // The outcome is decided now; the terminal write applies only while
        // the record is still at this revision (§4).
        let base = record.revision;
        let work = match self
            .files
            .directory(&task::work_name(&record.task_id))
            .await
        {
            Ok(path) => path,
            Err(error) => {
                self.fail(slot, "provider_error", error.to_string()).await;
                return;
            }
        };
        let canonical_work = match tokio::task::spawn_blocking({
            let work = work.clone();
            move || std::fs::canonicalize(work)
        })
        .await
        {
            Ok(Ok(path)) => path,
            _ => {
                self.fail(slot, "provider_error", "The work directory is unavailable")
                    .await;
                return;
            }
        };
        if let Err(error) = self
            .files
            .directory(&task::result_dir_name(&record.task_id))
            .await
        {
            let _ = error;
            self.fail(slot, "storage_error", "The result directory is unavailable")
                .await;
            return;
        }
        let mut names = BTreeSet::new();
        let mut written: Vec<String> = Vec::new();
        let mut files: Vec<ResultFile> = Vec::new();
        for file in &output.files {
            if !valid_file_name(&file.name) || !names.insert(file.name.clone()) {
                self.discard_results(&record.task_id, &written).await;
                self.fail(
                    slot,
                    "provider_error",
                    "The adapter returned an invalid file name",
                )
                .await;
                return;
            }
            let path = file.path.clone();
            let probe = {
                let canonical_work = canonical_work.clone();
                tokio::task::spawn_blocking(move || {
                    let metadata = std::fs::symlink_metadata(&path)?;
                    if !metadata.file_type().is_file() {
                        return Ok::<_, std::io::Error>(None);
                    }
                    let canonical = std::fs::canonicalize(&path)?;
                    if !canonical.starts_with(&canonical_work) || canonical == canonical_work {
                        return Ok(None);
                    }
                    Ok(Some(canonical))
                })
                .await
            };
            let canonical = match probe {
                Ok(Ok(Some(path))) => path,
                _ => {
                    self.discard_results(&record.task_id, &written).await;
                    self.fail(
                        slot,
                        "provider_error",
                        "The adapter returned a file outside its work directory",
                    )
                    .await;
                    return;
                }
            };
            let source = match tokio::fs::File::open(&canonical).await {
                Ok(source) => source.into_std().await,
                Err(error) => {
                    self.discard_results(&record.task_id, &written).await;
                    self.fail(slot, "provider_error", error.to_string()).await;
                    return;
                }
            };
            let (bytes, sha256) =
                match task::write_result(&self.files, &record.task_id, &file.name, source).await {
                    Ok(result) => result,
                    Err(error) => {
                        self.discard_results(&record.task_id, &written).await;
                        self.fail(slot, "storage_error", error.to_string()).await;
                        return;
                    }
                };
            written.push(file.name.clone());
            files.push(ResultFile {
                name: file.name.clone(),
                media_type: file.media_type.clone(),
                bytes,
                sha256,
            });
        }
        let _ = task::remove_work(&self.files, &record.task_id).await;
        let result = TaskResult {
            files,
            data: output.data,
        };
        let finished = self.now_text();
        match self
            .finish(slot, base, move |record| {
                record.state = TaskState::Succeeded;
                record.result = Some(result.clone());
                record.finished_at = Some(finished.clone());
            })
            .await
        {
            Write::Discarded(_) => {
                // Another terminal write won; remove what this outcome wrote
                // unless the winner refers to the same names.
                let winner = self.record(slot);
                let keep: BTreeSet<String> = winner
                    .result
                    .as_ref()
                    .map(|result| result.files.iter().map(|file| file.name.clone()).collect())
                    .unwrap_or_default();
                let orphaned: Vec<String> = written
                    .into_iter()
                    .filter(|name| !keep.contains(name))
                    .collect();
                self.discard_results(&record.task_id, &orphaned).await;
            }
            Write::Written(_) => {}
        }
        self.wake.notify_one();
    }

    async fn discard_results(&self, task_id: &str, names: &[String]) {
        for name in names {
            let _ = task::remove_result(&self.files, task_id, name).await;
        }
    }

    /// The inputs of `record` as files the adapter reads (A.2): inline ones
    /// under `input/`, or the caller's `node_path`.
    async fn job_files(&self, record: &TaskRecord) -> Result<Vec<JobFile>, String> {
        let metadata: Vec<InputFile> = record
            .params
            .get("files")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let input_dir = self
            .files
            .directory(&format!("{}/input", task::task_dir(&record.task_id)))
            .await
            .map_err(|error| error.to_string())?;
        Ok(metadata
            .into_iter()
            .map(|file| JobFile {
                path: match &file.node_path {
                    Some(path) => PathBuf::from(path),
                    None => input_dir.join(&file.name),
                },
                name: file.name,
                media_type: file.media_type,
            })
            .collect())
    }

    /// Records an adapter progress report (A10): the waiters see every
    /// report, the record sees at most one per second.
    fn report_progress(self: &Arc<Self>, slot: &Arc<Slot>, progress: AdapterProgress) {
        let _ = slot.progress.send_replace(Some(progress.clone()));
        let write = {
            let mut last = slot.progress_written.lock().unwrap();
            match *last {
                Some(last) if last.elapsed() < PROGRESS_INTERVAL => false,
                _ => {
                    *last = Some(Instant::now());
                    true
                }
            }
        };
        if !write {
            return;
        }
        let shared = self.clone();
        let slot = slot.clone();
        let persist = progress;
        self.tasks.spawn(async move {
            let _ = shared
                .put(&slot, ExpectKind::NonTerminal, |record| {
                    record.progress = Some(persist.clone());
                })
                .await;
        });
    }
}
