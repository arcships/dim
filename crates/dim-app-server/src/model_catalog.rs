//! Pure model descriptor projection owned by the provider service.

mod id;
pub(crate) mod name_inputs;
mod tokens;

use self::id::{Lookup, variants};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub use dim_agent_types::{
    CatalogCapabilities, ModelCapabilities, ModelReasoning, ModelServiceTier, ReasoningBudget,
    ReasoningOff,
};

pub(crate) mod source;
use source::CatalogData;

include!(concat!(env!("OUT_DIR"), "/catalog.rs"));

fn bundled_model(bytes: &[u8]) -> ModelEntry {
    #[cfg(feature = "testing")]
    crate::provider::read_work::update(|work| work.source_payloads += 1);
    bundled_record(bytes)
}

fn bundled_record<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
    use std::io::Read as _;
    let mut decoded = Vec::with_capacity(bytes.len().saturating_mul(3));
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut decoded)
        .expect("checked-in bundled catalog records decompress");
    serde_json::from_slice(&decoded).expect("checked-in bundled catalog records deserialize")
}
const REASONING_OFF: &str = include_str!("../resources/reasoning-off-overrides.json");

/// Limits cross the JSON boundary into JavaScript clients without rounding.
pub const MAX_MODEL_TOKENS: u64 = dim_agent_types::JS_MAX_SAFE_INTEGER;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("invalid bundled model catalog: {0}")]
    Catalog(#[from] serde_json::Error),
    #[error("model descriptor requires a non-empty id")]
    MissingModelId,
    #[error("model descriptor must be an object")]
    InvalidDescriptor,
    #[error("model limits must be positive safe JSON integers")]
    InvalidLimits,
}

/// The kind of provider record a projection stands for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelSource {
    Builtin,
    Custom,
    Oauth,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub model_id: String,
    pub display_name: String,
    pub status: ModelStatus,
    pub capabilities: CatalogCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_guide_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ModelMetadata>,
}

impl ModelEntry {
    pub(crate) fn codex_context_window_ceiling(&self) -> Option<u64> {
        self.metadata
            .as_ref()?
            .raw
            .get("max_context_window")?
            .as_u64()
    }
}

/// The editable surface excludes model identity, lookup diagnostics and raw
/// provider fields. Resetting an override is a separate provider operation.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelEdit {
    pub display_name: Option<String>,
    pub context_window: Option<std::num::NonZeroU64>,
    pub max_output_tokens: Option<std::num::NonZeroU64>,
    pub tool_calling: Option<bool>,
    pub vision: Option<bool>,
    pub reasoning: Option<ReasoningEdit>,
    pub modality_capabilities: Option<Vec<String>>,
    #[serde(default, deserialize_with = "crate::protocol::optional_non_null")]
    pub endpoint: Option<FieldUpdate<NewApiEndpoint>>,
    /// The catalog model this entry's id stands for. The record holds it
    /// beside the provider's other statements about its ids, not as a
    /// descriptor override.
    #[serde(default, deserialize_with = "crate::protocol::optional_non_null")]
    pub alias: Option<FieldUpdate<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum FieldUpdate<T> {
    Set { value: T },
    Clear,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasoningEdit {
    pub supported: bool,
    pub effort_options: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    #[default]
    Active,
    Beta,
    Deprecated,
    Unavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ModelServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_match_reason: Option<String>,
    /// How this entry was identified, and what supplied a limit it does not
    /// declare. Absent when nothing named the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identification: Option<ModelIdentification>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_override_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub raw: Map<String, Value>,
}

/// The one model an id named, and the numbers something else supplied.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelIdentification {
    /// The step that named the row, as `Rule`'s own word.
    pub rule: String,
    /// Step 3's display name settled several canonical hits.
    pub narrowed_by_display_name: bool,
    /// The rows a step still disagreed on, when it reported an ambiguity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<CandidateId>,
    /// The limits this entry did not declare and something else supplied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filled: Vec<FilledLimit>,
}

/// One row an ambiguity named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateId {
    pub provider_id: String,
    pub model_id: String,
}

/// A limit an entry did not declare, the entry that supplied it and how that
/// entry was identified, so a filled value never reads as a declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilledLimit {
    /// `contextWindow` or `maxOutputTokens`.
    pub field: String,
    pub value: u64,
    pub provider_id: String,
    pub model_id: String,
    /// How the supplying entry was identified.
    pub rule: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub input_token_price: Option<f64>,
    pub output_token_price: Option<f64>,
    pub currency: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub requests_per_minute: Option<u64>,
    pub tokens_per_minute: Option<u64>,
}
#[derive(Clone, Debug, Default)]
pub struct ProviderProjection {
    pub provider_id: String,
    /// The kind of provider this projection reads for, not where any model in
    /// `models` came from. Only `Custom` changes what the matcher reads: a
    /// custom provider declares nothing, so step 1 never answers for it, while
    /// any other provider declares its bundled rows and the models the user
    /// added.
    pub source: Option<ModelSource>,
    pub models: Vec<ModelEntry>,
    /// The models are the owner's admitted defaults and user rows, including a pinned old base.
    pub admitted_defaults: bool,
    pub context_window_override: Option<u64>,
    /// The prefix this provider's listed ids carry, stripped once before an id
    /// is broken into the tokens the steps compare.
    pub glued_prefix: Option<String>,
    /// The provider's own statement that a listed id stands for another one,
    /// keyed by the exact id the provider spells.
    pub model_aliases: std::collections::BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextWindowFallback {
    window: u64,
    #[serde(default, deserialize_with = "compile_patterns")]
    model_id_patterns: Vec<Regex>,
}

fn compile_patterns<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Regex>, D::Error> {
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|pattern| Regex::new(&pattern).map_err(serde::de::Error::custom))
        .collect()
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogMetadata {
    context_window_override: Option<u64>,
    context_window_fallback: Option<ContextWindowFallback>,
    pub(crate) remote_model_list: Option<bool>,
    #[serde(default)]
    reasoning_off_overrides: bool,
    role: Option<ProviderRole>,
    #[serde(default)]
    plan: bool,
    /// The canonical namespace this provider's own models name, stated by its
    /// sync policy so the rows of a vendor provider can be told apart.
    vendor_namespace: Option<String>,
    pub(crate) raw: Option<CatalogRawMetadata>,
}

/// Whether a provider serves its own vendor's models or resells other
/// vendors'. A provider whose model list comes from the user or the endpoint
/// has no bundled models and declares none.
///
/// The role is a provider-level fact, and a row is this vendor's declaration
/// only when its canonical id names the provider's own namespace or the row
/// states none. A row of a vendor provider whose canonical id names another
/// vendor is a resale, and the matcher reads it only where it reads an
/// aggregator row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderRole {
    Vendor,
    Aggregator,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogRawMetadata {
    #[serde(default)]
    pub(crate) endpoint_type_by_model: std::collections::BTreeMap<String, NewApiEndpoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NewApiEndpoint {
    Openai,
    OpenaiResponse,
    OpenaiResponseCompact,
    OpenaiAlphaSearch,
    Anthropic,
    Gemini,
    JinaRerank,
    ImageGeneration,
    Embeddings,
    #[serde(alias = "video-generation")]
    OpenaiVideo,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogFile {
    providers: Vec<CatalogProvider>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvider {
    pub provider_id: String,
    pub display_name: String,
    pub driver_kind: String,
    pub default_base_url: Option<String>,
    pub auth: CatalogAuth,
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub(crate) metadata: Option<CatalogMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogAuth {
    #[serde(rename = "type")]
    pub kind: String,
    pub header_name: Option<String>,
    #[serde(default)]
    pub prefix: String,
}

#[derive(Deserialize)]
struct OffFile {
    providers: HashMap<String, OffProvider>,
}
#[derive(Deserialize)]
struct OffProvider {
    models: std::collections::BTreeMap<String, OffModel>,
}
#[derive(Deserialize)]
struct OffModel {
    reasoning: OffReasoning,
}
#[derive(Deserialize)]
struct OffReasoning {
    off: Option<ReasoningOff>,
}

/// Shipped records are decoded on demand; supplied catalogs own their input.
pub struct BundledCatalog {
    data: Option<&'static CatalogData>,
    providers: Vec<CatalogProvider>,
    by_provider: HashMap<String, usize>,
    ids: id::IdIndex,
}

impl BundledCatalog {
    /// The catalog the bundled files describe, as every projection reads it.
    pub fn load() -> Result<Self, ProjectionError> {
        Ok(Self {
            data: Some(&DATA),
            providers: Vec::new(),
            by_provider: HashMap::new(),
            ids: id::IdIndex::new(&[]),
        })
    }

    /// The same catalog `load` builds, from a parsed catalog file. A caller
    /// that states its own rows gets exactly the projection the shipped file
    /// gets: a row keeps only the limits its `metadata.raw` declares (#522),
    /// and the reasoning-off table is read from the bundled overrides.
    ///
    /// The input is the parsed file rather than `CatalogFile` because that
    /// type is an implementation detail of this module; a `Value` is what the
    /// file parses to, and what a caller outside the module can state.
    pub fn from_value(value: Value) -> Result<Self, ProjectionError> {
        let mut file: CatalogFile = serde_json::from_value(value)?;
        // #522: a bundled entry keeps only the limits its metadata.raw declares.
        for provider in &mut file.providers {
            for model in &mut provider.models {
                drop_undeclared_limits(model);
            }
        }
        let by_provider = file
            .providers
            .iter()
            .enumerate()
            .map(|(i, p)| (p.provider_id.clone(), i))
            .collect();
        Ok(Self {
            data: None,
            ids: id::IdIndex::new(&file.providers),
            providers: file.providers,
            by_provider,
        })
    }

    fn reasoning_overrides() -> Result<HashMap<(String, String), ReasoningOff>, ProjectionError> {
        let off_file: OffFile = serde_json::from_str(REASONING_OFF)?;
        let mut result = HashMap::new();
        for (provider, overrides) in off_file.providers {
            for (model, entry) in overrides.models {
                if let Some(off) = entry.reasoning.off {
                    for variant in variants(&model) {
                        result.insert((provider.clone(), variant), off.clone());
                    }
                }
            }
        }
        Ok(result)
    }

    pub(crate) fn fingerprint(&self, id: &str) -> Option<&'static str> {
        self.data?.provider_named(id).map(|p| p.fingerprint)
    }

    pub(crate) fn image(&self) -> &'static [u8] {
        DATA.bytes
    }

    pub(crate) fn source(&self) -> &CatalogData {
        &DATA
    }

    pub(crate) fn revision(&self) -> &'static str {
        REVISION
    }

    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.data
            .into_iter()
            .flat_map(|data| data.providers().map(|p| p.id))
            .chain(self.providers.iter().map(|p| p.provider_id.as_str()))
    }

    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<ModelEntry> {
        if let Some(data) = self.data {
            let row = data.model(
                data.matching(&format!("t\0{provider_id}\0{model_id}"))
                    .next()?,
            );
            let mut entry: ModelEntry = bundled_model(row.bytes);
            drop_undeclared_limits(&mut entry);
            Some(entry)
        } else {
            self.providers[*self.by_provider.get(provider_id)?]
                .models
                .iter()
                .find(|m| m.model_id == model_id)
                .cloned()
        }
    }

    pub fn bundled_models(&self, provider_id: &str) -> Option<Vec<ModelEntry>> {
        if let Some(data) = self.data {
            let provider = data.provider_named(provider_id)?;
            Some(
                (provider.first..provider.end)
                    .map(|i| data.model(i))
                    .map(|row| {
                        let mut entry: ModelEntry = bundled_model(row.bytes);
                        drop_undeclared_limits(&mut entry);
                        entry
                    })
                    .collect(),
            )
        } else {
            Some(
                self.providers[*self.by_provider.get(provider_id)?]
                    .models
                    .clone(),
            )
        }
    }

    pub fn providers(&self) -> impl Iterator<Item = CatalogProvider> + '_ {
        self.provider_ids()
            .map(|id| self.provider(id).expect("a catalog provider id"))
    }

    pub fn provider(&self, provider_id: &str) -> Option<CatalogProvider> {
        let mut provider = self.provider_header(provider_id)?;
        provider.models = self.bundled_models(provider_id)?;
        Some(provider)
    }

    /// Provider settings without model descriptors.
    pub(crate) fn provider_header(&self, provider_id: &str) -> Option<CatalogProvider> {
        if let Some(data) = self.data {
            Some(bundled_record(data.provider_named(provider_id)?.bytes))
        } else {
            let mut provider = self.providers[*self.by_provider.get(provider_id)?].clone();
            provider.models.clear();
            Some(provider)
        }
    }

    /// Whether the bundled declarations for `provider_id` belong to that
    /// provider's own vendor or to a reseller's mixture, as the catalog states
    /// them. `None` when the provider is not bundled or lists no models.
    pub fn role(&self, provider_id: &str) -> Option<ProviderRole> {
        self.provider_header(provider_id)?.metadata.as_ref()?.role
    }

    /// Whether the bundled declarations for `provider_id` belong to a
    /// subscription or coding plan rather than the vendor's API entry.
    pub fn is_plan(&self, provider_id: &str) -> Option<bool> {
        Some(self.provider_header(provider_id)?.metadata.as_ref()?.plan)
    }

    /// The canonical namespace `provider_id`'s own models name, as the
    /// provider's sync policy stated it. `None` when the provider is not
    /// bundled or states none.
    pub fn vendor_namespace(&self, provider_id: &str) -> Option<String> {
        self.provider_header(provider_id)?
            .metadata?
            .vendor_namespace
    }

    pub fn project(
        &self,
        provider: &ProviderProjection,
        descriptor: &Value,
    ) -> Result<ModelEntry, ProjectionError> {
        let value = descriptor;
        let original = value
            .as_object()
            .ok_or(ProjectionError::InvalidDescriptor)?;
        let id = string(original.get("id"))
            .ok_or(ProjectionError::MissingModelId)?
            .to_owned();
        let input = value
            .as_object()
            .ok_or(ProjectionError::InvalidDescriptor)?;
        let mut entry = descriptor_entry(&provider.provider_id, &id, input)?;
        let explicit = entry
            .metadata
            .as_ref()
            .map(|m| m.capability_override_fields.clone())
            .unwrap_or_default();
        let kind = entry_kind(&entry);
        let identified = self.identify(provider, &id, kind.as_deref());
        let identification = Self::identification_record(&identified, &explicit);
        if let Ok(Some(found_lookup)) = identified {
            let pid = found_lookup.provider_id.clone();
            let found = found_lookup.entry;
            let strategy = found_lookup.strategy;
            let score = found_lookup.score;
            entry = merge_entry(entry, &found, &explicit);
            let declared = input.get("capabilities").and_then(Value::as_object);
            if input.get("modalities").is_none()
                && input.get("vision") != Some(&Value::Bool(true))
                && declared.is_none_or(|value| !value.contains_key("modalities"))
            {
                entry.capabilities.modalities = found.capabilities.modalities.clone();
            }
            macro_rules! inherit_capability {
                ($json:literal, $field:ident) => {
                    if input.get($json).is_none()
                        && declared.is_none_or(|value| !value.contains_key($json))
                    {
                        entry.capabilities.$field = found.capabilities.$field;
                    }
                };
            }
            inherit_capability!("toolCalling", tool_calling);
            inherit_capability!("streaming", streaming);
            inherit_capability!("structuredOutput", structured_output);
            inherit_capability!("vision", vision);
            inherit_capability!("audio", audio);
            let metadata = entry.metadata.get_or_insert_default();
            metadata.metadata_source = Some(pid.clone());
            metadata.matched_provider_id = Some(pid);
            metadata.matched_model_id = Some(found.model_id.clone());
            metadata.match_strategy = Some(strategy);
            metadata.match_score = Some(score);
        } else {
            let metadata = entry.metadata.get_or_insert_default();
            metadata.metadata_source = Some("provider-default".into());
            metadata.match_strategy = Some("provider-default".into());
            metadata.metadata_match_reason = Some("no_unique_match".into());
        }
        entry.metadata.get_or_insert_default().identification = identification;
        self.apply_provider_rules(provider, &mut entry);
        if !limits_valid(&entry.capabilities) {
            return Err(ProjectionError::InvalidLimits);
        }
        Ok(entry)
    }

    /// How an id was identified, as the entry carries it. `None` when no step
    /// named a row; the candidates when a step refused the id. `declared` names
    /// the limits the entry states itself: a merge adopts one minimum per limit
    /// from a joining row, and a limit the entry declares is not one it
    /// adopted, even when the two values are equal.
    fn identification_record(
        identified: &Result<Option<Lookup>, Vec<id::Candidate>>,
        declared: &[String],
    ) -> Option<ModelIdentification> {
        match identified {
            Ok(Some(found)) => Some(ModelIdentification {
                rule: found.rule.into(),
                narrowed_by_display_name: found.narrowed,
                candidates: Vec::new(),
                filled: adopted(found.suppliers.clone(), declared),
            }),
            Err(candidates) => Some(ModelIdentification {
                rule: "ambiguous".into(),
                narrowed_by_display_name: false,
                candidates: candidates
                    .iter()
                    .map(|candidate| CandidateId {
                        provider_id: candidate.provider_id.clone(),
                        model_id: candidate.model_id.clone(),
                    })
                    .collect(),
                filled: Vec::new(),
            }),
            Ok(None) => None,
        }
    }

    /// The limits `found` supplies `model` and the row that supplied each, so
    /// a value the entry never stated never reads as one. `needs` selects the
    /// limits the entry does not state, which are the only ones written and
    /// listed.
    fn fill_from(found: &Lookup, model: &mut ModelEntry, needs: [bool; 2]) -> Vec<FilledLimit> {
        let mut filled = Vec::new();
        for (field, needs, value, own) in [
            (
                "contextWindow",
                needs[0],
                found.entry.capabilities.context_window,
                &mut model.capabilities.context_window,
            ),
            (
                "maxOutputTokens",
                needs[1],
                found.entry.capabilities.max_output_tokens,
                &mut model.capabilities.max_output_tokens,
            ),
        ] {
            let Some(value) = value.filter(|_| needs) else {
                continue;
            };
            *own = Some(value);
            // A merge adopted the value from one of the joining rows, and that
            // row is the supplier; a step that named one row is its own.
            let supplier = found
                .suppliers
                .iter()
                .find(|limit| limit.field == field && limit.value == value);
            filled.push(match supplier {
                Some(limit) => limit.clone(),
                None => FilledLimit {
                    field: field.into(),
                    value,
                    provider_id: found.provider_id.clone(),
                    model_id: found.entry.model_id.clone(),
                    rule: found.rule.to_owned(),
                },
            });
        }
        filled
    }

    /// Records the limits something else supplied in `model`'s identification,
    /// so a value the entry never stated never reads as one.
    fn record_filled(model: &mut ModelEntry, filled: Vec<FilledLimit>) {
        if filled.is_empty() {
            return;
        }
        let metadata = model.metadata.get_or_insert_default();
        match &mut metadata.identification {
            Some(identification) => identification.filled = filled,
            None => {
                metadata.identification = Some(ModelIdentification {
                    rule: "provider-exact".into(),
                    narrowed_by_display_name: false,
                    candidates: Vec::new(),
                    filled,
                })
            }
        }
    }

    /// A limit the row does not declare, supplied by the entry the id
    /// identifies and recorded in `identification.filled` so it never reads as
    /// the row's own declaration.
    fn fill_undeclared(&self, model: &mut ModelEntry, provider_id: &str) {
        // The row's own provider writes a default pair rather than declaring a
        // limit, so such a row counts as undeclared here.
        let placeholder = id::is_placeholder(provider_id, model);
        let needs = [
            model.capabilities.context_window.is_none() || placeholder,
            model.capabilities.max_output_tokens.is_none() || placeholder,
        ];
        if !needs[0] && !needs[1] {
            return;
        }
        // The identified entry supplies the pair. The row itself answers step 1
        // on its own provider, so the entry is looked up outside that list.
        let kind = entry_kind(model);
        let Some(found) = self.lookup(
            &ProviderProjection::default(),
            &model.model_id,
            kind.as_deref(),
        ) else {
            return;
        };
        let filled = Self::fill_from(&found, model, needs);
        Self::record_filled(model, filled);
    }

    /// What a listing states about one of its own ids, read with the
    /// projection the record serves: the identification the id earns, the row
    /// it named, and each limit the entry does not state taken from that row.
    ///
    /// A limit the entry marks in `capabilityOverrideFields` is its own
    /// statement, so neither the listing's own fill nor an override's value is
    /// rewritten; the limits a listing fills by itself are unmarked and become
    /// the identified row's.
    ///
    /// The entry's descriptor stays what its author wrote — its name,
    /// modalities, reasoning, pricing and `metadataSource` — so this writes
    /// only the facts identification owns. The identification is set on every
    /// read, so a statement that no longer names anything leaves the entry
    /// unnamed instead of keeping what an earlier read found.
    pub(crate) fn identify_entry(&self, provider: &ProviderProjection, entry: &mut ModelEntry) {
        let marked = entry
            .metadata
            .as_ref()
            .map(|metadata| metadata.capability_override_fields.clone())
            .unwrap_or_default();
        let kind = entry_kind(entry);
        let identified = self.identify(provider, &entry.model_id, kind.as_deref());
        entry.metadata.get_or_insert_default().identification =
            Self::identification_record(&identified, &marked);
        let needs = [
            !marked.iter().any(|name| name == "contextWindow"),
            !marked.iter().any(|name| name == "maxOutputTokens"),
        ];
        let filled = match identified {
            Ok(Some(found)) => {
                let metadata = entry.metadata.get_or_insert_default();
                metadata.matched_provider_id = Some(found.provider_id.clone());
                metadata.matched_model_id = Some(found.entry.model_id.clone());
                metadata.match_strategy = Some(found.strategy.clone());
                metadata.match_score = Some(found.score);
                Self::fill_from(&found, entry, needs)
            }
            _ => {
                let metadata = entry.metadata.get_or_insert_default();
                metadata.matched_provider_id = None;
                metadata.matched_model_id = None;
                metadata.match_strategy = None;
                metadata.match_score = None;
                Vec::new()
            }
        };
        Self::record_filled(entry, filled);
    }

    pub fn effective_models(
        &self,
        provider: &ProviderProjection,
        defaults: &[ModelEntry],
        overrides: &[ModelEntry],
    ) -> Vec<ModelEntry> {
        let mut models = defaults
            .iter()
            .map(|model| {
                let mut model = model.clone();
                let kind = entry_kind(&model);
                let identified = self.identify(provider, &model.model_id, kind.as_deref());
                let identification = Self::identification_record(
                    &identified,
                    model
                        .metadata
                        .as_ref()
                        .map(|metadata| metadata.capability_override_fields.as_slice())
                        .unwrap_or_default(),
                );
                if let Ok(Some(found)) = identified {
                    let provider_id = found.provider_id.clone();
                    let strategy = found.strategy.clone();
                    let score = found.score;
                    let explicit = model
                        .metadata
                        .as_ref()
                        .map(|m| m.capability_override_fields.clone())
                        .unwrap_or_default();
                    model = merge_entry(model, &found.entry, &explicit);
                    let metadata = model.metadata.get_or_insert_default();
                    metadata.metadata_source = Some(provider_id.clone());
                    metadata.matched_provider_id = Some(provider_id.clone());
                    metadata.matched_model_id = Some(found.entry.model_id.clone());
                    metadata.match_strategy = Some(strategy);
                    metadata.match_score = Some(score);
                    metadata.metadata_match_reason = None;
                    model.metadata.get_or_insert_default().identification = identification;
                    self.fill_undeclared(&mut model, &provider_id);
                } else {
                    // Nothing named the model, so its descriptor is the only
                    // statement of what it supports, and the two fields that
                    // carry reasoning must not disagree.
                    if let Some(supported) = model
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.reasoning.as_ref())
                        .map(|reasoning| reasoning.supported)
                    {
                        model.capabilities.reasoning = supported;
                    }
                    model.metadata.get_or_insert_default().identification = identification;
                }
                self.apply_provider_rules(provider, &mut model);
                model
            })
            .collect::<Vec<_>>();
        for item in overrides {
            if let Some(pos) = models.iter().position(|m| m.model_id == item.model_id) {
                let base = &models[pos];
                let mut model = inherit_limits(item.clone(), base);
                if provider.provider_id == "openai-codex"
                    && let Some(maximum) = base.codex_context_window_ceiling()
                {
                    model
                        .metadata
                        .get_or_insert_default()
                        .raw
                        .insert("max_context_window".into(), maximum.into());
                    if let Some(window) = &mut model.capabilities.context_window {
                        *window = (*window).min(maximum);
                    }
                }
                models[pos] = model;
            } else {
                models.push(item.clone())
            }
        }
        if let Some(window) = provider.context_window_override {
            for model in &mut models {
                model.capabilities.context_window = Some(window);
            }
        }
        models
    }

    /// Applies an editor patch to visible fields while retaining all hidden metadata.
    pub fn merge_visible_edit(&self, existing: &ModelEntry, edit: ModelEdit) -> ModelEntry {
        let mut model = existing.clone();
        if let Some(name) = edit.display_name {
            model.display_name = name;
        }
        if let Some(tool_calling) = edit.tool_calling {
            model.capabilities.tool_calling = tool_calling;
        }
        if let Some(vision) = edit.vision {
            model.capabilities.vision = vision;
        }
        for (field, value, target) in [
            (
                "contextWindow",
                edit.context_window,
                &mut model.capabilities.context_window,
            ),
            (
                "maxOutputTokens",
                edit.max_output_tokens,
                &mut model.capabilities.max_output_tokens,
            ),
        ] {
            let metadata = model.metadata.get_or_insert_default();
            let marked = metadata
                .capability_override_fields
                .iter()
                .any(|name| name == field);
            match value {
                Some(value) => {
                    *target = Some(value.get());
                    if !marked {
                        metadata.capability_override_fields.push(field.into());
                    }
                    // The edit declares this limit, so a record of an entry
                    // that supplied it is stale and must not survive the edit.
                    if let Some(identification) = metadata.identification.as_mut() {
                        identification.filled.retain(|limit| limit.field != field);
                    }
                }
                // A limit this entry does not declare is dropped, so an
                // override never captures a value inherited from a bundle.
                None if !marked => *target = None,
                None => {}
            }
        }
        if let Some(reasoning) = edit.reasoning {
            model.capabilities.reasoning = reasoning.supported;
            let current = model
                .metadata
                .get_or_insert_default()
                .reasoning
                .get_or_insert_default();
            current.supported = reasoning.supported;
            current.effort_options = if reasoning.supported {
                reasoning.effort_options
            } else {
                None
            };
        }
        if let Some(capabilities) = edit.modality_capabilities {
            model.metadata.get_or_insert_default().raw.insert(
                "imageGeneration".into(),
                Value::Bool(capabilities.iter().any(|id| id == "image.generate")),
            );
            model.metadata.get_or_insert_default().raw.insert(
                "modalityCapabilities".into(),
                Value::Array(capabilities.into_iter().map(Value::String).collect()),
            );
        }
        model
    }

    /// Bundled metadata for a model served by a relay outside the catalog,
    /// read with the projection of the record that serves it. `kind` is the
    /// kind the listed id states, as `raw_kind` reads one, so a media id takes
    /// no chat row and a chat id takes no media row.
    pub fn fallback(
        &self,
        provider: &ProviderProjection,
        id: &str,
        kind: Option<&str>,
    ) -> Option<ModelEntry> {
        self.lookup(provider, id, kind).map(|found| found.entry)
    }

    fn apply_provider_rules(&self, provider: &ProviderProjection, entry: &mut ModelEntry) {
        let bundled = self.provider_header(&provider.provider_id);
        let metadata = bundled.as_ref().and_then(|p| p.metadata.as_ref());
        let override_window = provider
            .context_window_override
            .or_else(|| metadata.and_then(|m| m.context_window_override));
        if let Some(window) = override_window {
            entry.capabilities.context_window = Some(window);
        }
        let fallback = metadata.and_then(|m| m.context_window_fallback.as_ref());
        if let Some(fallback) = fallback
            && override_window.is_none()
            && fallback
                .model_id_patterns
                .iter()
                .any(|r| variants(&entry.model_id).iter().any(|id| r.is_match(id)))
        {
            entry.capabilities.context_window = Some(fallback.window);
        }
        let enabled = metadata.is_some_and(|m| m.reasoning_off_overrides);
        if enabled
            && entry
                .metadata
                .as_ref()
                .and_then(|m| m.reasoning.as_ref())
                .is_some_and(|r| r.supported && r.off.is_none())
            && let Some(off) = {
                let overrides =
                    Self::reasoning_overrides().expect("validated bundled reasoning overrides");
                variants(&entry.model_id).iter().find_map(|id| {
                    overrides
                        .get(&(provider.provider_id.clone(), id.clone()))
                        .cloned()
                })
            }
        {
            entry
                .metadata
                .as_mut()
                .unwrap()
                .reasoning
                .as_mut()
                .unwrap()
                .off = Some(off.clone());
        }
    }
}

/// The merged limits the projection actually adopted: a merge adopts one
/// minimum per limit from a joining row, and `declared` names the limits the
/// entry states itself. A declared limit is the entry's own number, so it has
/// no `filled` record even when the adopted minimum equals it.
fn adopted(suppliers: Vec<FilledLimit>, declared: &[String]) -> Vec<FilledLimit> {
    suppliers
        .into_iter()
        .filter(|limit| !declared.contains(&limit.field))
        .collect()
}

fn descriptor_entry(
    provider_id: &str,
    id: &str,
    input: &Map<String, Value>,
) -> Result<ModelEntry, ProjectionError> {
    let capabilities = input.get("capabilities").and_then(Value::as_object);
    let (context, output) = declared_limits(input);
    // A descriptor states its reasoning at the top level, and a stored entry
    // sent back carries the same object under `metadata`.
    let reasoning_value = input.get("reasoning").or_else(|| {
        input
            .get("metadata")
            .and_then(|metadata| metadata.get("reasoning"))
    });
    let reasoning = project_reasoning(reasoning_value, declared_efforts(input))?;
    let mut caps = CatalogCapabilities::default();
    if let Some(v) = capabilities {
        apply_capabilities(&mut caps, v);
    }
    if let Some(values) = input.get("modalities").and_then(Value::as_array) {
        caps.modalities = strings(values);
    }
    if let Some(v) = input.get("toolCalling").and_then(Value::as_bool) {
        caps.tool_calling = v;
    }
    if let Some(v) = input.get("vision").and_then(Value::as_bool) {
        caps.vision = v;
        if v && input.get("modalities").is_none()
            && capabilities.is_none_or(|c| !c.contains_key("modalities"))
        {
            caps.modalities = vec!["text".into(), "image".into()];
        }
    }
    if let Some(r) = &reasoning {
        caps.reasoning = r.supported;
    }
    if let Some(v) = context {
        caps.context_window = Some(v);
    }
    if let Some(v) = output {
        caps.max_output_tokens = Some(v);
    }
    let mut raw = input
        .get("raw")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    raw.insert("id".into(), Value::String(id.into()));
    raw.insert("providerId".into(), Value::String(provider_id.into()));
    for field in ["name", "imageGeneration", "modalityCapabilities"] {
        if let Some(value) = input.get(field) {
            raw.insert(field.into(), value.clone());
        }
    }
    if let Some(capabilities) = input.get("modalityCapabilities").and_then(Value::as_array) {
        raw.insert(
            "imageGeneration".into(),
            Value::Bool(
                capabilities
                    .iter()
                    .any(|id| id.as_str() == Some("image.generate")),
            ),
        );
    }
    if input.get("imageGeneration") == Some(&Value::Bool(true))
        && !input.contains_key("modalityCapabilities")
    {
        raw.insert("type".into(), Value::String("image-generation".into()));
    }
    if let Some(field) = reasoning_value.and_then(|r| r.get("interleavedField")) {
        let interleaved = raw
            .entry("interleaved")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(interleaved) = interleaved.as_object_mut() {
            interleaved.insert("field".into(), field.clone());
        }
    }
    let pricing = pricing(&raw);
    let service_tier = service_tier(input.get("serviceTier"), &raw);
    let mut metadata = ModelMetadata {
        pricing,
        reasoning,
        service_tier,
        raw,
        ..Default::default()
    };
    if context.is_some() {
        metadata
            .capability_override_fields
            .push("contextWindow".into());
    }
    if output.is_some() {
        metadata
            .capability_override_fields
            .push("maxOutputTokens".into());
    }
    Ok(ModelEntry {
        model_id: id.into(),
        display_name: string(input.get("displayName"))
            .or_else(|| string(input.get("display_name")))
            .or_else(|| string(input.get("name")))
            .unwrap_or(id)
            .into(),
        status: input
            .get("status")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        capabilities: caps,
        model_guide_prompt: None,
        metadata: Some(metadata),
    })
}

fn merge_entry(mut current: ModelEntry, fallback: &ModelEntry, explicit: &[String]) -> ModelEntry {
    if current.display_name == current.model_id {
        current.display_name = fallback.display_name.clone();
    }
    if !explicit.iter().any(|x| x == "contextWindow") {
        current.capabilities.context_window = fallback.capabilities.context_window;
    }
    if !explicit.iter().any(|x| x == "maxOutputTokens") {
        current.capabilities.max_output_tokens = fallback.capabilities.max_output_tokens;
    }
    let cm = current.metadata.get_or_insert_default();
    if let Some(fm) = &fallback.metadata {
        if cm.description.is_none() {
            cm.description = fm.description.clone();
        }
        if cm.release_date.is_none() {
            cm.release_date = fm.release_date.clone();
        }
        if let Some(pricing) = &fm.pricing {
            let current = cm.pricing.get_or_insert_default();
            current.input_token_price = current.input_token_price.or(pricing.input_token_price);
            current.output_token_price = current.output_token_price.or(pricing.output_token_price);
            if current.currency.is_none() {
                current.currency = pricing.currency.clone();
            }
        }
        if let Some(rate_limit) = &fm.rate_limit {
            let current = cm.rate_limit.get_or_insert_default();
            current.requests_per_minute = current
                .requests_per_minute
                .or(rate_limit.requests_per_minute);
            current.tokens_per_minute = current.tokens_per_minute.or(rate_limit.tokens_per_minute);
        }
        cm.reasoning = match (&cm.reasoning, &fm.reasoning) {
            (Some(r), _) if !r.supported => Some(r.clone()),
            (Some(r), Some(f)) => {
                let mut n = r.clone();
                inherit_reasoning(&mut n, f);
                Some(n)
            }
            (None, f) => f.clone(),
            (r, _) => r.clone(),
        };
        match (&mut cm.service_tier, &fm.service_tier) {
            (Some(tier), Some(fallback)) if tier.supported => {
                if tier.default.is_none() {
                    tier.default = fallback.default.clone();
                }
                if tier.options.is_empty() {
                    tier.options = fallback.options.clone();
                }
            }
            (None, fallback) => cm.service_tier = fallback.clone(),
            _ => {}
        }
        for (k, v) in &fm.raw {
            cm.raw.entry(k.clone()).or_insert(v.clone());
        }
    }
    if let Some(reasoning) = &cm.reasoning {
        current.capabilities.reasoning = reasoning.supported;
    }
    if current.model_guide_prompt.is_none() {
        current.model_guide_prompt = fallback.model_guide_prompt.clone();
    }
    if let Some(state) = &fallback.capabilities.provider_state {
        let current = current.capabilities.provider_state.get_or_insert_default();
        for (key, value) in state {
            current.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    current
}

/// One declared limit: a positive safe JSON integer.
pub fn limit_valid(limit: u64) -> bool {
    (1..=MAX_MODEL_TOKENS).contains(&limit)
}

/// Every limit that is present is a positive safe JSON integer; an absent one
/// is the undeclared value.
pub fn limits_valid(capabilities: &CatalogCapabilities) -> bool {
    [capabilities.context_window, capabilities.max_output_tokens]
        .into_iter()
        .flatten()
        .all(limit_valid)
}

/// The values that declare a model's two limits, in the order the projection
/// reads them: the flat descriptor keys, the nested `limit`, `capabilities`,
/// and the nested `top_provider` some listings use. The remote listing and the
/// stored record's `raw` are read with this one function, so the projection
/// and the upgrade agree on what a declaration is.
///
/// Besides the descriptor keys, the flat keys read are OpenRouter's
/// `context_length` and `top_provider.max_completion_tokens`, vLLM's
/// `max_model_len`, and LM Studio's `max_context_length`.
pub(crate) fn declared_limit_values(input: &Map<String, Value>) -> (Vec<&Value>, Vec<&Value>) {
    fn values<'a>(
        input: &'a Map<String, Value>,
        keys: &[&str],
        limit: &str,
        capability: &str,
        nested: Option<(&str, &str)>,
    ) -> Vec<&'a Value> {
        keys.iter()
            .filter_map(|key| input.get(*key))
            .chain(input.get("limit").and_then(|value| value.get(limit)))
            .chain(
                input
                    .get("capabilities")
                    .and_then(|value| value.get(capability)),
            )
            .chain(
                nested
                    .and_then(|(outer, inner)| input.get(outer).and_then(|value| value.get(inner))),
            )
            .collect()
    }
    (
        values(
            input,
            &[
                "contextWindow",
                "context_window",
                "inputTokenLimit",
                "context_length",
                "max_model_len",
                "max_context_length",
            ],
            "context",
            "contextWindow",
            None,
        ),
        values(
            input,
            &["maxOutputTokens", "max_output_tokens", "outputTokenLimit"],
            "output",
            "maxOutputTokens",
            Some(("top_provider", "max_completion_tokens")),
        ),
    )
}

/// The first valid value among each limit's declaration sources.
pub(crate) fn declared_limits(input: &Map<String, Value>) -> (Option<u64>, Option<u64>) {
    let (context, output) = declared_limit_values(input);
    (
        context.into_iter().find_map(|value| positive(Some(value))),
        output.into_iter().find_map(|value| positive(Some(value))),
    )
}

/// A limit the entry does not declare is inherited from the entry it replaces.
pub fn inherit_limits(mut entry: ModelEntry, base: &ModelEntry) -> ModelEntry {
    if entry.capabilities.context_window.is_none() {
        entry.capabilities.context_window = base.capabilities.context_window;
    }
    if entry.capabilities.max_output_tokens.is_none() {
        entry.capabilities.max_output_tokens = base.capabilities.max_output_tokens;
    }
    entry
}

/// A field named in `capabilityOverrideFields` is a declaration and must be
/// present; an unmarked absent field is the undeclared value.
pub fn declarations_hold(entry: &ModelEntry) -> bool {
    let marked = entry
        .metadata
        .as_ref()
        .map(|metadata| metadata.capability_override_fields.as_slice())
        .unwrap_or_default();
    (entry.capabilities.context_window.is_some()
        || !marked.iter().any(|name| name == "contextWindow"))
        && (entry.capabilities.max_output_tokens.is_some()
            || !marked.iter().any(|name| name == "maxOutputTokens"))
}

/// Records `field` as a declaration on `entry` unless it is marked already.
pub fn declare(entry: &mut ModelEntry, field: &str) {
    let fields = &mut entry
        .metadata
        .get_or_insert_default()
        .capability_override_fields;
    if !fields.iter().any(|name| name == field) {
        fields.push(field.into());
    }
}

/// The context window that the earlier formats' probe projection filled in,
/// and that the legacy schema defaulted to, when no source declared one.
const ASSUMED_CONTEXT_WINDOW: u64 = 128_000;
/// The maximum output of the same assumed pair.
const ASSUMED_MAX_OUTPUT_TOKENS: u64 = 16_384;

/// Declares the limits a stored or imported entry supplied, unless a value may
/// be the assumed pair above. Each field is decided on its own: a limit
/// `capabilityOverrideFields` marks, or `metadata.raw` declares, is a
/// declaration and gains the mark; an unmarked limit equal to the assumed
/// default is removed, because the probe filled each missing field with it and
/// the entry cannot tell the assumption from a typed value; any other value
/// can only have been typed or listed, so it is kept and marked. Returns
/// whether the entry changed.
pub fn declare_supplied_limits(entry: &mut ModelEntry) -> bool {
    let metadata = entry.metadata.as_ref();
    let marked = metadata
        .map(|metadata| metadata.capability_override_fields.clone())
        .unwrap_or_default();
    let (declared_context, declared_output) = metadata
        .map(|metadata| declared_limits(&metadata.raw))
        .unwrap_or((None, None));
    let context = supplied_limit(
        entry.capabilities.context_window,
        marked.iter().any(|name| name == "contextWindow"),
        declared_context.is_some(),
        ASSUMED_CONTEXT_WINDOW,
    );
    let output = supplied_limit(
        entry.capabilities.max_output_tokens,
        marked.iter().any(|name| name == "maxOutputTokens"),
        declared_output.is_some(),
        ASSUMED_MAX_OUTPUT_TOKENS,
    );
    let mut changed = false;
    match context {
        None => {}
        Some(SuppliedLimit::Remove) => {
            entry.capabilities.context_window = None;
            changed = true;
        }
        Some(SuppliedLimit::Mark) => {
            declare(entry, "contextWindow");
            changed = true;
        }
    }
    match output {
        None => {}
        Some(SuppliedLimit::Remove) => {
            entry.capabilities.max_output_tokens = None;
            changed = true;
        }
        Some(SuppliedLimit::Mark) => {
            declare(entry, "maxOutputTokens");
            changed = true;
        }
    }
    changed
}

/// One field's decision for a supplied limit; `None` leaves it alone.
enum SuppliedLimit {
    /// Keep the value and mark it as a declaration.
    Mark,
    /// Delete the value, because it may be the assumed default.
    Remove,
}

fn supplied_limit(
    value: Option<u64>,
    marked: bool,
    declared: bool,
    assumed: u64,
) -> Option<SuppliedLimit> {
    match value {
        None => None,
        Some(_) if marked => None,
        Some(_) if declared => Some(SuppliedLimit::Mark),
        Some(value) if value == assumed => Some(SuppliedLimit::Remove),
        Some(_) => Some(SuppliedLimit::Mark),
    }
}

/// Deletes the limits of projection output that neither the mark nor
/// `metadata.raw` declares: a listing, a bundle-derived entry or a descriptor
/// override is re-derived from its source, so an unmarked limit there was the
/// old assumed default whatever its value.
pub fn drop_undeclared_limits(model: &mut ModelEntry) -> bool {
    let marked = model
        .metadata
        .as_ref()
        .map(|metadata| metadata.capability_override_fields.as_slice())
        .unwrap_or_default();
    // A listed model keeps the item it came from in `raw`; a limit that item
    // declares was a declaration even when the store does not mark it.
    let (declared_context, declared_output) = model
        .metadata
        .as_ref()
        .map(|metadata| declared_limits(&metadata.raw))
        .unwrap_or((None, None));
    let mut changed = false;
    if model.capabilities.context_window.is_some()
        && !marked.iter().any(|field| field == "contextWindow")
        && declared_context.is_none()
    {
        model.capabilities.context_window = None;
        changed = true;
    }
    if model.capabilities.max_output_tokens.is_some()
        && !marked.iter().any(|field| field == "maxOutputTokens")
        && declared_output.is_none()
    {
        model.capabilities.max_output_tokens = None;
        changed = true;
    }
    changed
}

fn inherit_reasoning(current: &mut ModelReasoning, fallback: &ModelReasoning) {
    if !current.supported {
        return;
    }
    macro_rules! inherit {
        ($($field:ident),*) => { $(
            if current.$field.is_none() { current.$field = fallback.$field.clone(); }
        )* };
    }
    inherit!(
        default_enabled,
        mode,
        effort,
        effort_options,
        level,
        level_options,
        verbosity,
        verbosity_options,
        interleaved,
        summaries,
        visibility,
        continuation,
        budget,
        notes,
        off
    );
}

/// The efforts a descriptor declares, from the first shape it states: the
/// `reasoning_options` entry of type `effort`, the `effort` of each
/// `supported_reasoning_levels` item, or the flat `reasoning_levels` list.
///
/// Each shape is read at the descriptor's top level and under `raw`, where the
/// projection keeps the listing item it came from. A stated shape whose values
/// all drop is a declaration of an empty list — the model offers no effort
/// choice — so it is returned as one and the identified row does not fill it.
/// A descriptor that states none of the three returns `None`.
fn declared_efforts(input: &Map<String, Value>) -> Option<Vec<String>> {
    let stated = |key: &str| {
        input
            .get(key)
            .or_else(|| input.get("raw").and_then(|raw| raw.get(key)))
            .and_then(Value::as_array)
    };
    if let Some(values) = stated("reasoning_options")
        .and_then(|options| {
            options
                .iter()
                .find(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
        })
        .and_then(|effort| effort.get("values"))
        .and_then(Value::as_array)
    {
        return Some(effort_values(values.iter()));
    }
    if let Some(levels) = stated("supported_reasoning_levels") {
        return Some(effort_values(
            levels.iter().filter_map(|level| level.get("effort")),
        ));
    }
    stated("reasoning_levels").map(|levels| effort_values(levels.iter()))
}

/// The stated strings, trimmed, non-empty and unique, in their stated order.
fn effort_values<'a>(values: impl Iterator<Item = &'a Value>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && seen.insert(*value))
        .map(str::to_owned)
        .collect()
}

fn project_reasoning(
    value: Option<&Value>,
    options: Option<Vec<String>>,
) -> Result<Option<ModelReasoning>, ProjectionError> {
    Ok(match value {
        Some(Value::Bool(v)) => Some(ModelReasoning {
            supported: *v,
            effort_options: if *v { options } else { None },
            ..Default::default()
        }),
        Some(Value::Object(m)) => {
            let supported = m.get("supported").and_then(Value::as_bool).unwrap_or(true);
            let mut details = m.clone();
            details.remove("supported");
            details.remove("interleavedField");
            if let Some(default) = details.remove("default") {
                details.entry("defaultEnabled").or_insert(default);
            }
            if let Some(options) = details.remove("effort_options") {
                details.entry("effortOptions").or_insert(options);
            }
            if !details.contains_key("effortOptions")
                && supported
                && let Some(v) = options
            {
                details.insert(
                    "effortOptions".into(),
                    Value::Array(v.into_iter().map(Value::String).collect()),
                );
            }
            details.insert("supported".into(), Value::Bool(supported));
            Some(serde_json::from_value(Value::Object(details))?)
        }
        None => options.map(|v| ModelReasoning {
            supported: true,
            effort_options: Some(v),
            ..Default::default()
        }),
        _ => None,
    })
}
fn service_tier(value: Option<&Value>, raw: &Map<String, Value>) -> Option<ModelServiceTier> {
    if value == Some(&Value::Bool(false)) {
        return Some(ModelServiceTier {
            supported: false,
            default: None,
            options: vec![],
        });
    }
    let extra = raw.get("extra_capabilities");
    let direct = raw.get("serviceTier").or_else(|| raw.get("service_tier"));
    let extra = extra.and_then(|v| v.get("serviceTier").or_else(|| v.get("service_tier")));
    let fast = raw
        .get("experimental")
        .and_then(|v| v.pointer("/modes/fast/provider/body"))
        .and_then(|v| string(v.get("service_tier").or_else(|| v.get("serviceTier"))));
    let mut supported = None;
    let mut default = None;
    let mut options = vec![];
    for v in [direct, extra, value].into_iter().flatten() {
        let preference = v
            .is_object()
            .then(|| string(v.get("default").or_else(|| v.get("default_tier"))))
            .flatten()
            .or_else(|| v.as_str());
        let support = v
            .as_bool()
            .or_else(|| v.get("supported").and_then(Value::as_bool));
        if support.is_some() && (supported.is_none() || Some(v) == value) {
            supported = support;
        }
        if preference.is_some() && (default.is_none() || Some(v) == value) {
            default = preference.map(str::to_owned);
        }
        if let Some(values) = v.get("options").and_then(Value::as_array) {
            options.extend(strings(values));
        } else if let Some(v) = v.as_str() {
            options.push(v.to_owned());
        }
    }
    if let Some(fast) = fast {
        options.extend(["default".into(), fast.into()]);
    }
    let mut seen = HashSet::new();
    options.retain(|option| seen.insert(option.clone()));
    let default = default
        .or_else(|| options.iter().find(|v| *v == "default").cloned())
        .or_else(|| options.first().cloned());
    let supported =
        supported.or_else(|| (default.is_some() || !options.is_empty()).then_some(true))?;
    Some(ModelServiceTier {
        supported,
        default,
        options,
    })
}
fn pricing(raw: &Map<String, Value>) -> Option<ModelPricing> {
    let c = raw.get("pricing")?.as_object()?;
    Some(ModelPricing {
        input_token_price: c.get("input").and_then(Value::as_f64).filter(|n| *n >= 0.),
        output_token_price: c.get("output").and_then(Value::as_f64).filter(|n| *n >= 0.),
        currency: None,
    })
}
fn apply_capabilities(c: &mut CatalogCapabilities, m: &Map<String, Value>) {
    if let Some(a) = m.get("modalities").and_then(Value::as_array) {
        c.modalities = strings(a)
    }
    macro_rules! b {
        ($j:literal,$f:ident) => {
            if let Some(v) = m.get($j).and_then(Value::as_bool) {
                c.$f = v
            }
        };
    }
    b!("toolCalling", tool_calling);
    b!("streaming", streaming);
    b!("structuredOutput", structured_output);
    b!("vision", vision);
    b!("audio", audio);
    b!("reasoning", reasoning);
    if let Some(state) = m.get("providerState").and_then(Value::as_object) {
        c.provider_state
            .get_or_insert_default()
            .extend(state.clone());
    }
}

/// The kind a stated `type` names, lowercased with `_` read as `-`. `model`
/// names the object (`"object": "model"`) rather than a kind, so it reads as
/// none, and so does an empty `type`. This and `raw_kind` are the one reader
/// of a stated kind: `raw_kind` reads a raw model map, and a caller that has
/// only the stated string normalises it here.
pub fn kind_of(value: &str) -> Option<String> {
    let kind = value.trim().to_lowercase().replace('_', "-");
    (!kind.is_empty() && kind != "model").then_some(kind)
}

/// The kind a raw model map states in its `type`, read by `kind_of`. Every
/// reader of a stated kind reads it here: the catalog's rows, an entry's own
/// kind, and the kind a listed id carries.
pub fn raw_kind(raw: &Map<String, Value>) -> Option<String> {
    kind_of(string(raw.get("type"))?)
}

/// The kind a model descriptor declares in its raw `type`.
pub(crate) fn model_kind(metadata: &ModelMetadata) -> Option<String> {
    raw_kind(&metadata.raw)
}

/// The kind an entry states: the `type` in the raw provider fields its id came
/// from, which a merge reads to decide which rows may join it.
pub(crate) fn entry_kind(entry: &ModelEntry) -> Option<String> {
    entry.metadata.as_ref().and_then(model_kind)
}

fn string(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
fn positive(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64).filter(|x| *x > 0)
}
fn strings(v: &[Value]) -> Vec<String> {
    v.iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}
