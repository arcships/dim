//! The image slice of the Modality process owner (`docs/design/plugins/Modality.md`)
//! after the Web settings contract: `modality.capabilities` reports every
//! category with availability, options, the persisted default and the model a
//! task would use now; `modality.configure` validates one operation default and
//! writes it to the user-scope `modality` Settings source with CAS.
//!
//! Adapter operations (`image.generate`, `image.edit`, `image.enhance`) need a
//! process-scoped [`ModalityAdapter`]; the first slice ships no adapter in the
//! product, so they report unavailable with an `adapter` reason and their
//! defaults cannot be set. `image.understand` is selection only: its consumer
//! is the Session image-understanding path, which does not read the default yet
//! (§9.4). Task methods and provider connections are not part of this slice.
use crate::{
    methods::ManagementMethods,
    protocol::Empty,
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
use dim_agent_capabilities::{
    host::{HostError, HostErrorKind, ModelSelectionHost, SelectableModelsHost},
    methods::{Interruption, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::Names;
use dim_agent_types::{ModelCapabilitySnapshot, ModelId};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// One operation a Modality adapter serves for a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterOperation {
    pub category: String,
    pub operation: String,
}

/// A process-scoped `modalityAdapter` (Modality A.2). The first slice needs
/// only availability and the operations a model is served for; submit, wait and
/// cancellation belong to the task slice.
pub trait ModalityAdapter: Send + Sync + 'static {
    fn available(&self) -> bool;
    fn operations(&self, model: &ModelId) -> Vec<AdapterOperation>;
}

/// The adapter service Modality reads per call. A missing service is an
/// unavailable operation, never a failed load.
pub struct ModalityAdapterService(pub Arc<dyn ModalityAdapter>);

/// The four image operations. Each has its own Settings key; there is no
/// category fallback (Modality A.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageOperation {
    Generate,
    Edit,
    Enhance,
    Understand,
}
impl ImageOperation {
    const ALL: [Self; 4] = [Self::Generate, Self::Edit, Self::Enhance, Self::Understand];
    fn id(self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::Edit => "edit",
            Self::Enhance => "enhance",
            Self::Understand => "understand",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Generate => "modality.image.generate",
            Self::Edit => "modality.image.edit",
            Self::Enhance => "modality.image.enhance",
            Self::Understand => "modality.image.understand",
        }
    }
    fn is_adapter(self) -> bool {
        self != Self::Understand
    }
    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|operation| operation.id() == id)
    }
}

/// One catalog operation: its default key, when it has one, and its id.
type OperationRow = (Option<&'static str>, &'static str);

/// Every non-image category and operation of Modality A.1. The image category
/// is built from [`ImageOperation`]; these report a `build` reason until their
/// adapters and runtime exist.
const CATALOG: [(&str, &[OperationRow]); 8] = [
    (
        "video",
        &[
            (Some("modality.video"), "generate"),
            (Some("modality.video"), "edit"),
            (Some("modality.video"), "enhance"),
        ],
    ),
    (
        "speech",
        &[
            (Some("modality.speech.synthesize"), "synthesize"),
            (Some("modality.speech.transcribe"), "transcribe"),
        ],
    ),
    ("music", &[(Some("modality.music"), "generate")]),
    ("audio", &[(Some("modality.audio"), "generate")]),
    ("asset", &[(Some("modality.asset"), "generate")]),
    ("motion", &[(Some("modality.motion"), "generate")]),
    ("realtime", &[(Some("modality.realtime"), "connect")]),
    ("ocr", &[(None, "recognize")]),
];

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
    fn validate(&self) -> crate::settings::Result<()> {
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
struct Evaluation {
    defaults: BTreeMap<String, ModelId>,
    selectable: Vec<ModelCapabilitySnapshot>,
    global: Option<ModelId>,
    adapter: Option<Arc<ModalityAdapterService>>,
}
impl Evaluation {
    fn adapter_available(&self) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|adapter| adapter.0.available())
    }
    fn serves(&self, model: &ModelId, operation: ImageOperation) -> bool {
        let Some(adapter) = &self.adapter else {
            return false;
        };
        adapter.0.available()
            && adapter
                .0
                .operations(model)
                .iter()
                .any(|served| served.category == "image" && served.operation == operation.id())
    }
    fn selectable(&self, provider_id: &str, model_id: &str) -> Option<&ModelCapabilitySnapshot> {
        self.selectable.iter().find(|model| {
            model.target.provider_id == provider_id && model.target.model_id == model_id
        })
    }
    /// Options for `operation` in Provider's selectable-model order, after the current
    /// default when it can no longer be selected or served.
    fn options(&self, operation: ImageOperation) -> Vec<OptionSnapshot> {
        let mut options: Vec<OptionSnapshot> = self
            .selectable
            .iter()
            .filter(|model| match operation {
                ImageOperation::Understand => model.capabilities.vision,
                operation => self.serves(&model.target, operation),
            })
            .map(|model| OptionSnapshot {
                provider_id: model.target.provider_id.clone(),
                model_id: model.target.model_id.clone(),
                available: true,
                reason: None,
            })
            .collect();
        if let Some(default) = self.defaults.get(operation.key())
            && !options.iter().any(|option| {
                option.model() == (default.provider_id.as_str(), default.model_id.as_str())
            })
        {
            options.push(OptionSnapshot {
                provider_id: default.provider_id.clone(),
                model_id: default.model_id.clone(),
                available: false,
                reason: Some(self.unavailable_reason(default, operation)),
            });
        }
        options
    }
    fn unavailable_reason(&self, model: &ModelId, operation: ImageOperation) -> Reason {
        match self.selectable(&model.provider_id, &model.model_id) {
            None => reason("provider", "Model is no longer selectable"),
            Some(model)
                if operation == ImageOperation::Understand && !model.capabilities.vision =>
            {
                reason("configuration", "Model is not a selectable vision model")
            }
            Some(_) => reason(
                "adapter",
                "No available adapter serves the configured model",
            ),
        }
    }
    fn operation(&self, operation: ImageOperation) -> OperationSnapshot {
        let options = self.options(operation);
        let served = options.iter().any(|option| option.available);
        let (available, reasons) = if operation.is_adapter() {
            match (&self.adapter, self.adapter_available()) {
                (None, _) => (
                    false,
                    vec![reason("adapter", "No image adapter in this build")],
                ),
                (Some(_), false) => (
                    false,
                    vec![reason("runtime", "The image adapter is unavailable")],
                ),
                (Some(_), true) if !served => (
                    false,
                    vec![reason(
                        "configuration",
                        "No selectable model is served by an available adapter",
                    )],
                ),
                (Some(_), true) => (true, vec![]),
            }
        } else if !served {
            (
                false,
                vec![reason("configuration", "No selectable vision model")],
            )
        } else {
            (true, vec![])
        };
        let default = self.defaults.get(operation.key()).map(|model| {
            let (provider_id, model_id) = (model.provider_id.as_str(), model.model_id.as_str());
            options
                .iter()
                .find(|option| option.model() == (provider_id, model_id))
                .cloned()
                .unwrap_or_else(|| OptionSnapshot {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    available: false,
                    reason: Some(self.unavailable_reason(model, operation)),
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
            operation: operation.id(),
            key: Some(operation.key()),
            available,
            reasons,
            default,
            effective,
            options,
        }
    }
    fn image(&self) -> CategorySnapshot {
        let operations: Vec<OperationSnapshot> = ImageOperation::ALL
            .into_iter()
            .map(|operation| self.operation(operation))
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
            category: "image",
            available,
            reasons,
            operations,
        }
    }
    fn snapshot(&self, revision: &str) -> Snapshot {
        let categories = std::iter::once(self.image())
            .chain(CATALOG.iter().map(|(category, operations)| {
                CategorySnapshot {
                    category,
                    available: false,
                    reasons: vec![reason("build", "Not in this build")],
                    operations: operations
                        .iter()
                        .map(|(key, operation)| OperationSnapshot {
                            operation,
                            key: *key,
                            available: false,
                            reasons: vec![reason("build", "Not in this build")],
                            default: None,
                            effective: None,
                            options: vec![],
                        })
                        .collect(),
                }
            }))
            .collect();
        Snapshot {
            revision: revision.to_owned(),
            categories,
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

fn host_error(error: HostError) -> RpcError {
    match error {
        HostError::Cancelled(_) => RpcError::interrupted(),
        HostError::Failed {
            kind: HostErrorKind::OwnerClosed,
            ..
        } => RpcError::reloading(),
        error => RpcError::app(-32000, "modality_failed", error.to_string()),
    }
}

async fn evaluate(
    settings: &SettingsService,
    selectable: &SelectableModelsHost,
    selection: &ModelSelectionHost,
    ctx: &Ctx,
    cancel: &CancellationToken,
) -> Result<(String, Evaluation), RpcError> {
    let snapshot = settings
        .read::<ModalitySettings>(SettingsScope::User, cancel)
        .await
        .map_err(settings_error)?;
    let selectable = selectable
        .0
        .selectable(cancel.clone())
        .await
        .map_err(host_error)?;
    let global = selection
        .0
        .resolve(None, None, cancel.clone())
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
            selectable,
            global,
            adapter: ctx.get::<ModalityAdapterService>(),
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

fn unknown_operation(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "modality_unknown_operation", detail)
}
fn unavailable(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "modality_unavailable", detail)
}
fn invalid_selection(detail: impl Into<String>) -> RpcError {
    RpcError::app(-32000, "modality_invalid_selection", detail)
}

fn configured_operation(params: &Configure) -> Result<ImageOperation, RpcError> {
    if params.category != "image" {
        return Err(unavailable(format!(
            "Category {} is not in this build",
            params.category
        )));
    }
    params
        .operation
        .as_deref()
        .and_then(ImageOperation::from_id)
        .ok_or_else(|| unknown_operation("Unknown image operation"))
}

/// Process owner of the `modality` Settings source and the `modality.*`
/// management namespace (Modality §1, A.3).
pub struct ModalityPlugin {
    settings: Key<SettingsService>,
    methods: Key<ManagementMethods>,
    selection: Key<ModelSelectionHost>,
    selectable: Key<SelectableModelsHost>,
}
impl ModalityPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            settings: names.key()?,
            methods: names.key()?,
            selection: names.key()?,
            selectable: names.key()?,
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
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let settings = ctx.require_at(&self.settings)?;
            let methods = ctx.require_at(&self.methods)?;
            let selection = ctx.require_at(&self.selection)?;
            let selectable = ctx.require_at(&self.selectable)?;
            settings.register::<ModalitySettings>(ctx)?;
            let context = ctx.clone();
            methods.register(
                ctx,
                "modality",
                vec![
                    MethodDecl::new("modality.capabilities", false, Interruption::Report),
                    MethodDecl::new("modality.configure", true, Interruption::Report),
                ],
                Arc::new(move |method, value, stopping, _| {
                    let settings = settings.clone();
                    let selection = selection.clone();
                    let selectable = selectable.clone();
                    let context = context.clone();
                    Box::pin(async move {
                        match method.as_str() {
                            "modality.capabilities" => {
                                let _: Empty = params(value)?;
                                let (revision, evaluation) = evaluate(
                                    &settings, &selectable, &selection, &context, &stopping,
                                )
                                .await?;
                                reply(evaluation.snapshot(&revision))
                            }
                            "modality.configure" => {
                                let params: Configure = params(value)?;
                                let operation = configured_operation(&params)?;
                                let Some(model) = &params.model else {
                                    return Err(RpcError::invalid(
                                        "model is required; use null to clear the default",
                                    ));
                                };
                                let (_, mut evaluation) = evaluate(
                                    &settings, &selectable, &selection, &context, &stopping,
                                )
                                .await?;
                                if let Some(model) = model {
                                    let selected = evaluation
                                        .options(operation)
                                        .into_iter()
                                        .find(|option| {
                                            option.model()
                                                == (model.provider_id.as_str(), model.model_id.as_str())
                                        })
                                        .filter(|option| option.available);
                                    if selected.is_none() {
                                        return Err(invalid_selection(if operation.is_adapter() {
                                            format!(
                                                "No available adapter serves this model for image.{}",
                                                operation.id()
                                            )
                                        } else {
                                            "This model is not a selectable vision model".into()
                                        }));
                                    }
                                }
                                let mut document = ModalitySettings {
                                    defaults: evaluation.defaults.clone(),
                                };
                                match model {
                                    Some(model) => {
                                        document
                                            .defaults
                                            .insert(operation.key().into(), model.clone());
                                    }
                                    None => {
                                        document.defaults.remove(operation.key());
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
                            _ => unreachable!("registered modality management method"),
                        }
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}
