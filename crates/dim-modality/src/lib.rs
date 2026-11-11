//! The image slice of the Modality process owner (`docs/design/plugins/Modality.md`)
//! after the Web settings contract: `modality.capabilities` reports every
//! category with availability, options, the persisted default and the model a
//! task would use now; `modality.configure` validates one operation default and
//! writes it to the user-scope `modality` Settings source with CAS.
//!
//! The catalog covers every A.1 category and operation. Adapter operations
//! (`image.generate`, `image.edit`, `image.enhance`) need a process-scoped
//! [`ModalityAdapter`]; the first slice ships no adapter in the product, so
//! they report unavailable with an `adapter` reason, and the other categories
//! report `build`. A default can still be persisted for any operation with a
//! Settings key when the model is one of its options; options of an operation
//! no adapter serves are the models Provider declares for that operation id.
//! `image.understand` is selection only: its consumer is the Session
//! image-understanding path, which does not read the default yet (§9.4).
//!
//! The task layer (`modality.task.submit/status/list/wait/cancel/result` and
//! the `modalityTasks` process service) stores its records and files in
//! `pluginFiles`, runs at most four tasks per category, and settles every
//! terminal state once under the record's lock.
//!
//! The realtime layer (`modality.realtime.open/send/close`) keeps one session
//! per `open` in this generation's memory, owned by the connection that opened
//! it, and reports each event and the session's end to that connection alone.
//! A session is bounded in frames, in number and in idle time, and every way
//! it can end passes through its supervisor.
//!
//! The plugin lives in this crate, which depends on the App Server for the
//! Settings service and management methods; a host installs it with
//! [`install`].
mod adapter;
mod engine;
mod realtime;
pub mod task;
pub use adapter::{
    AdapterError, AdapterErrorCode, AdapterFuture, AdapterOperation, AdapterOutput,
    AdapterProgress, Job, JobFile, ModalityAdapter, ProgressSink, RealtimeConnection, RealtimeEnd,
    RealtimeEvent, RealtimeFrame, RealtimeFrameKind, RealtimeOpened, RealtimeTransport,
    RemoteCancel, Submitted,
};
use dim_agent_capabilities::{
    host::{
        HostError, HostErrorKind, ModelPortError, ModelSelectionHost, ModelSelectionRequest,
        SelectableModelsHost,
    },
    methods::{Interruption, MethodCall, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::{AgentBuilder, Names};
use dim_agent_types::{ModalityModelSnapshot, ModelCapabilitySnapshot, ModelId};
use dim_app_server::{
    methods::ManagementMethods,
    plugin_files::PluginFileTransactions,
    protocol::Empty,
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
pub use engine::{
    ArtifactHook, Clock, DEFAULT_LIST_LIMIT, MAX_INLINE_BYTES, MAX_LIST_LIMIT, MAX_RESULT_BYTES,
    MAX_RUNNING_PER_CATEGORY, MAX_SUBMISSION_BYTES, MAX_WAIT_MS, ModalityTasks, NoArtifactHook,
    PROGRESS_INTERVAL, SystemClock, TaskCancel, TaskInput, TaskList, TaskListReply, TaskReply,
    TaskResultQuery, TaskResultReply, TaskStatus, TaskSubmit, TaskWait,
};
use engine::{not_found, unavailable};
use realtime::Realtime;
pub use realtime::{DEFAULT_FRAME_BYTES, DEFAULT_IDLE, DEFAULT_SESSIONS, RealtimeLimits};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
pub use task::{TaskRecord, TaskState};
use tokio_util::sync::CancellationToken;

/// One target this build runs on the host itself (decision 4): the category
/// and operation it serves, and the local model the record names.
pub(crate) struct HostTargetSpec {
    pub category: &'static str,
    pub operation: &'static str,
    pub provider_id: &'static str,
    pub model_id: &'static str,
}
/// The build's own OCR target.
pub(crate) const APPLE_VISION_TARGET: HostTargetSpec = HostTargetSpec {
    category: "ocr",
    operation: "recognize",
    provider_id: dim_ocr::APPLE_VISION_PROVIDER,
    model_id: dim_ocr::APPLE_VISION_MODEL,
};
/// The OCR target a plugin supplies through `ocrHelper` (A.2.1).
pub(crate) const PLUGIN_OCR_TARGET: HostTargetSpec = HostTargetSpec {
    category: "ocr",
    operation: "recognize",
    provider_id: dim_ocr::PLUGIN_PROVIDER,
    model_id: dim_ocr::PLUGIN_MODEL,
};
/// The host targets this build runs, beside the catalog they serve.
pub(crate) const HOST_TARGETS: &[HostTargetSpec] = &[APPLE_VISION_TARGET, PLUGIN_OCR_TARGET];

/// The host targets an operation runs, if this build runs it itself.
pub(crate) fn host_target<'a>(
    category: &'a str,
    operation: &'a str,
) -> Option<impl Iterator<Item = &'static HostTargetSpec> + 'a> {
    let mut targets = HOST_TARGETS
        .iter()
        .filter(move |target| target.category == category && target.operation == operation)
        .peekable();
    targets.peek()?;
    Some(targets)
}

/// The host target `model` names for this operation, if any.
pub(crate) fn host_target_for(
    category: &str,
    operation: &str,
    model: &ModelId,
) -> Option<&'static HostTargetSpec> {
    host_target(category, operation)?
        .find(|target| target.provider_id == model.provider_id && target.model_id == model.model_id)
}

/// The adapter service Modality reads per call. A missing service is an
/// unavailable operation, never a failed load.
pub struct ModalityAdapterService(pub Arc<dyn ModalityAdapter>);

/// One catalog operation of Modality A.1: its id and the Settings key that
/// holds its default, if it has one.
pub(crate) struct OperationRow {
    operation: &'static str,
    key: Option<&'static str>,
}
impl OperationRow {
    /// The `<category>.<operation>` id a client names.
    fn id(&self, category: &str) -> String {
        format!("{}.{}", category, self.operation)
    }
}

/// One A.1 category. `category_key` says its operations share one Settings
/// key and `configure` may omit the operation; image and speech key each
/// operation separately, and OCR keys its one operation as `modality.ocr`.
pub(crate) struct CatalogCategory {
    name: &'static str,
    category_key: bool,
    rows: &'static [OperationRow],
}

/// Every category and operation of Modality A.1, in its order. `image` and
/// `speech` key each operation; the other categories share one key, and OCR
/// names its key for the category (A.1, A3).
pub(crate) const CATALOG: [CatalogCategory; 9] = [
    CatalogCategory {
        name: "image",
        category_key: false,
        rows: &[
            OperationRow {
                operation: "generate",
                key: Some("modality.image.generate"),
            },
            OperationRow {
                operation: "edit",
                key: Some("modality.image.edit"),
            },
            OperationRow {
                operation: "enhance",
                key: Some("modality.image.enhance"),
            },
            OperationRow {
                operation: "understand",
                key: Some("modality.image.understand"),
            },
        ],
    },
    CatalogCategory {
        name: "video",
        category_key: true,
        rows: &[
            OperationRow {
                operation: "generate",
                key: Some("modality.video"),
            },
            OperationRow {
                operation: "edit",
                key: Some("modality.video"),
            },
            OperationRow {
                operation: "enhance",
                key: Some("modality.video"),
            },
        ],
    },
    CatalogCategory {
        name: "speech",
        category_key: false,
        rows: &[
            OperationRow {
                operation: "synthesize",
                key: Some("modality.speech.synthesize"),
            },
            OperationRow {
                operation: "transcribe",
                key: Some("modality.speech.transcribe"),
            },
        ],
    },
    CatalogCategory {
        name: "music",
        category_key: true,
        rows: &[OperationRow {
            operation: "generate",
            key: Some("modality.music"),
        }],
    },
    CatalogCategory {
        name: "audio",
        category_key: true,
        rows: &[OperationRow {
            operation: "generate",
            key: Some("modality.audio"),
        }],
    },
    CatalogCategory {
        name: "asset",
        category_key: true,
        rows: &[OperationRow {
            operation: "generate",
            key: Some("modality.asset"),
        }],
    },
    CatalogCategory {
        name: "motion",
        category_key: true,
        rows: &[OperationRow {
            operation: "generate",
            key: Some("modality.motion"),
        }],
    },
    CatalogCategory {
        name: "realtime",
        category_key: true,
        rows: &[
            OperationRow {
                operation: "voice",
                key: Some("modality.realtime"),
            },
            OperationRow {
                operation: "transcribe",
                key: Some("modality.realtime"),
            },
            OperationRow {
                operation: "avatar",
                key: Some("modality.realtime"),
            },
        ],
    },
    CatalogCategory {
        name: "ocr",
        category_key: false,
        rows: &[OperationRow {
            operation: "recognize",
            key: Some("modality.ocr"),
        }],
    },
];

pub(crate) fn catalog_category(name: &str) -> Option<&'static CatalogCategory> {
    CATALOG.iter().find(|category| category.name == name)
}

/// `image.understand` is selection only: its options are selectable vision
/// models, never the adapter (Modality §9.4).
pub(crate) fn is_understand(category: &str, operation: &str) -> bool {
    category == "image" && operation == "understand"
}

/// Every `realtime.<operation>` is opened as a session, never submitted as a
/// task (A.3, decision 6).
pub(crate) fn is_realtime(category: &str) -> bool {
    category == "realtime"
}

/// Whether a model's declared `<category>.<operation>` ids include this
/// operation.
fn declares(operations: &[String], category: &str, operation: &str) -> bool {
    operations.iter().any(|id| {
        id.split_once('.')
            .is_some_and(|(declared_category, declared_operation)| {
                declared_category == category && declared_operation == operation
            })
    })
}

/// A display reason, never parsed by a client (Modality A.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    pub dimension: &'static str,
    pub detail: String,
}
fn reason(dimension: &'static str, detail: impl Into<String>) -> Reason {
    Reason {
        dimension,
        detail: detail.into(),
    }
}

/// A model an operation could use, or a persisted default that no longer can.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionSnapshot {
    pub provider_id: String,
    pub model_id: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
}
impl OptionSnapshot {
    fn model(&self) -> (&str, &str) {
        (&self.provider_id, &self.model_id)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Effective {
    pub provider_id: String,
    pub model_id: String,
    /// `default` or `automatic`.
    pub source: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSnapshot {
    pub operation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<&'static str>,
    pub available: bool,
    pub reasons: Vec<Reason>,
    /// The persisted choice, or `null`.
    pub default: Option<OptionSnapshot>,
    /// The model a task or consumer would use now; never a substitute for an
    /// unavailable default.
    pub effective: Option<Effective>,
    pub options: Vec<OptionSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategorySnapshot {
    pub category: &'static str,
    pub available: bool,
    pub reasons: Vec<Reason>,
    pub operations: Vec<OperationSnapshot>,
}

/// The one shape `modality.capabilities` and `modality.configure` return.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub revision: String,
    pub categories: Vec<CategorySnapshot>,
}

/// The `modality` Settings document: one Provider model reference per
/// operation key. User scope only; a missing source reads as empty defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModalitySettings {
    #[serde(default, deserialize_with = "closed_defaults")]
    pub defaults: BTreeMap<String, ModelId>,
}
impl BusinessDocument for ModalitySettings {
    const NAMESPACE: &'static str = "modality";
    const USER_FILE: &'static str = "modality.json";
    fn validate(&self) -> dim_app_server::settings::Result<()> {
        for (key, model) in &self.defaults {
            if !valid_key(key) {
                return Err(SettingsError::Invalid("invalid modality default key"));
            }
            if model.provider_id.is_empty() || model.model_id.is_empty() {
                return Err(SettingsError::Invalid("empty modality default model"));
            }
        }
        Ok(())
    }
}

/// `modality.<category>[.<operation>]`: the prefix names the setting, not
/// another nesting level (Modality §2).
fn valid_key(key: &str) -> bool {
    key.strip_prefix("modality.").is_some_and(|rest| {
        let parts: Vec<&str> = rest.split('.').collect();
        (1..=2).contains(&parts.len())
            && parts.iter().all(|part| {
                part.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
    })
}

/// `ModelId` accepts unknown fields elsewhere; this closed source does not.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClosedModel {
    provider_id: String,
    model_id: String,
}
impl From<ClosedModel> for ModelId {
    fn from(model: ClosedModel) -> Self {
        Self {
            provider_id: model.provider_id,
            model_id: model.model_id,
        }
    }
}
fn closed_defaults<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, ModelId>, D::Error> {
    Ok(BTreeMap::<String, ClosedModel>::deserialize(deserializer)?
        .into_iter()
        .map(|(key, model)| (key, model.into()))
        .collect())
}
/// Tells an absent `model` from an explicit `null`: absent decodes as `None`,
/// `null` as `Some(None)`, and an object as `Some(Some(model))`.
fn explicit_model<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<ModelId>>, D::Error> {
    Ok(Some(
        Option::<ClosedModel>::deserialize(deserializer)?.map(Into::into),
    ))
}

/// One read of what Provider would serve, the collected adapter and the
/// persisted defaults; snapshots before and after a write share it.
pub(crate) struct Evaluation {
    defaults: BTreeMap<String, ModelId>,
    selectable: Vec<ModelCapabilitySnapshot>,
    /// The models Provider would select for a non-conversational use, with
    /// the operations each declares (Modality §9.2).
    declared: Vec<ModalityModelSnapshot>,
    global: Option<ModelId>,
    adapter: Option<Arc<ModalityAdapterService>>,
    /// Whether this daemon holds a HostPlatform, which a host task runs on
    /// (A14).
    host_platform: bool,
    /// The helper this build starts for its host targets; the product's
    /// installer passes its own executable.
    ocr_helper: Option<PathBuf>,
    /// The plugin-supplied target as this call read it (A.2.1); a call that
    /// cannot reach the OCR target carries [`PluginTargetState::Absent`]
    /// without reading it.
    pp_ocr: PluginTargetState,
}

/// The plugin-supplied OCR target as one call reads it: absent (no service,
/// a failed or null `describe`), present with another contract, or ready.
pub(crate) enum PluginTargetState {
    Absent,
    Outdated,
    Ready(dim_ocr::HelperDescription),
}
impl PluginTargetState {
    /// Why the target cannot run, or `None` when it is ready.
    fn reason(&self) -> Option<Reason> {
        match self {
            Self::Absent => Some(reason("runtime", "The PP-OCR plugin is not running")),
            Self::Outdated => Some(reason("runtime", "The PP-OCR plugin needs an update")),
            Self::Ready(_) => None,
        }
    }
}

/// How long one call waits for `ocrHelper.describe` (A.2.1): the row already
/// holds the value, so only a stalled row ever reaches it.
pub(crate) const PLUGIN_DESCRIBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Reads `ocrHelper.describe` once for this call (A.2.1). A missing service,
/// a failed call, a cancelled call, a reply that misses
/// [`PLUGIN_DESCRIBE_TIMEOUT`], or a reply naming another target is absent.
/// The method is `async`, so the call starts without blocking the calling
/// thread; giving up on the wait drops the reply, which cancels the call and
/// discards a late answer. The contract is read first, ignoring the rest of
/// the reply's shape, so a newer contract reports itself outdated whatever its
/// fields are; only this build's contract is parsed strictly, and a reply that
/// fails that parse is absent.
pub(crate) async fn plugin_target_state(
    ctx: &Ctx,
    cancel: &CancellationToken,
) -> PluginTargetState {
    use rutis_bridge::session::{HostDispatch, Value as ServiceValue, host_key, settle};
    let Some(service) = ctx.get_at::<dyn HostDispatch>(&host_key("ocrHelper")) else {
        return PluginTargetState::Absent;
    };
    // The declared kind decides how the call would run before it is made
    // (A.2.1): only `async` never blocks the calling thread. A `describe`
    // declared with another kind is a package that does not implement this
    // contract's `async` method, and an update is the honest reason; a
    // service that declares no `describe` method is not this contract's
    // service.
    match service.methods() {
        Some(methods) => match methods.get("describe").and_then(|kind| kind.as_str()) {
            Some("async") => {}
            Some(_) => return PluginTargetState::Outdated,
            None => return PluginTargetState::Absent,
        },
        None => return PluginTargetState::Absent,
    }
    let Ok(reply) = service.invoke("describe", ServiceValue::List(Vec::new())) else {
        return PluginTargetState::Absent;
    };
    let value = tokio::select! {
        _ = cancel.cancelled() => return PluginTargetState::Absent,
        reply = tokio::time::timeout(PLUGIN_DESCRIBE_TIMEOUT, settle(reply)) => match reply {
            Ok(Ok(value)) => value,
            _ => return PluginTargetState::Absent,
        },
    };
    let Ok(json) = value.json() else {
        return PluginTargetState::Absent;
    };
    if json.is_null() {
        return PluginTargetState::Absent;
    }
    let version = match serde_json::from_value::<dim_ocr::HelperVersion>(json.clone()) {
        Ok(version) if version.target.names_plugin_target() => version,
        _ => return PluginTargetState::Absent,
    };
    if version.contract != dim_ocr::HELPER_CONTRACT {
        return PluginTargetState::Outdated;
    }
    match serde_json::from_value::<dim_ocr::HelperDescription>(json) {
        Ok(description) if description.names_plugin_target() => {
            PluginTargetState::Ready(description)
        }
        _ => PluginTargetState::Absent,
    }
}
impl Evaluation {
    /// Why the build's own host target cannot run here, or `None` when it can
    /// (decision 4): `platform` for a host without Apple Vision, `build` for a
    /// build that starts no helper or holds no host platform.
    pub(crate) fn host_reason(&self) -> Option<Reason> {
        if !dim_ocr::apple_vision_available() {
            return Some(reason(
                "platform",
                "Apple Vision needs macOS 12 or later on Apple Silicon",
            ));
        }
        if self.ocr_helper.is_none() {
            return Some(reason("build", "No OCR helper in this build"));
        }
        if !self.host_platform {
            return Some(reason("build", "The host platform is unavailable"));
        }
        None
    }
    /// Why the plugin-supplied target cannot run, or `None` when it is ready.
    pub(crate) fn plugin_reason(&self) -> Option<Reason> {
        self.pp_ocr.reason()
    }
    /// Why `model` cannot run as one of this build's host targets; `None`
    /// when it is not a host target or is ready.
    pub(crate) fn host_target_reason(&self, model: &ModelId) -> Option<Reason> {
        if model.provider_id == dim_ocr::APPLE_VISION_PROVIDER
            && model.model_id == dim_ocr::APPLE_VISION_MODEL
        {
            return self.host_reason();
        }
        if model.provider_id == dim_ocr::PLUGIN_PROVIDER && model.model_id == dim_ocr::PLUGIN_MODEL
        {
            return self.plugin_reason();
        }
        None
    }
    /// The options one host operation offers: the build's own target when it
    /// can run here, and the plugin-supplied target when it is ready or
    /// present with another contract (then listed unavailable with its
    /// reason); an absent plugin target is not listed (A.2.1, decision 4).
    fn host_options(&self) -> Vec<OptionSnapshot> {
        let mut options = Vec::new();
        if self.host_reason().is_none() {
            options.push(OptionSnapshot {
                provider_id: dim_ocr::APPLE_VISION_PROVIDER.into(),
                model_id: dim_ocr::APPLE_VISION_MODEL.into(),
                available: true,
                reason: None,
            });
        }
        match &self.pp_ocr {
            PluginTargetState::Ready(_) => options.push(OptionSnapshot {
                provider_id: dim_ocr::PLUGIN_PROVIDER.into(),
                model_id: dim_ocr::PLUGIN_MODEL.into(),
                available: true,
                reason: None,
            }),
            PluginTargetState::Outdated => options.push(OptionSnapshot {
                provider_id: dim_ocr::PLUGIN_PROVIDER.into(),
                model_id: dim_ocr::PLUGIN_MODEL.into(),
                available: false,
                reason: self.plugin_reason(),
            }),
            PluginTargetState::Absent => {}
        }
        options
    }

    fn adapter_available(&self) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|adapter| adapter.0.available())
    }
    /// Whether a process adapter is collected at all, whatever it serves: a
    /// collected adapter that serves no candidate model is not "no adapter".
    fn has_adapter(&self) -> bool {
        self.adapter.is_some()
    }
    /// Whether the adapter serves this category for any model either read
    /// names; its `operations` then decides the category's options.
    fn adapter_serves(&self, category: &str) -> bool {
        let Some(adapter) = &self.adapter else {
            return false;
        };
        self.selectable
            .iter()
            .map(|model| &model.target)
            .chain(self.declared.iter().map(|model| &model.target))
            .any(|model| {
                adapter
                    .0
                    .operations(model)
                    .iter()
                    .any(|served| served.category == category)
            })
    }
    fn serves(&self, model: &ModelId, category: &str, operation: &str) -> bool {
        let Some(adapter) = &self.adapter else {
            return false;
        };
        adapter.0.available()
            && adapter
                .0
                .operations(model)
                .iter()
                .any(|served| served.category == category && served.operation == operation)
    }
    fn selectable(&self, provider_id: &str, model_id: &str) -> Option<&ModelCapabilitySnapshot> {
        self.selectable.iter().find(|model| {
            model.target.provider_id == provider_id && model.target.model_id == model_id
        })
    }
    fn declared_for(&self, provider_id: &str, model_id: &str) -> Option<&ModalityModelSnapshot> {
        self.declared.iter().find(|model| {
            model.target.provider_id == provider_id && model.target.model_id == model_id
        })
    }
    /// Whether either Provider read names this model.
    pub(crate) fn known(&self, model: &ModelId) -> bool {
        self.selectable(&model.provider_id, &model.model_id)
            .is_some()
            || self
                .declared_for(&model.provider_id, &model.model_id)
                .is_some()
    }
    /// The collected adapter, when the process has one.
    pub(crate) fn adapter(&self) -> Option<&Arc<ModalityAdapterService>> {
        self.adapter.as_ref()
    }
    /// The adapter's declaration of one operation for `model`: its parameter
    /// schema, read per submission (A.2, decision 3). `None` when no
    /// available adapter serves the operation for that model.
    pub(crate) fn operation_spec(
        &self,
        model: &ModelId,
        category: &str,
        operation: &str,
    ) -> Option<AdapterOperation> {
        let adapter = self.adapter.as_ref()?;
        if !adapter.0.available() {
            return None;
        }
        adapter
            .0
            .operations(model)
            .into_iter()
            .find(|spec| spec.category == category && spec.operation == operation)
    }
    fn option(model: &ModelId) -> OptionSnapshot {
        OptionSnapshot {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            available: true,
            reason: None,
        }
    }
    /// Options for one operation: Provider's candidates narrowed by the
    /// adapter when it serves the category, a selectable vision model for
    /// understanding, the models Provider declares for a realtime operation,
    /// and otherwise the models Provider declares for the operation id
    /// (Modality §9.2), in each read's order. The candidates of an
    /// adapter-served category are the deduplicated union of the two Provider
    /// reads, selectable first, so a declared media model execution rejects is
    /// still offered when the adapter serves it. A realtime session's adapter
    /// is handed the transport its entry declares, so the entry is the
    /// per-model answer and narrows nothing (A.3, D2). The persisted default
    /// follows when it is no longer an option.
    fn options(&self, category: &str, row: &OperationRow) -> Vec<OptionSnapshot> {
        let mut options: Vec<OptionSnapshot> = if host_target(category, row.operation).is_some() {
            // A host target this platform or build cannot run is absent, never
            // an unavailable option; a plugin target another contract supplies
            // is listed with its reason (A.2.1, decision 4).
            self.host_options()
        } else if is_understand(category, row.operation) {
            self.selectable
                .iter()
                .filter(|model| model.capabilities.vision)
                .map(|model| Self::option(&model.target))
                .collect()
        } else if is_realtime(category) {
            self.declared
                .iter()
                .filter(|model| declares(&model.operations, category, row.operation))
                .map(|model| Self::option(&model.target))
                .collect()
        } else if self.adapter_serves(category) {
            let mut served: Vec<OptionSnapshot> = Vec::new();
            for model in self
                .selectable
                .iter()
                .map(|model| &model.target)
                .chain(self.declared.iter().map(|model| &model.target))
            {
                let (provider_id, model_id) = (model.provider_id.as_str(), model.model_id.as_str());
                if !served
                    .iter()
                    .any(|option| option.model() == (provider_id, model_id))
                    && self.serves(model, category, row.operation)
                {
                    served.push(Self::option(model));
                }
            }
            served
        } else {
            self.declared
                .iter()
                .filter(|model| declares(&model.operations, category, row.operation))
                .map(|model| Self::option(&model.target))
                .collect()
        };
        if let Some(key) = row.key
            && let Some(default) = self.defaults.get(key)
            && !options.iter().any(|option| {
                option.model() == (default.provider_id.as_str(), default.model_id.as_str())
            })
        {
            options.push(OptionSnapshot {
                provider_id: default.provider_id.clone(),
                model_id: default.model_id.clone(),
                available: false,
                reason: Some(self.unavailable_reason(
                    default,
                    category,
                    row.operation,
                    &row.id(category),
                )),
            });
        }
        options
    }
    /// Why `model` is not an available option of the operation, naming it as
    /// the client configured it: `name` is the operation id, or the category
    /// when the write is category-wide. A persisted default and a refused
    /// `configure` share this, so both speak one vocabulary (Modality §9.1).
    pub(crate) fn unavailable_reason(
        &self,
        model: &ModelId,
        category: &str,
        operation: &str,
        name: &str,
    ) -> Reason {
        if is_understand(category, operation) {
            return match self.selectable(&model.provider_id, &model.model_id) {
                None => reason("provider", "Model is no longer selectable"),
                Some(model) if !model.capabilities.vision => {
                    reason("configuration", "Model is not a selectable vision model")
                }
                Some(_) => reason(
                    "adapter",
                    "No available adapter serves the configured model",
                ),
            };
        }
        if host_target(category, operation).is_some() {
            if host_target_for(category, operation, model).is_none() {
                return reason(
                    "configuration",
                    format!("The model does not declare {name}"),
                );
            }
            // The configured model is a host target itself: that target's own
            // reason is the answer when it cannot run here. An available target
            // is never refused, so when there is no reason the shared
            // vocabulary below answers for an unavailable model.
            if let Some(reason) = self.host_target_reason(model) {
                return reason;
            }
        }
        if !self.known(model) {
            return reason("provider", "Model is no longer selectable");
        }
        if self.adapter_serves(category) {
            reason(
                "adapter",
                "No available adapter serves the configured model",
            )
        } else {
            reason(
                "configuration",
                format!("The model does not declare {name}"),
            )
        }
    }
    pub(crate) fn operation(&self, category: &str, row: &OperationRow) -> OperationSnapshot {
        let options = self.options(category, row);
        let served = options.iter().any(|option| option.available);
        let (available, reasons) = if is_understand(category, row.operation) {
            if served {
                (true, vec![])
            } else {
                (
                    false,
                    vec![reason("configuration", "No selectable vision model")],
                )
            }
        } else if self.adapter_serves(category) {
            match self.adapter_available() {
                false => (false, vec![reason("runtime", "The adapter is unavailable")]),
                // A realtime operation names itself: the adapter serves the
                // category for one of its operations and nothing serves this
                // one, which a client reads as an adapter gap, not a
                // configuration one (A.3).
                true if !served && is_realtime(category) => (
                    false,
                    vec![reason(
                        "adapter",
                        format!("No available adapter serves {}", row.id(category)),
                    )],
                ),
                true if !served => (
                    false,
                    vec![reason(
                        "configuration",
                        "No selectable model is served by an available adapter",
                    )],
                ),
                true => (true, vec![]),
            }
        } else if host_target(category, row.operation).is_some() {
            if served {
                (true, vec![])
            } else {
                let mut reasons = Vec::new();
                if let Some(reason) = self.host_reason() {
                    reasons.push(reason);
                }
                if let Some(reason) = self.plugin_reason() {
                    reasons.push(reason);
                }
                (false, reasons)
            }
        } else if category == "image" {
            let detail = if self.has_adapter() {
                "The adapter serves no image model Provider lists"
            } else {
                "No image adapter in this build"
            };
            (false, vec![reason("adapter", detail)])
        } else {
            (false, vec![reason("build", "Not in this build")])
        };
        let default = row.key.and_then(|key| self.defaults.get(key)).map(|model| {
            let (provider_id, model_id) = (model.provider_id.as_str(), model.model_id.as_str());
            options
                .iter()
                .find(|option| option.model() == (provider_id, model_id))
                .cloned()
                .unwrap_or_else(|| OptionSnapshot {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    available: false,
                    reason: Some(self.unavailable_reason(
                        model,
                        category,
                        row.operation,
                        &row.id(category),
                    )),
                })
        });
        let effective = match &default {
            Some(default) if default.available => Some(Effective {
                provider_id: default.provider_id.clone(),
                model_id: default.model_id.clone(),
                source: "default",
            }),
            Some(_) => None,
            None => {
                let automatic = self
                    .global
                    .as_ref()
                    .filter(|global| {
                        options.iter().any(|option| {
                            option.model()
                                == (global.provider_id.as_str(), global.model_id.as_str())
                        })
                    })
                    .cloned()
                    .or_else(|| {
                        options
                            .iter()
                            .find(|option| option.available)
                            .map(|option| ModelId {
                                provider_id: option.provider_id.clone(),
                                model_id: option.model_id.clone(),
                            })
                    });
                automatic.map(|model| Effective {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                    source: "automatic",
                })
            }
        };
        OperationSnapshot {
            operation: row.operation,
            key: row.key,
            available,
            reasons,
            default,
            effective,
            options,
        }
    }
    fn category(&self, category: &CatalogCategory) -> CategorySnapshot {
        let operations: Vec<OperationSnapshot> = category
            .rows
            .iter()
            .map(|row| self.operation(category.name, row))
            .collect();
        let available = operations.iter().any(|operation| operation.available);
        let reasons = if available {
            vec![]
        } else {
            operations
                .iter()
                .flat_map(|operation| operation.reasons.iter().cloned())
                .fold(Vec::new(), |mut reasons, reason| {
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                    reasons
                })
        };
        CategorySnapshot {
            category: category.name,
            available,
            reasons,
            operations,
        }
    }
    fn snapshot(&self, revision: &str) -> Snapshot {
        Snapshot {
            revision: revision.to_owned(),
            categories: CATALOG
                .iter()
                .map(|category| self.category(category))
                .collect(),
        }
    }
}

fn settings_error(error: SettingsError) -> RpcError {
    match error {
        SettingsError::Closed => RpcError::reloading(),
        SettingsError::Cancelled => RpcError::interrupted(),
        SettingsError::Conflict => RpcError::app(
            -32000,
            "modality_conflict",
            "Modality defaults changed; read the current snapshot before saving again",
        ),
        error => RpcError::app(-32000, "modality_failed", error.to_string()),
    }
}

pub(crate) fn host_error(error: HostError) -> RpcError {
    match error {
        HostError::Cancelled(_) => RpcError::interrupted(),
        HostError::Failed {
            kind: HostErrorKind::OwnerClosed,
            ..
        } => RpcError::reloading(),
        error => RpcError::app(-32000, "modality_failed", error.to_string()),
    }
}

/// A process model port's failure as this plugin reports it: the two codes a
/// caller acts on, and the owner's message for the rest.
pub(crate) fn port_error(error: ModelPortError) -> RpcError {
    match error {
        ModelPortError::Cancelled { .. } => RpcError::interrupted(),
        ModelPortError::OwnerClosed { .. } => RpcError::reloading(),
        error => RpcError::app(-32000, "modality_failed", error.to_string()),
    }
}

/// Whether one call reads the plugin-supplied OCR target (A.2.1): only a call
/// whose reply or resolution names the OCR category reads `ocrHelper`, so an
/// image, video or speech request and a realtime session never wait on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OcrLookup {
    /// This call cannot reach the OCR target.
    Skip,
    /// This call's reply or resolved operation carries the OCR target: read
    /// `describe` once.
    Read,
}

pub(crate) async fn evaluate(
    settings: &SettingsService,
    selectable: &SelectableModelsHost,
    selection: &ModelSelectionHost,
    ctx: &Ctx,
    ocr_helper: Option<&std::path::Path>,
    cancel: &CancellationToken,
    ocr: OcrLookup,
) -> Result<(String, Evaluation), RpcError> {
    let snapshot = settings
        .read::<ModalitySettings>(SettingsScope::User, cancel)
        .await
        .map_err(settings_error)?;
    let selectable_models = selectable
        .selectable_models()
        .await
        .map_err(port_error)?
        .models;
    let declared = selectable
        .selectable_for_modality()
        .await
        .map_err(port_error)?
        .models;
    let global = selection
        .resolve(ModelSelectionRequest::at(None, None).map_err(|error| host_error(error.into()))?)
        .await
        .ok()
        .map(|selection| ModelId {
            provider_id: selection.provider_id,
            model_id: selection.model_id,
        });
    Ok((
        snapshot.revision,
        Evaluation {
            defaults: snapshot.document.defaults,
            selectable: selectable_models,
            declared,
            global,
            adapter: ctx.get::<ModalityAdapterService>(),
            host_platform: ctx
                .get::<dim_agent_capabilities::host_platform::HostPlatform>()
                .is_some(),
            ocr_helper: ocr_helper.map(|path| path.to_owned()),
            pp_ocr: match ocr {
                OcrLookup::Skip => PluginTargetState::Absent,
                OcrLookup::Read => plugin_target_state(ctx, cancel).await,
            },
        },
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Configure {
    expected_revision: String,
    category: String,
    #[serde(default)]
    operation: Option<String>,
    /// Absent is malformed; `null` clears the key.
    #[serde(default, deserialize_with = "explicit_model")]
    model: Option<Option<ModelId>>,
}

pub(crate) fn unknown_operation(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "modality_unknown_operation", detail)
}
fn invalid_selection(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "modality_invalid_selection", detail)
}

/// One operation `modality.configure` writes: its category, the row whose id
/// is named (the category's first row when the operation is omitted), the
/// Settings key, and whether the model may come from any operation of the
/// category.
struct ConfiguredOperation {
    category: &'static CatalogCategory,
    row: &'static OperationRow,
    key: &'static str,
    category_wide: bool,
}

fn configured_operation(params: &Configure) -> Result<ConfiguredOperation, RpcError> {
    let category = catalog_category(&params.category)
        .ok_or_else(|| unknown_operation(format!("Unknown category {}", params.category)))?;
    let (row, category_wide) = match params.operation.as_deref() {
        Some(operation) => (
            category
                .rows
                .iter()
                .find(|row| row.operation == operation)
                .ok_or_else(|| {
                    unknown_operation(format!("Unknown {} operation {}", category.name, operation))
                })?,
            false,
        ),
        None if category.category_key => (
            category
                .rows
                .first()
                .ok_or_else(|| unknown_operation(format!("Unknown category {}", category.name)))?,
            true,
        ),
        None => {
            return Err(unknown_operation(format!(
                "An operation is required for {}",
                category.name
            )));
        }
    };
    let key = row.key.ok_or_else(|| {
        unknown_operation(format!(
            "{}.{} has no default",
            category.name, row.operation
        ))
    })?;
    Ok(ConfiguredOperation {
        category,
        row,
        key,
        category_wide,
    })
}

/// The refusal for a model that is not an available option of the configured
/// operation: the detail is the reason the snapshot would carry for that
/// model, so a client reads one vocabulary however it learns the refusal.
fn selection_refusal(
    evaluation: &Evaluation,
    configured: &ConfiguredOperation,
    model: &ModelId,
) -> RpcError {
    let name = if configured.category_wide {
        configured.category.name.to_owned()
    } else {
        configured.row.id(configured.category.name)
    };
    let reason = evaluation.unavailable_reason(
        model,
        configured.category.name,
        configured.row.operation,
        &name,
    );
    invalid_selection(reason.detail)
}

/// Process owner of the `modality` Settings source and the `modality.*`
/// management namespace (Modality §1, A.3).
pub struct ModalityPlugin {
    settings: Key<SettingsService>,
    methods: Key<ManagementMethods>,
    selection: Key<ModelSelectionHost>,
    selectable: Key<SelectableModelsHost>,
    files: Key<PluginFileTransactions>,
    options: InstallOptions,
}
impl ModalityPlugin {
    pub fn new(names: &Names<'_>, options: InstallOptions) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            methods: names.key()?,
            selection: names.key()?,
            selectable: names.key()?,
            files: names.key()?,
            options,
        })
    }
}
impl DynPlugin for ModalityPlugin {
    fn name(&self) -> &str {
        "Modality"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.settings.erase(),
            self.methods.erase(),
            self.selection.erase(),
            self.selectable.erase(),
            self.files.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let methods = ctx.require_at(&self.methods)?;
            let selection = ctx.require_at(&self.selection)?;
            let selectable = ctx.require_at(&self.selectable)?;
            let files = ctx.require_at(&self.files)?;
            settings.register::<ModalitySettings>(ctx)?;
            // The task layer: one state for the methods, the scheduler and
            // `modalityTasks` (A.3, A.6).
            let shared = Arc::new(engine::Shared::new(
                files,
                settings.clone(),
                selection.clone(),
                selectable.clone(),
                ctx.clone(),
                self.options.clock.clone(),
                self.options.ocr_helper.clone(),
                self.options.artifact.clone(),
            ));
            let tasks = Arc::new(ModalityTasks::new(shared.clone()));
            let task_handle = tasks.clone();
            ctx.provide_at(Key::<ModalityTasks>::named("modalityTasks"), tasks)?;
            // The realtime layer: sessions live in this generation and end
            // with it (§3.3).
            let realtime = Arc::new(Realtime::new(shared.clone(), self.options.realtime));
            let realtime_handle = realtime.clone();
            let cleanup = shared.clone();
            let cleanup_realtime = realtime.clone();
            ctx.effect(move || async move {
                cleanup_realtime.shutdown().await;
                cleanup.shutdown().await;
                Ok(())
            })?;
            shared.spawn_scheduler();
            // Startup on every apply (A.6): settle the records this
            // generation cannot continue before the methods are visible.
            shared.recover().await;
            let context = ctx.clone();
            let ocr_helper = self.options.ocr_helper.clone();
            let mut wait = MethodDecl::new("modality.task.wait", false, Interruption::Report);
            wait.progress = true;
            let mut open = MethodDecl::new("modality.realtime.open", true, Interruption::Report);
            open.notifications = vec![
                "modality.realtime.event".to_owned(),
                "modality.realtime.ended".to_owned(),
            ];
            methods.register(
                ctx,
                "modality",
                vec![
                    MethodDecl::new("modality.capabilities", false, Interruption::Report),
                    MethodDecl::new("modality.configure", true, Interruption::Report),
                    MethodDecl::new("modality.task.submit", true, Interruption::Drain),
                    MethodDecl::new("modality.task.status", false, Interruption::Report),
                    MethodDecl::new("modality.task.list", false, Interruption::Report),
                    wait,
                    MethodDecl::new("modality.task.cancel", true, Interruption::Report),
                    MethodDecl::new("modality.task.result", false, Interruption::Report),
                    open,
                    MethodDecl::new("modality.realtime.send", true, Interruption::Report),
                    MethodDecl::new("modality.realtime.close", true, Interruption::Report),
                ],
                Arc::new(move |call, stopping, progress| {
                    let MethodCall {
                        method,
                        params: value,
                        origin,
                    } = call;
                    let settings = settings.clone();
                    let selection = selection.clone();
                    let selectable = selectable.clone();
                    let context = context.clone();
                    let tasks = task_handle.clone();
                    let ocr_helper = ocr_helper.clone();
                    let realtime = realtime_handle.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "modality.capabilities" => {
                                let _: Empty = params(value)?;
                                let (revision, evaluation) = evaluate(
                                    &settings,
                                    &selectable,
                                    &selection,
                                    &context,
                                    ocr_helper.as_deref(),
                                    &stopping,
                                    // The snapshot carries every category,
                                    // the OCR one included.
                                    OcrLookup::Read,
                                )
                                .await?;
                                reply(evaluation.snapshot(&revision))
                            }
                            "modality.configure" => {
                                let params: Configure = params(value)?;
                                let configured = configured_operation(&params)?;
                                let Some(model) = &params.model else {
                                    return Err(RpcError::invalid(
                                        "model is required; use null to clear the default",
                                    ));
                                };
                                let (_, mut evaluation) = evaluate(
                                    &settings,
                                    &selectable,
                                    &selection,
                                    &context,
                                    ocr_helper.as_deref(),
                                    &stopping,
                                    // The reply is the same snapshot: every
                                    // category, the OCR one included.
                                    OcrLookup::Read,
                                )
                                .await?;
                                if let Some(model) = model {
                                    let options: Vec<OptionSnapshot> = if configured.category_wide {
                                        configured
                                            .category
                                            .rows
                                            .iter()
                                            .flat_map(|row| {
                                                evaluation.options(configured.category.name, row)
                                            })
                                            .collect()
                                    } else {
                                        evaluation.options(configured.category.name, configured.row)
                                    };
                                    // A category-wide save flattens every row,
                                    // and each row appends the persisted
                                    // default when it is not an option, marked
                                    // unavailable. The model is valid when any
                                    // row offers it as available.
                                    let selected = options.iter().any(|option| {
                                        option.model()
                                            == (model.provider_id.as_str(), model.model_id.as_str())
                                            && option.available
                                    });
                                    if !selected {
                                        return Err(selection_refusal(
                                            &evaluation,
                                            &configured,
                                            model,
                                        ));
                                    }
                                }
                                let mut document = ModalitySettings {
                                    defaults: evaluation.defaults.clone(),
                                };
                                match model {
                                    Some(model) => {
                                        document
                                            .defaults
                                            .insert(configured.key.into(), model.clone());
                                    }
                                    None => {
                                        document.defaults.remove(configured.key);
                                    }
                                }
                                let updated = settings
                                    .update::<ModalitySettings>(
                                        SettingsScope::User,
                                        &params.expected_revision,
                                        document,
                                        &stopping,
                                    )
                                    .await
                                    .map_err(settings_error)?;
                                evaluation.defaults = updated.document.defaults;
                                reply(evaluation.snapshot(&updated.revision))
                            }
                            "modality.task.submit" => {
                                let request: TaskSubmit = params(value)?;
                                reply(tasks.submit(request, &stopping).await?)
                            }
                            "modality.task.status" => {
                                let request: TaskStatus = params(value)?;
                                reply(tasks.status(request).await?)
                            }
                            "modality.task.list" => {
                                let request: TaskList = params(value)?;
                                reply(tasks.list(request).await?)
                            }
                            "modality.task.wait" => {
                                let request: TaskWait = params(value)?;
                                reply(tasks.wait(request, &stopping, Some(&progress)).await?)
                            }
                            "modality.task.cancel" => {
                                let request: TaskCancel = params(value)?;
                                reply(tasks.cancel(request, &stopping).await?)
                            }
                            "modality.task.result" => {
                                let request: TaskResultQuery = params(value)?;
                                reply(tasks.result(request, &stopping).await?)
                            }
                            "modality.realtime.open" => {
                                let request: realtime::RealtimeOpen = params(value)?;
                                let Some(caller) = origin.caller.clone() else {
                                    return Err(unavailable(
                                        "A realtime session needs a client connection",
                                    ));
                                };
                                reply(realtime.open(request, caller, &stopping).await?)
                            }
                            "modality.realtime.send" => {
                                let request: realtime::RealtimeSend = params(value)?;
                                let Some(caller) = origin.caller.clone() else {
                                    return Err(not_found("No realtime session on this caller"));
                                };
                                reply(realtime.send(request, &caller, &stopping).await?)
                            }
                            "modality.realtime.close" => {
                                let request: realtime::RealtimeClose = params(value)?;
                                let Some(caller) = origin.caller.clone() else {
                                    return Err(not_found("No realtime session on this caller"));
                                };
                                reply(realtime.close(request, &caller)?)
                            }
                            _ => unreachable!("registered modality management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

/// Installs Modality as an optional process plugin: the `modality` Settings
/// source, the `modality.*` management namespace and the `modalityTasks`
/// process service. The host must register the names of the services it reads
/// (settings, managementMethods, pluginFiles and Provider's selection and
/// selectable-model reads), as the App Server's assembly does; when one of
/// them has no provider, the host still starts and opens Sessions, and only
/// `modality.*` is unavailable.
pub fn install(builder: &mut AgentBuilder) {
    install_with(builder, InstallOptions::default());
}

/// What an installation varies: the clock a generation reads, the bounds on
/// realtime sessions and the executable host targets run. `Default` is the
/// product's set; a test injects its own so a lifetime rule or the cap can be
/// observed without waiting.
#[derive(Clone)]
pub struct InstallOptions {
    /// The clock a generation reads; the wall clock by default. A test
    /// injects one to control retention.
    pub clock: Arc<dyn Clock>,
    /// The bounds on realtime sessions a generation enforces; the product's
    /// set by default.
    pub realtime: RealtimeLimits,
    /// The executable that serves host targets: this build's own, started
    /// with the hidden `ocr-helper` subcommand. `None` starts none, and the
    /// host targets are absent from the snapshot.
    pub ocr_helper: Option<PathBuf>,
    /// The seam between one host artifact's size check and its copy; the
    /// product's does nothing. A test injects one to change that work file in
    /// the window.
    pub artifact: Arc<dyn ArtifactHook>,
}
impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            realtime: RealtimeLimits::default(),
            ocr_helper: None,
            artifact: Arc::new(NoArtifactHook),
        }
    }
}
impl std::fmt::Debug for InstallOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallOptions")
            .field("ocr_helper", &self.ocr_helper)
            .finish_non_exhaustive()
    }
}

/// [`install`] with what the generation reads; a test injects a clock to
/// control retention, short realtime bounds to observe their rules, and a
/// host executable to observe the OCR path.
pub fn install_with(builder: &mut AgentBuilder, options: InstallOptions) {
    dim_agent_runtime::names::register::<ModalityAdapterService>(builder.catalog_mut(), None);
    builder.optional_plugin(move |names| {
        Ok(Box::new(ModalityPlugin::new(names, options.clone())?) as Box<dyn DynPlugin>)
    });
}
