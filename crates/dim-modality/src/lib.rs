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
//! image-understanding path, which does not read the default yet (§9.4). Task
//! methods and provider connections are not part of this slice.
//!
//! The plugin lives in this crate, which depends on the App Server for the
//! Settings service and management methods; a host installs it with
//! [`install`].
use dim_agent_capabilities::{
    host::{HostError, HostErrorKind, ModelSelectionHost, SelectableModelsHost},
    methods::{Interruption, MethodDecl, RpcError, params, reply},
};
use dim_agent_runtime::{AgentBuilder, Names};
use dim_agent_types::{ModalityModelSnapshot, ModelCapabilitySnapshot, ModelId};
use dim_app_server::{
    methods::ManagementMethods,
    protocol::Empty,
    settings::{BusinessDocument, SettingsError, SettingsScope, SettingsService},
};
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

/// One catalog operation of Modality A.1: its id and the Settings key that
/// holds its default, if it has one.
struct OperationRow {
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
/// operation separately, and OCR has no default key at all.
struct CatalogCategory {
    name: &'static str,
    category_key: bool,
    rows: &'static [OperationRow],
}

/// Every category and operation of Modality A.1, in its order. `image` and
/// `speech` key each operation; the other categories share one key; OCR has
/// none (A.1, A3).
const CATALOG: [CatalogCategory; 9] = [
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
        rows: &[OperationRow {
            operation: "connect",
            key: Some("modality.realtime"),
        }],
    },
    CatalogCategory {
        name: "ocr",
        category_key: false,
        rows: &[OperationRow {
            operation: "recognize",
            key: None,
        }],
    },
];

fn catalog_category(name: &str) -> Option<&'static CatalogCategory> {
    CATALOG.iter().find(|category| category.name == name)
}

/// `image.understand` is selection only: its options are selectable vision
/// models, never the adapter (Modality §9.4).
fn is_understand(category: &str, operation: &str) -> bool {
    category == "image" && operation == "understand"
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
struct Evaluation {
    defaults: BTreeMap<String, ModelId>,
    selectable: Vec<ModelCapabilitySnapshot>,
    /// The models Provider would select for a non-conversational use, with
    /// the operations each declares (Modality §9.2).
    declared: Vec<ModalityModelSnapshot>,
    global: Option<ModelId>,
    adapter: Option<Arc<ModalityAdapterService>>,
}
impl Evaluation {
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
    fn known(&self, model: &ModelId) -> bool {
        self.selectable(&model.provider_id, &model.model_id)
            .is_some()
            || self
                .declared_for(&model.provider_id, &model.model_id)
                .is_some()
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
    /// understanding, and otherwise the models Provider declares for the
    /// operation id (Modality §9.2), in each read's order. The candidates of
    /// an adapter-served category are the deduplicated union of the two
    /// Provider reads, selectable first, so a declared media model execution
    /// rejects is still offered when the adapter serves it. The persisted
    /// default follows when it is no longer an option.
    fn options(&self, category: &str, row: &OperationRow) -> Vec<OptionSnapshot> {
        let mut options: Vec<OptionSnapshot> = if is_understand(category, row.operation) {
            self.selectable
                .iter()
                .filter(|model| model.capabilities.vision)
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
    fn unavailable_reason(
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
    fn operation(&self, category: &str, row: &OperationRow) -> OperationSnapshot {
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
                true if !served => (
                    false,
                    vec![reason(
                        "configuration",
                        "No selectable model is served by an available adapter",
                    )],
                ),
                true => (true, vec![]),
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
    let selectable_models = selectable
        .0
        .selectable(cancel.clone())
        .await
        .map_err(host_error)?;
    let declared = selectable
        .0
        .selectable_for_modality(cancel.clone())
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
            selectable: selectable_models,
            declared,
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
                                    &settings,
                                    &selectable,
                                    &selection,
                                    &context,
                                    &stopping,
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
                                    &stopping,
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
/// source and the `modality.*` management namespace. The host must register
/// the names of the services it reads (settings, managementMethods and
/// Provider's selection and selectable-model reads), as the App Server's
/// assembly does; when one of them has no provider, the host still starts
/// and opens Sessions, and only `modality.*` is unavailable.
pub fn install(builder: &mut AgentBuilder) {
    dim_agent_runtime::names::register::<ModalityAdapterService>(builder.catalog_mut(), None);
    builder
        .optional_plugin(|names| Ok(Box::new(ModalityPlugin::new(names)?) as Box<dyn DynPlugin>));
}
