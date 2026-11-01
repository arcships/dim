//! Product catalog, static connections and model selection. OAuth plugins own
//! managed credentials; this plugin owns provider configuration and SQLite records.
mod auxiliary;
mod drivers;
mod editing;
mod legacy;
pub(crate) mod management;
mod models;
mod new_api;
mod preferences;
mod remote;
mod secrets;
mod view;
pub use dim_agent_types::ModelPreferences;
pub use editing::{ConnectionInput, CreateProvider, ProbeProvider, ProbeResult, ProviderPatch};
pub use legacy::{
    ImportResult, LEGACY_METADATA_FIELDS, LEGACY_PROVIDER_FIELDS, LegacyConversion, LegacyRow,
    LegacySelection, LegacySnapshot, convert_legacy_provider, convert_legacy_selection,
    legacy_omitted_fields, read_legacy_source,
};
pub use models::PreferencesPatch;
use secrets::StaticCredential;
pub use secrets::{ProviderSecret, ProviderSecretHost, ResolveProviderSecret};
pub use view::{ManagementProvider, ManagementView, ModelsListed};

use crate::{
    codex_oauth::OpenAICodexOAuthService,
    grok_oauth::GrokOAuthService,
    managed_oauth,
    managed_provider::{AccountSnapshot, CredentialLease, ManagedModel, ManagedProviders},
    model_catalog::{
        BundledCatalog, CatalogCapabilities, CatalogProvider, MAX_MODEL_TOKENS, ModelEdit,
        ModelEntry, ModelMetadata, ModelSource, NewApiEndpoint, ProviderProjection,
        declarations_hold, declare, declare_supplied_limits, declared_limit_values,
        drop_undeclared_limits, inherit_limits, limits_valid,
    },
    storage::SqliteStorage,
};
use dim_agent_capabilities::host::*;
use dim_agent_runtime::{AgentError, Names};
use dim_agent_types::{
    ModelCapabilitySnapshot, ModelConfig, ModelId, ModelRequest, ModelSelection,
};
use futures_util::future::BoxFuture as SharedFuture;
use reqwest::Client;
use rusqlite::{OptionalExtension, params};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Provider or model was not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    InvalidConfig(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("Provider authentication is required")]
    AuthRequired,
    #[error("Provider authentication was rejected")]
    AuthFailed,
    #[error("Provider request failed")]
    TransportFailed,
    #[error("Provider operation cancelled")]
    Cancelled,
    #[error("Provider operation failed")]
    Failed,
}

impl From<HostError> for ProviderError {
    fn from(error: HostError) -> Self {
        match error {
            HostError::Cancelled(_) => Self::Cancelled,
            HostError::Failed {
                kind: HostErrorKind::OwnerClosed,
                ..
            } => Self::Cancelled,
            HostError::Failed {
                kind: HostErrorKind::InvalidRequest,
                message,
                ..
            } => Self::InvalidConfig(message),
            HostError::Failed {
                kind: HostErrorKind::Unsupported,
                message,
                ..
            } => Self::Unsupported(message),
            HostError::Failed {
                kind:
                    HostErrorKind::Http {
                        status: Some(401 | 403),
                        ..
                    },
                ..
            } => Self::AuthFailed,
            HostError::Failed {
                kind: HostErrorKind::Transport | HostErrorKind::Http { .. },
                ..
            } => Self::TransportFailed,
            _ => Self::Failed,
        }
    }
}

impl From<AgentError> for ProviderError {
    fn from(_: AgentError) -> Self {
        Self::Failed
    }
}
impl From<managed_oauth::OAuthError> for ProviderError {
    fn from(error: managed_oauth::OAuthError) -> Self {
        use managed_oauth::OAuthError;
        match error {
            OAuthError::Missing | OAuthError::RecoveryRequired => Self::AuthRequired,
            OAuthError::Rejected => Self::AuthFailed,
            OAuthError::AccountChanged => Self::Conflict("Managed account changed".into()),
            OAuthError::Transient(Some(401 | 403)) => Self::AuthFailed,
            OAuthError::Transient(_) => Self::TransportFailed,
            OAuthError::Closed => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}
impl From<crate::managed_provider::ManagedProviderError> for ProviderError {
    fn from(error: crate::managed_provider::ManagedProviderError) -> Self {
        match error.code {
            "account_required" | "account_recovery_required" => Self::AuthRequired,
            "account_changed" => Self::Conflict("Managed account changed".into()),
            "account_identity_rejected" => Self::AuthFailed,
            _ => Self::Failed,
        }
    }
}
impl From<rusqlite::Error> for ProviderError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Failed
    }
}
impl From<serde_json::Error> for ProviderError {
    fn from(_: serde_json::Error) -> Self {
        Self::Failed
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    pub endpoint: Option<ModelProtocol>,
    pub config: ModelConfig,
    pub capabilities: CatalogCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_limits: Vec<String>,
    pub unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zdr: bool,
    pub metadata: Option<ModelMetadata>,
}

/// The error text for a model no source declares limits for.
fn undeclared_limits(capabilities: &CatalogCapabilities) -> String {
    format!(
        "Model has no declared {}",
        capabilities.missing_limits().join(" or ")
    )
}

/// The adopted capability snapshot of `model`: exactly the catalog fields a
/// selection carries (ContextCompaction §13.2). A model with an undeclared
/// limit cannot be converted, which is where adoption refuses it.
fn capability_snapshot(
    model: &ProviderModel,
    target: ModelId,
) -> Result<ModelCapabilitySnapshot, ProviderError> {
    let capabilities = model
        .capabilities
        .declared()
        .ok_or_else(|| ProviderError::InvalidConfig(undeclared_limits(&model.capabilities)))?;
    Ok(ModelCapabilitySnapshot {
        target,
        capabilities,
        reasoning: model.metadata.as_ref().and_then(|m| m.reasoning.clone()),
        service_tier: model.metadata.as_ref().and_then(|m| m.service_tier.clone()),
    })
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRecord {
    kind: ManagedRecordKind,
    account: AccountSnapshot,
    models: Vec<ManagedModel>,
    model_config_overrides: Vec<ModelEntry>,
    disabled_model_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_preferences: BTreeMap<String, ModelPreferences>,
    version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ManagedRecordKind {
    Managed,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum PersistedProviderRecord {
    Managed(Box<ProviderRecord>),
    Static(Box<StaticProviderRecord>),
}
fn parse_provider_row(
    id: &str,
    version: u32,
    body: &str,
) -> Result<PersistedProviderRecord, HostError> {
    let record: PersistedProviderRecord =
        serde_json::from_str(body).map_err(|_| failure("Invalid persisted provider record"))?;
    match record {
        PersistedProviderRecord::Managed(record) => {
            validate_catalog(version, &record).map_err(failure)?;
            Ok(PersistedProviderRecord::Managed(record))
        }
        PersistedProviderRecord::Static(mut record) => {
            if record.version != version {
                return Err(failure("Provider version mismatch"));
            }
            validate_static_record(id, &mut record)?;
            Ok(PersistedProviderRecord::Static(record))
        }
    }
}
fn parse_static_row(
    id: &str,
    version: u32,
    body: &str,
) -> Result<Option<StaticProviderRecord>, HostError> {
    Ok(match parse_provider_row(id, version, body)? {
        PersistedProviderRecord::Static(record) => Some(*record),
        PersistedProviderRecord::Managed(_) => None,
    })
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticProviderRecord {
    kind: StaticRecordKind,
    display_name: String,
    driver_kind: String,
    auth: StaticAuth,
    #[serde(default)]
    credential_authority: CredentialAuthority,
    base_url: String,
    #[serde(default)]
    network: ModelNetwork,
    #[serde(default)]
    headers: HashMap<String, String>,
    credential: Option<StaticCredential>,
    models: Vec<ModelEntry>,
    #[serde(default)]
    remote_models: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    endpoint_types: BTreeMap<String, NewApiEndpoint>,
    /// Derived at the persistence/input boundary, never read from stored JSON.
    #[serde(skip)]
    routes: HashMap<String, ModelProtocol>,
    /// Derived from an admitted Bedrock endpoint; never trusted from storage.
    #[serde(skip)]
    bedrock_region: Option<String>,
    #[serde(default)]
    model_config_overrides: Vec<ModelEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    user_models: Vec<ModelEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_preferences: BTreeMap<String, ModelPreferences>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    endpoint_overrides: BTreeMap<String, NewApiEndpoint>,
    #[serde(default)]
    disabled_model_ids: Vec<String>,
    enabled: bool,
    #[serde(default)]
    definition_fingerprint: Option<String>,
    version: u32,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum StaticRecordKind {
    Builtin,
    Custom,
}

impl StaticProviderRecord {
    pub fn is_builtin(&self) -> bool {
        self.kind == StaticRecordKind::Builtin
    }
    /// A builtin at version 1, the version seeding writes: no import or
    /// reseed has replaced it.
    pub fn is_pristine_builtin(&self) -> bool {
        self.is_builtin() && self.version == 1
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn with_version(mut self, version: u32) -> Self {
        self.version = version;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialAuthority {
    #[default]
    Static,
    OpenaiCodex,
    Grok,
}

fn managed_authority(provider_id: &str) -> CredentialAuthority {
    match provider_id {
        "openai-codex" => CredentialAuthority::OpenaiCodex,
        "grok-build" => CredentialAuthority::Grok,
        _ => CredentialAuthority::Static,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum StaticAuth {
    ApiKey { header_name: String, prefix: String },
    Bearer,
    CustomHeader { header_name: String },
    Anonymous,
    AwsBedrock,
}

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum CredentialUpdate {
    Keep,
    Clear,
    Set {
        value: String,
    },
    #[serde(rename = "set_aws")]
    SetAws {
        value: AwsCredentials,
    },
    #[serde(rename = "set_ref")]
    SetRef {
        reference: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub provider_id: String,
    pub source: &'static str,
    pub display_name: String,
    pub driver_kind: String,
    pub auth: StaticAuth,
    pub credential_authority: CredentialAuthority,
    pub base_url: String,
    pub proxy_configured: bool,
    pub timeout_ms: Option<u64>,
    pub header_names: Vec<String>,
    pub credential_configured: bool,
    /// Whether `provider.refresh` can list this provider's models.
    pub remote_model_list: bool,
    pub enabled: bool,
    pub version: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderList {
    pub providers: Vec<ProviderSummary>,
    pub errors: Vec<ProviderRecordError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecordError {
    pub provider_id: String,
    pub message: String,
}

impl ProviderRecord {
    fn catalog(&self, bundled: &BundledCatalog) -> ProviderCatalog {
        let defaults: Vec<_> = self
            .models
            .iter()
            .map(|model| model.entry.clone())
            .collect();
        let entries: Vec<_> = defaults
            .into_iter()
            .map(|entry| {
                self.model_config_overrides
                    .iter()
                    .find(|m| m.model_id == entry.model_id)
                    .map(|replacement| inherit_limits(replacement.clone(), &entry))
                    .unwrap_or(entry)
            })
            .collect();
        ProviderCatalog {
            account: Some(self.account.clone()),
            version: self.version,
            disabled_model_ids: self.disabled_model_ids.clone(),
            models: entries
                .into_iter()
                .filter_map(|mut entry| {
                    let source = self
                        .models
                        .iter()
                        .find(|model| model.entry.model_id == entry.model_id)?;
                    fill_bundled_limits(bundled, &mut entry);
                    let missing_limits = entry.capabilities.missing_limits();
                    // `limits_undeclared` is derived from the effective limits,
                    // so a supplied limit makes the model available again.
                    let unavailable_reason =
                        limits_reason(source.unavailable_reason.clone(), &missing_limits);
                    Some(ProviderModel {
                        id: entry.model_id,
                        name: entry.display_name,
                        endpoint: Some(source.protocol),
                        config: ModelConfig {
                            max_tokens: entry.capabilities.max_output_tokens.map(|n| n as f64),
                            ..Default::default()
                        },
                        capabilities: entry.capabilities,
                        missing_limits,
                        unavailable_reason,
                        zdr: source.zdr,
                        metadata: entry.metadata,
                    })
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalog {
    pub account: Option<AccountSnapshot>,
    pub models: Vec<ProviderModel>,
    pub disabled_model_ids: Vec<String>,
    pub version: u32,
}
impl ProviderCatalog {
    fn selectable(&self, id: &str) -> Option<&ProviderModel> {
        self.models.iter().find(|model| {
            model.id == id
                && model.unavailable_reason.is_none()
                && !self.disabled_model_ids.contains(&model.id)
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSelection {
    pub provider_id: String,
    pub model_id: String,
    pub version: u32,
}

fn read_static(
    connection: &rusqlite::Connection,
    provider_id: &str,
) -> dim_agent_runtime::Result<Option<StaticProviderRecord>> {
    let row: Option<(u32, String)> = connection
        .query_row(
            "SELECT version,record FROM providers WHERE provider_id=?1",
            [provider_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    row.map(|(version, body)| parse_static(provider_id, version, &body))
        .transpose()
        .map_err(storage)
}

pub fn parse_static(
    provider_id: &str,
    version: u32,
    body: &str,
) -> Result<StaticProviderRecord, HostError> {
    let mut record: StaticProviderRecord =
        serde_json::from_str(body).map_err(|_| failure("Invalid persisted provider record"))?;
    if record.version != version {
        return Err(failure("Provider version mismatch"));
    }
    validate_static_record(provider_id, &mut record)?;
    Ok(record)
}

fn validate_static_record(
    provider_id: &str,
    record: &mut StaticProviderRecord,
) -> Result<(), HostError> {
    if record.credential_authority != managed_authority(provider_id) {
        return Err(failure("Invalid provider credential authority"));
    }
    let managed = record.credential_authority != CredentialAuthority::Static;
    if managed {
        let (driver, base_url) = match record.credential_authority {
            CredentialAuthority::OpenaiCodex => {
                ("openai-codex", "https://chatgpt.com/backend-api/codex")
            }
            CredentialAuthority::Grok => ("xai-grok-build", "https://api.x.ai/v1"),
            CredentialAuthority::Static => unreachable!(),
        };
        if record.kind != StaticRecordKind::Builtin
            || record.auth != StaticAuth::Bearer
            || record.driver_kind != driver
            || record.base_url != base_url
            || !record.headers.is_empty()
            || record.credential.is_some()
        {
            return Err(failure("Managed provider binding is read-only"));
        }
    } else if matches!(
        record.driver_kind.as_str(),
        "openai-codex" | "xai-grok-build"
    ) {
        return Err(failure(
            "Subscription driver requires its managed authority",
        ));
    }
    if provider_id.trim().is_empty() || record.display_name.trim().is_empty() {
        return Err(failure("Provider identity and name must not be empty"));
    }
    if let Some(proxy) = &record.network.proxy {
        let url = reqwest::Url::parse(proxy).map_err(|_| failure("Invalid provider proxy URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(failure("Provider proxy must be an HTTP or HTTPS root URL"));
        }
    }
    if record
        .network
        .timeout_ms
        .is_some_and(|value| value == 0 || value > dim_agent_types::JS_MAX_SAFE_INTEGER)
    {
        return Err(failure(
            "Provider timeout must be a positive safe JSON integer",
        ));
    }
    record.bedrock_region = None;
    if record.kind == StaticRecordKind::Custom || !record.base_url.is_empty() || record.enabled {
        let url = reqwest::Url::parse(&record.base_url)
            .map_err(|_| failure("Invalid provider base URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(failure(
                "Provider base URL must be HTTP(S) without credentials, query or fragment",
            ));
        }
        if record.driver_kind == "azure-openai"
            && (url.scheme() != "https"
                || !url.host_str().is_some_and(|host| {
                    host.ends_with(".openai.azure.com") || host.ends_with(".services.ai.azure.com")
                })
                || url.path().trim_end_matches('/') != "/openai")
        {
            return Err(failure(
                "Azure OpenAI base URL must be an official resource /openai endpoint",
            ));
        }
        if record.driver_kind == "amazon-bedrock" {
            let host = url.host_str().unwrap_or_default();
            let region = host
                .strip_prefix("bedrock-runtime.")
                .and_then(|value| value.strip_suffix(".amazonaws.com"))
                .filter(|value| {
                    !value.is_empty()
                        && value.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        })
                });
            if url.scheme() != "https"
                || url.port().is_some()
                || url.path() != "/"
                || region.is_none()
            {
                return Err(failure(
                    "Amazon Bedrock base URL must be an official regional runtime endpoint",
                ));
            }
            record.bedrock_region = region.map(str::to_owned);
        }
    }
    if (record.auth == StaticAuth::AwsBedrock) != (record.driver_kind == "amazon-bedrock") {
        return Err(failure(
            "AWS Bedrock auth is only valid for the Amazon Bedrock driver",
        ));
    }
    if (record.kind == StaticRecordKind::Custom || record.enabled)
        && !drivers::supported(&record.driver_kind)
    {
        return Err(failure("Unsupported provider driver"));
    }
    for models in [
        &record.models,
        &record.model_config_overrides,
        &record.user_models,
    ] {
        let mut ids = HashSet::new();
        if models.iter().any(|model| {
            model.model_id.trim().is_empty()
                || model.display_name.trim().is_empty()
                || !ids.insert(&model.model_id)
                || !limits_valid(&model.capabilities)
                || !declarations_hold(model)
        }) {
            return Err(failure(
                "Provider contains invalid or duplicate model descriptors",
            ));
        }
    }
    if record.user_models.iter().any(|user| {
        record
            .models
            .iter()
            .chain(&record.model_config_overrides)
            .any(|model| model.model_id == user.model_id)
    }) {
        return Err(failure(
            "User models conflict with default models or descriptor overrides",
        ));
    }
    if !record.endpoint_overrides.is_empty()
        && (record.driver_kind != "new-api-router"
            || record.credential_authority != CredentialAuthority::Static
            || record
                .endpoint_overrides
                .values()
                .any(|endpoint| endpoint.protocol().is_none()))
    {
        return Err(failure(
            "Endpoint overrides require an executable static New API route",
        ));
    }
    let credential_header = match &record.auth {
        StaticAuth::ApiKey { header_name, .. } | StaticAuth::CustomHeader { header_name } => {
            Some(header_name.as_str())
        }
        StaticAuth::Bearer => Some("Authorization"),
        StaticAuth::Anonymous | StaticAuth::AwsBedrock => None,
    };
    let mut names = HashSet::new();
    for (name, value) in &record.headers {
        let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| failure("Invalid HTTP header name"))?;
        let signer_owned = record.auth == StaticAuth::AwsBedrock
            && matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "content-type"
                    | "host"
                    | "x-amz-date"
                    | "x-amz-content-sha256"
                    | "x-amz-security-token"
            );
        if !names.insert(header)
            || credential_header.is_some_and(|h| h.eq_ignore_ascii_case(name))
            || signer_owned
        {
            return Err(failure("Duplicate or conflicting credential header"));
        }
        reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| failure("Invalid HTTP header value"))?;
    }
    if let Some(name) = credential_header {
        reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| failure("Invalid credential header"))?;
    }
    if let StaticAuth::ApiKey { prefix, .. } = &record.auth {
        reqwest::header::HeaderValue::from_str(prefix)
            .map_err(|_| failure("Invalid credential header prefix"))?;
    }
    if let Some(value) = &record.credential {
        match value {
            StaticCredential::Inline(value) => value.validate(&record.auth)?,
            StaticCredential::Reference { reference } => {
                if reference.trim().is_empty() || reference.chars().any(char::is_control) {
                    return Err(failure("Invalid provider secret reference"));
                }
            }
        }
    } else if !managed && record.enabled && record.auth != StaticAuth::Anonymous {
        return Err(failure("Enabled provider credential is required"));
    }
    let models: Vec<_> = record
        .models
        .iter()
        .chain(&record.user_models)
        .map(|model| {
            record
                .model_config_overrides
                .iter()
                .find(|entry| entry.model_id == model.model_id)
                .unwrap_or(model)
                .clone()
        })
        .collect();
    record.routes = drivers::routes(
        &record.driver_kind,
        &models,
        &record.endpoint_types,
        &record.endpoint_overrides,
    )?;
    Ok(())
}

fn static_entries(
    bundled: &BundledCatalog,
    provider_id: &str,
    record: &StaticProviderRecord,
) -> Vec<ModelEntry> {
    let models: Vec<_> = record
        .models
        .iter()
        .chain(&record.user_models)
        .cloned()
        .collect();
    bundled
        .effective_models(
            &ProviderProjection {
                provider_id: provider_id.to_owned(),
                source: Some(if record.kind == StaticRecordKind::Builtin {
                    ModelSource::Builtin
                } else {
                    ModelSource::Custom
                }),
                models: models.clone(),
                ..Default::default()
            },
            &models,
            &record.model_config_overrides,
        )
        .into_iter()
        .filter(|entry| {
            record
                .models
                .iter()
                .chain(&record.user_models)
                .any(|m| m.model_id == entry.model_id)
        })
        .collect()
}

fn static_models(
    bundled: &BundledCatalog,
    provider_id: &str,
    record: &StaticProviderRecord,
    account_available: bool,
) -> Vec<ProviderModel> {
    static_entries(bundled, provider_id, record)
        .into_iter()
        .map(|entry| {
            let missing_limits = entry.capabilities.missing_limits();
            let unavailable =
                matches!(entry.status, crate::model_catalog::ModelStatus::Unavailable)
                    || !entry.capabilities.modalities.iter().any(|m| m == "text")
                    || (record.driver_kind != "new-api-router"
                        && entry
                            .metadata
                            .as_ref()
                            .and_then(crate::model_catalog::model_kind)
                            .is_some_and(|kind| kind != "chat"))
                    || !record.enabled
                    || !record.routes.contains_key(&entry.model_id)
                    || !account_available
                    || (record.credential_authority == CredentialAuthority::Static
                        && record.auth != StaticAuth::Anonymous
                        && record.credential.is_none());
            let endpoint = record.routes.get(&entry.model_id).copied();
            ProviderModel {
                id: entry.model_id,
                name: entry.display_name,
                endpoint,
                config: ModelConfig {
                    max_tokens: entry.capabilities.max_output_tokens.map(|n| n as f64),
                    ..Default::default()
                },
                capabilities: entry.capabilities,
                unavailable_reason: if unavailable {
                    Some("provider_unavailable".into())
                } else if !missing_limits.is_empty() {
                    Some("limits_undeclared".into())
                } else {
                    None
                },
                missing_limits,
                zdr: false,
                metadata: entry.metadata,
            }
        })
        .collect()
}

/// Whether `provider.refresh` can list this static provider's models: its
/// driver lists them, and the bundled definition does not opt out.
fn can_list_models(
    bundled: &BundledCatalog,
    provider_id: &str,
    record: &StaticProviderRecord,
) -> bool {
    record.credential_authority == CredentialAuthority::Static
        && drivers::remote_model_list(&record.driver_kind)
        && !bundled
            .provider(provider_id)
            .and_then(|provider| provider.metadata.as_ref())
            .is_some_and(|metadata| metadata.remote_model_list == Some(false))
}

/// The conflict a listing raises when a discovered ID belongs to a user model.
/// `provider.refresh` and an update that lists both fail with it — the catalog
/// is never renamed or merged — and it names every colliding ID and the fix,
/// so the user can delete them and retry.
fn reject_user_model_conflicts(
    record: &StaticProviderRecord,
    models: &[ModelEntry],
) -> Result<(), ProviderError> {
    let colliding: Vec<&str> = models
        .iter()
        .filter(|model| {
            record
                .user_models
                .iter()
                .any(|user| user.model_id == model.model_id)
        })
        .map(|model| model.model_id.as_str())
        .collect();
    if colliding.is_empty() {
        return Ok(());
    }
    Err(ProviderError::Conflict(format!(
        "Discovered models conflict with user-added models: {}. Delete them through provider.models.delete, then retry.",
        colliding.join(", ")
    )))
}

fn static_summary(
    bundled: &BundledCatalog,
    provider_id: String,
    record: StaticProviderRecord,
) -> ProviderSummary {
    let remote_model_list = can_list_models(bundled, &provider_id, &record);
    ProviderSummary {
        provider_id,
        source: if record.kind == StaticRecordKind::Builtin {
            "builtin"
        } else {
            "custom"
        },
        display_name: record.display_name,
        driver_kind: record.driver_kind,
        auth: record.auth,
        credential_authority: record.credential_authority,
        base_url: record.base_url,
        header_names: {
            let mut names: Vec<_> = record.headers.into_keys().collect();
            names.sort();
            names
        },
        credential_configured: record.credential.is_some(),
        remote_model_list,
        proxy_configured: record.network.proxy.is_some(),
        timeout_ms: record.network.timeout_ms,
        enabled: record.enabled,
        version: record.version,
    }
}

fn static_catalog(
    bundled: &BundledCatalog,
    provider_id: &str,
    record: &StaticProviderRecord,
    account_available: bool,
) -> ProviderCatalog {
    ProviderCatalog {
        account: None,
        models: static_models(bundled, provider_id, record, account_available),
        disabled_model_ids: record.disabled_model_ids.clone(),
        version: record.version,
    }
}

fn seeded(provider: &CatalogProvider) -> bool {
    matches!(provider.auth.kind.as_str(), "api_key" | "aws_bedrock")
        || managed_authority(&provider.provider_id) != CredentialAuthority::Static
}

fn bundled_seed(provider: &CatalogProvider) -> Result<StaticProviderRecord, HostError> {
    let authority = managed_authority(&provider.provider_id);
    let endpoint_types = provider
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.raw.as_ref())
        .map(|raw| raw.endpoint_type_by_model.clone())
        .unwrap_or_default();
    let fingerprint = serde_json::to_vec(&(
        &provider.display_name,
        &provider.driver_kind,
        &provider.auth,
        &provider.default_base_url,
        &provider.models,
        &endpoint_types,
    ))
    .map_err(failure)?;
    Ok(StaticProviderRecord {
        kind: StaticRecordKind::Builtin,
        display_name: provider.display_name.clone(),
        driver_kind: provider.driver_kind.clone(),
        auth: if authority == CredentialAuthority::Static {
            if provider.auth.kind == "aws_bedrock" {
                StaticAuth::AwsBedrock
            } else {
                StaticAuth::ApiKey {
                    header_name: provider
                        .auth
                        .header_name
                        .clone()
                        .unwrap_or_else(|| "Authorization".into()),
                    prefix: provider.auth.prefix.clone(),
                }
            }
        } else {
            StaticAuth::Bearer
        },
        credential_authority: authority,
        base_url: provider.default_base_url.clone().unwrap_or_default(),
        network: ModelNetwork::default(),
        headers: HashMap::new(),
        credential: None,
        models: provider.models.clone(),
        remote_models: false,
        endpoint_types,
        routes: HashMap::new(),
        bedrock_region: None,
        model_config_overrides: Vec::new(),
        user_models: Vec::new(),
        model_preferences: BTreeMap::new(),
        endpoint_overrides: BTreeMap::new(),
        disabled_model_ids: Vec::new(),
        enabled: authority != CredentialAuthority::Static,
        definition_fingerprint: Some(format!("{:x}", Sha256::digest(fingerprint))),
        version: 1,
    })
}

fn bundled_seed_for(
    bundled: &BundledCatalog,
    provider_id: &str,
) -> Result<Option<StaticProviderRecord>, HostError> {
    bundled
        .provider(provider_id)
        .filter(|provider| seeded(provider))
        .map(bundled_seed)
        .transpose()
}

fn bundled_seeds(
    bundled: &BundledCatalog,
) -> Result<HashMap<String, StaticProviderRecord>, HostError> {
    bundled
        .providers()
        .filter(|provider| seeded(provider))
        .map(|provider| Ok((provider.provider_id.clone(), bundled_seed(provider)?)))
        .collect()
}

fn reseed_candidate(
    previous: &StaticProviderRecord,
    mut definition: StaticProviderRecord,
) -> StaticProviderRecord {
    definition.base_url = previous.base_url.clone();
    definition.network = previous.network.clone();
    definition.headers = previous.headers.clone();
    definition.credential = previous.credential.clone();
    if previous.remote_models {
        definition.models = previous.models.clone();
        definition.remote_models = true;
    }
    definition.model_config_overrides = previous.model_config_overrides.clone();
    definition.user_models = previous.user_models.clone();
    definition.model_preferences = previous.model_preferences.clone();
    definition.endpoint_overrides = previous.endpoint_overrides.clone();
    definition.disabled_model_ids = previous.disabled_model_ids.clone();
    definition.enabled = previous.enabled && drivers::supported(&definition.driver_kind);
    definition
}

pub struct ProviderService {
    db: Arc<SqliteStorage>,
    oauth: Arc<ManagedProviders>,
    codex: Arc<OpenAICodexOAuthService>,
    grok: Arc<GrokOAuthService>,
    secret_host: Option<Arc<ProviderSecretHost>>,
    bundled: Arc<BundledCatalog>,
    client: Client,
    stop: CancellationToken,
    tasks: TaskTracker,
    admission: Mutex<()>,
}
fn failure(error: impl std::fmt::Display) -> HostError {
    HostError::failed(error.to_string())
}
fn storage(error: impl std::fmt::Display) -> AgentError {
    AgentError::Storage(error.to_string())
}
fn same_account(a: &AccountSnapshot, b: &AccountSnapshot) -> bool {
    a.credential_epoch == b.credential_epoch && a.generation == b.generation
}
fn read_catalog(
    connection: &rusqlite::Connection,
    provider_id: &str,
) -> dim_agent_runtime::Result<Option<ProviderRecord>> {
    let row: Option<(u32, String)> = connection
        .query_row(
            "SELECT version, record FROM providers WHERE provider_id = ?1",
            [provider_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    row.map(|(version, body)| parse_catalog(version, &body))
        .transpose()
}
fn parse_catalog(version: u32, body: &str) -> dim_agent_runtime::Result<ProviderRecord> {
    let catalog: ProviderRecord = serde_json::from_str(body).map_err(storage)?;
    validate_catalog(version, &catalog)?;
    Ok(catalog)
}
fn validate_catalog(version: u32, catalog: &ProviderRecord) -> dim_agent_runtime::Result<()> {
    if catalog.version != version {
        return Err(storage("Provider version mismatch"));
    }
    let mut ids = HashSet::new();
    if catalog.account.account.is_none()
        || catalog.models.iter().any(|model| {
            model.entry.model_id.trim().is_empty()
                || !ids.insert(&model.entry.model_id)
                || !limits_valid(&model.entry.capabilities)
                || (model.unavailable_reason.is_none()
                    && !model.entry.capabilities.missing_limits().is_empty())
        })
    {
        return Err(storage("Invalid persisted provider catalog"));
    }
    let mut overrides = HashSet::new();
    if catalog.model_config_overrides.iter().any(|model| {
        model.model_id.trim().is_empty()
            || !overrides.insert(&model.model_id)
            || !limits_valid(&model.capabilities)
            || !declarations_hold(model)
    }) {
        return Err(storage("Invalid persisted model override"));
    }
    Ok(())
}
/// The #522 format step. Projection output is re-derived, so a stored limit
/// that neither `capabilityOverrideFields` marks nor `metadata.raw` declares
/// there was the old assumed default and the upgrade deletes it, whatever its
/// value: that is the remote listing of a record with `remoteModels`, a
/// builtin record's bundle-derived `models`, and the managed record's
/// `models[].entry`. A custom record's own models, its user models and its
/// descriptor overrides hold what a client or the legacy importers supplied,
/// which no format before this step marked; each field there is decided by
/// `declare_supplied_limits`: a marked or raw-declared limit is a declaration,
/// an unmarked limit equal to the assumed 128000/16384 may be the probe's fill
/// and is deleted, and any other value was typed and gains the mark. A managed
/// record's overrides are written only by `models.edit`, which marks every
/// limit it edits, so they keep the strict rule. The storage layer calls this
/// inside its startup transaction; this module owns the record's field names.
pub(crate) fn rewrite_stored_limits(
    connection: &rusqlite::Connection,
) -> dim_agent_runtime::Result<()> {
    let mut query = connection
        .prepare("SELECT provider_id, record FROM providers")
        .map_err(storage)?;
    let rows = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)?;
    for (id, body) in rows {
        // A record that does not parse is reported by `list`; the upgrade
        // leaves its bytes untouched.
        let Ok(record) = serde_json::from_str::<PersistedProviderRecord>(&body) else {
            continue;
        };
        let (changed, body) = match record {
            PersistedProviderRecord::Static(mut record) => {
                let mut changed = false;
                for model in &mut record.models {
                    changed |= if record.remote_models || record.kind == StaticRecordKind::Builtin {
                        drop_undeclared_limits(model)
                    } else {
                        declare_supplied_limits(model)
                    };
                }
                // A descriptor override holds what a client or the legacy
                // importers supplied, and the importers copy the legacy row
                // without a mark, so the override takes the same per-field
                // rule as the user models.
                for model in &mut record.model_config_overrides {
                    changed |= declare_supplied_limits(model);
                }
                for model in &mut record.user_models {
                    changed |= declare_supplied_limits(model);
                }
                (changed, serde_json::to_string(&*record).map_err(storage)?)
            }
            PersistedProviderRecord::Managed(mut record) => {
                let mut changed = false;
                for model in &mut record.models {
                    changed |= drop_undeclared_limits(&mut model.entry);
                    // The managed record shape keeps a reason on a model with
                    // an absent limit; the catalog recomputes the reason from
                    // the effective limits.
                    if !model.entry.capabilities.missing_limits().is_empty()
                        && model.unavailable_reason.is_none()
                    {
                        model.unavailable_reason = Some("limits_undeclared".into());
                        changed = true;
                    }
                }
                for model in &mut record.model_config_overrides {
                    changed |= drop_undeclared_limits(model);
                }
                (changed, serde_json::to_string(&*record).map_err(storage)?)
            }
        };
        if changed {
            connection
                .execute(
                    "UPDATE providers SET record=?2 WHERE provider_id=?1",
                    params![id, body],
                )
                .map_err(storage)?;
        }
    }
    Ok(())
}

/// The limits a model does not declare come from the bundled catalog, which
/// owns the descriptors both the catalog and the management view publish.
pub(crate) fn fill_bundled_limits(bundled: &BundledCatalog, model: &mut ModelEntry) {
    if let Some(found) = bundled.fallback(&model.model_id) {
        model.capabilities.context_window = model
            .capabilities
            .context_window
            .or(found.capabilities.context_window);
        model.capabilities.max_output_tokens = model
            .capabilities
            .max_output_tokens
            .or(found.capabilities.max_output_tokens);
    }
}

/// A stored reason other than the limit-derived one stands; otherwise an
/// absent limit names `limits_undeclared`, so a supplied limit clears it.
pub(crate) fn limits_reason(reason: Option<String>, missing_limits: &[String]) -> Option<String> {
    reason
        .filter(|reason| reason != "limits_undeclared")
        .or_else(|| (!missing_limits.is_empty()).then(|| "limits_undeclared".into()))
}

fn save_catalog(
    connection: &rusqlite::Connection,
    provider_id: &str,
    catalog: &ProviderRecord,
) -> dim_agent_runtime::Result<()> {
    connection.execute("INSERT INTO providers(provider_id,version,record) VALUES (?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET version=excluded.version,record=excluded.record",
        params![provider_id, catalog.version, serde_json::to_string(catalog).map_err(storage)?]).map_err(storage)?;
    Ok(())
}
fn selection_scope(cwd: Option<&Path>) -> Result<String, HostError> {
    match cwd {
        None => Ok("global".into()),
        Some(path) if path.is_absolute() => Ok(format!(
            "workspace:{}",
            path.to_str()
                .ok_or_else(|| failure("Workspace path is not UTF-8"))?
        )),
        Some(_) => Err(failure("Workspace path must be absolute")),
    }
}
impl ProviderService {
    async fn account_available(
        self: &Arc<Self>,
        provider_id: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, HostError> {
        self.check(cancel)?;
        let authority = managed_authority(provider_id);
        if authority == CredentialAuthority::Static {
            return Ok(true);
        }
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let status = match authority {
                    CredentialAuthority::OpenaiCodex => owner.codex.status(&cancel).await,
                    CredentialAuthority::Grok => owner.grok.status(&cancel).await,
                    CredentialAuthority::Static => unreachable!(),
                };
                owner.check(&cancel)?;
                Ok(status.map_err(failure)?.authenticated)
            })
        })
        .await
    }

    async fn managed_credential(
        self: &Arc<Self>,
        authority: CredentialAuthority,
        rejected: Option<managed_oauth::CredentialLease>,
        cancel: &CancellationToken,
    ) -> Result<managed_oauth::CredentialLease, HostError> {
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let lease = match authority {
                    CredentialAuthority::OpenaiCodex => {
                        owner.codex.credential(rejected.as_ref(), &cancel).await
                    }
                    CredentialAuthority::Grok => {
                        owner.grok.credential(rejected.as_ref(), &cancel).await
                    }
                    CredentialAuthority::Static => unreachable!(),
                };
                owner.check(&cancel)?;
                lease.map_err(failure)
            })
        })
        .await
    }

    pub async fn catalog_for(
        self: &Arc<Self>,
        provider_id: String,
        cancel: &CancellationToken,
    ) -> Result<Option<ProviderCatalog>, HostError> {
        if self
            .oauth
            .contains(&provider_id, cancel)
            .await
            .map_err(failure)?
        {
            return self.catalog(provider_id, cancel).await;
        }
        let available = self.account_available(&provider_id, cancel).await?;
        let db = self.db.clone();
        let bundled = self.bundled.clone();
        self.owned(cancel, move |_| {
            Box::pin(async move {
                db.run(move |connection| {
                    Ok(read_static(connection, &provider_id)?
                        .map(|record| static_catalog(&bundled, &provider_id, &record, available)))
                })
                .await
                .map_err(failure)
            })
        })
        .await
    }
    async fn seed_bundled(self: &Arc<Self>) -> Result<(), HostError> {
        let mut seeds = bundled_seeds(&self.bundled)?;
        let db = self.db.clone();
        self.owned(&CancellationToken::new(), move |_| {
            Box::pin(async move {
                db.run(move |connection| {
                    let tx = connection.transaction().map_err(storage)?;
                    let rows = {
                        let mut query = tx
                            .prepare("SELECT provider_id,version,record FROM providers ")
                            .map_err(storage)?;
                        query
                            .query_map([], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, u32>(1)?,
                                    r.get::<_, String>(2)?,
                                ))
                            })
                            .map_err(storage)?
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(storage)?
                    };
                    for (id, version, body) in rows {
                        let definition = seeds.remove(&id);
                        let Ok(Some(mut previous)) = parse_static_row(&id, version, &body) else {
                            continue;
                        };
                        if previous.kind != StaticRecordKind::Builtin {
                            continue;
                        }
                        let mut record = match definition {
                            Some(definition)
                                if previous.definition_fingerprint
                                    != definition.definition_fingerprint =>
                            {
                                reseed_candidate(&previous, definition)
                            }
                            None if previous.enabled => {
                                previous.enabled = false;
                                previous
                            }
                            _ => continue,
                        };
                        if record.user_models.iter().any(|user| {
                            record
                                .models
                                .iter()
                                .any(|model| model.model_id == user.model_id)
                        }) {
                            continue;
                        }
                        record.version = version
                            .checked_add(1)
                            .ok_or_else(|| storage("Provider version overflow"))?;
                        validate_static_record(&id, &mut record).map_err(storage)?;
                        tx.execute(
                            "UPDATE providers SET version=?2,record=?3 WHERE provider_id=?1",
                            params![
                                id,
                                record.version,
                                serde_json::to_string(&record).map_err(storage)?
                            ],
                        )
                        .map_err(storage)?;
                    }
                    for (id, mut definition) in seeds {
                        validate_static_record(&id, &mut definition).map_err(storage)?;
                        tx.execute(
                            "INSERT INTO providers(provider_id,version,record) VALUES (?1,1,?2)",
                            params![id, serde_json::to_string(&definition).map_err(storage)?],
                        )
                        .map_err(storage)?;
                    }
                    tx.commit().map_err(storage)
                })
                .await
                .map_err(failure)
            })
        })
        .await
    }

    pub async fn list(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<ProviderList, HostError> {
        let db = self.db.clone();
        let bundled = self.bundled.clone();
        let mut list = self.owned(cancel, move |_| Box::pin(async move { db.run(move |connection| {
            let mut query = connection.prepare("SELECT provider_id,version,record FROM providers ORDER BY provider_id").map_err(storage)?;
            let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))).map_err(storage)?;
            let mut result = ProviderList { providers: Vec::new(), errors: Vec::new() };
            for row in rows {
                let (id, version, body) = row.map_err(storage)?;
                match parse_static_row(&id, version, &body) {
                    Ok(None)=>{},
                    Ok(Some(record)) => result.providers.push(static_summary(&bundled, id, record)),
                    Err(error) => result.errors.push(ProviderRecordError { provider_id: id, message: error.to_string() }),
                }
            }
            Ok(result)
        }).await.map_err(failure) })).await?;
        for provider in &mut list.providers {
            if provider.credential_authority != CredentialAuthority::Static {
                match self.account_available(&provider.provider_id, cancel).await {
                    Ok(available) => provider.credential_configured = available,
                    Err(error) => list.errors.push(ProviderRecordError {
                        provider_id: provider.provider_id.clone(),
                        message: error.to_string(),
                    }),
                }
            }
        }
        self.check(cancel)?;
        Ok(list)
    }

    /// Every model execution would accept, one snapshot per provider.
    pub async fn selectable_models(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<Vec<ModelCapabilitySnapshot>, HostError> {
        self.check(cancel)?;
        let mut providers: BTreeSet<String> = self
            .oauth
            .providers(cancel)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|provider| provider.provider_id)
            .collect();
        providers.extend(
            self.list(cancel)
                .await?
                .providers
                .into_iter()
                .map(|provider| provider.provider_id),
        );
        let mut models = Vec::new();
        for provider_id in providers {
            let catalog = match self.catalog_for(provider_id.clone(), cancel).await {
                Ok(Some(catalog)) => catalog,
                Ok(None) | Err(_) => continue,
            };
            for model in &catalog.models {
                let Some(model) = catalog.selectable(&model.id) else {
                    continue;
                };
                models.push(
                    capability_snapshot(
                        model,
                        ModelId {
                            provider_id: provider_id.clone(),
                            model_id: model.id.clone(),
                        },
                    )
                    .map_err(|error| failure(error.to_string()))?,
                );
            }
        }
        self.check(cancel)?;
        Ok(models)
    }

    pub async fn delete_static(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if self.oauth.contains(&provider_id, cancel).await? {
            return Err(ProviderError::Unsupported(
                "Managed Provider belongs to its account owner".into(),
            ));
        }
        let db = self.db.clone();
        self.owned(cancel, move |_| {
            Box::pin(async move {
                db.run(move |connection| {
                    Ok((|| -> Result<_, ProviderError> {
                        let tx = connection.transaction()?;
                        let mut record =
                            read_static(&tx, &provider_id)?.ok_or(ProviderError::NotFound)?;
                        if record.version != expected_version {
                            return Err(ProviderError::Conflict("Provider version changed".into()));
                        }
                        if record.kind == StaticRecordKind::Builtin {
                            record.enabled = false;
                            record.version =
                                record.version.checked_add(1).ok_or(ProviderError::Failed)?;
                            tx.execute(
                                "UPDATE providers SET version=?2,record=?3 WHERE provider_id=?1",
                                params![
                                    provider_id,
                                    record.version,
                                    serde_json::to_string(&record)?
                                ],
                            )?;
                        } else {
                            tx.execute(
                                "DELETE FROM provider_selections WHERE provider_id=?1",
                                [&provider_id],
                            )?;
                            tx.execute(
                                "DELETE FROM providers WHERE provider_id=?1",
                                [&provider_id],
                            )?;
                        }
                        tx.commit()?;
                        Ok(())
                    })())
                })
                .await?
            })
        })
        .await
    }

    fn check(&self, cancel: &CancellationToken) -> Result<(), HostError> {
        if self.stop.is_cancelled() {
            return Err(HostError::classified(
                HostErrorKind::OwnerClosed,
                "Provider is closed",
            ));
        }
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled("Provider operation cancelled".into()));
        }
        Ok(())
    }
    async fn owned<T: Send + 'static, E: From<HostError> + Send + 'static>(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        operation: impl FnOnce(CancellationToken) -> SharedFuture<'static, Result<T, E>>
        + Send
        + 'static,
    ) -> Result<T, E> {
        let task = {
            let _guard = self.admission.lock().unwrap();
            self.check(cancel).map_err(E::from)?;
            let caller = cancel.clone();
            let stop = self.stop.clone();
            self.tasks.spawn(async move {
                let cancel = stop.child_token();
                let mut operation = operation(cancel.clone());
                tokio::select! {
                    result = &mut operation => return result,
                    _ = caller.cancelled() => cancel.cancel(),
                    _ = stop.cancelled() => {},
                }
                operation.await
            })
        };
        let result = task
            .await
            .map_err(|_| E::from(failure("Provider worker failed")))?;
        if result.is_err() {
            self.check(cancel).map_err(E::from)?;
        }
        result
    }
    pub async fn catalog(
        self: &Arc<Self>,
        provider_id: String,
        cancel: &CancellationToken,
    ) -> Result<Option<ProviderCatalog>, HostError> {
        let account = self
            .oauth
            .status(&provider_id, cancel)
            .await
            .map_err(failure)?;
        let db = self.db.clone();
        let bundled = self.bundled.clone();
        self.owned(cancel, move |_| {
            Box::pin(async move {
                db.run(move |c| {
                    Ok(read_catalog(c, &provider_id)?
                        .filter(|r| account.account.is_some() && same_account(&r.account, &account))
                        .map(|r| r.catalog(&bundled)))
                })
                .await
                .map_err(failure)
            })
        })
        .await
    }
    pub async fn refresh(
        self: &Arc<Self>,
        provider_id: String,
        cancel: &CancellationToken,
    ) -> Result<ProviderCatalog, ProviderError> {
        if !self.oauth.contains(&provider_id, cancel).await? {
            return Err(ProviderError::Unsupported(
                "Managed Provider package is unavailable".into(),
            ));
        }
        let db = self.db.clone();
        let id = provider_id.clone();
        let expected_version = self
            .owned(cancel, move |_| {
                Box::pin(async move {
                    db.run(move |connection| {
                        Ok(read_catalog(connection, &id)?.map(|record| record.version))
                    })
                    .await
                    .map_err(ProviderError::from)
                })
            })
            .await?;
        let catalog = self.oauth.catalog(&provider_id, cancel).await?;
        let current = self.oauth.status(&provider_id, cancel).await?;
        if !same_account(&current, &catalog.account) {
            return Err(ProviderError::Conflict("Managed account changed".into()));
        }
        let db = self.db.clone();
        let bundled = self.bundled.clone();
        let id = provider_id.clone();
        let result = self
            .owned(cancel, move |_| {
                Box::pin(async move {
                    db.run(move |connection| {
                        Ok((|| -> Result<_, ProviderError> {
                            let tx = connection.transaction()?;
                            let old = read_catalog(&tx, &provider_id)?;
                            if old.as_ref().map(|record| record.version) != expected_version {
                                return Err(ProviderError::Conflict(
                                    "Provider version changed".into(),
                                ));
                            }
                            let version = old
                                .as_ref()
                                .map_or(0, |record| record.version)
                                .checked_add(1)
                                .ok_or(ProviderError::Failed)?;
                            let old = old
                                .filter(|record| same_account(&record.account, &catalog.account));
                            let (disabled_model_ids, model_config_overrides, model_preferences) =
                                old.map_or_else(
                                    || (Vec::new(), Vec::new(), BTreeMap::new()),
                                    |record| {
                                        (
                                            record.disabled_model_ids,
                                            record.model_config_overrides,
                                            record.model_preferences,
                                        )
                                    },
                                );
                            let record = ProviderRecord {
                                kind: ManagedRecordKind::Managed,
                                account: catalog.account,
                                models: catalog.models,
                                disabled_model_ids,
                                model_config_overrides,
                                model_preferences,
                                version,
                            };
                            save_catalog(&tx, &provider_id, &record)?;
                            tx.commit()?;
                            Ok(record.catalog(&bundled))
                        })())
                    })
                    .await?
                })
            })
            .await?;
        let settled = self.oauth.status(&id, cancel).await?;
        if !same_account(&current, &settled) {
            return Err(ProviderError::Conflict("Managed account changed".into()));
        }
        Ok(result)
    }
    pub async fn selection(
        self: &Arc<Self>,
        cwd: Option<&Path>,
        cancel: &CancellationToken,
    ) -> Result<Option<ProviderSelection>, HostError> {
        let scope = selection_scope(cwd)?;
        let db = self.db.clone();
        self.owned(cancel, move |_cancel| {
            Box::pin(async move {
                db.run(move |connection| {
                    connection
                        .query_row(
                            "SELECT provider_id, model_id, version FROM provider_selections WHERE scope=?1",
                            [scope],
                            |row| {
                                Ok(ProviderSelection {
                                    provider_id: row.get(0)?,
                                    model_id: row.get(1)?,
                                    version: row.get(2)?,
                                })
                            },
                        )
                        .optional()
                        .map_err(storage)
                })
                .await
                .map_err(failure)
            })
        })
        .await
    }
    pub async fn select(
        self: &Arc<Self>,
        cwd: Option<&Path>,
        target: ModelId,
        expected_version: Option<u32>,
        cancel: &CancellationToken,
    ) -> Result<ProviderSelection, ProviderError> {
        let scope = selection_scope(cwd)?;
        let catalog = self
            .catalog_for(target.provider_id.clone(), cancel)
            .await?
            .ok_or(ProviderError::NotFound)?;
        if !catalog
            .models
            .iter()
            .any(|model| model.id == target.model_id)
        {
            return Err(ProviderError::NotFound);
        }
        if catalog.selectable(&target.model_id).is_none() {
            let model = catalog
                .models
                .iter()
                .find(|model| model.id == target.model_id)
                .ok_or(ProviderError::NotFound)?;
            return Err(ProviderError::InvalidConfig(
                if model.missing_limits.is_empty() {
                    "Model is unavailable".into()
                } else {
                    undeclared_limits(&model.capabilities)
                },
            ));
        }
        let db = self.db.clone();
        self.owned(cancel, move |_| Box::pin(async move {
            db.run(move |connection| Ok((|| -> Result<_, ProviderError> {
                let tx = connection.transaction()?;
                let version: Option<u32> = tx.query_row("SELECT version FROM provider_selections WHERE scope=?1", [&scope], |row| row.get(0)).optional()?;
                if version != expected_version { return Err(ProviderError::Conflict("Selection version changed".into())); }
                let version = version.unwrap_or(0).checked_add(1).ok_or(ProviderError::Failed)?;
                tx.execute("INSERT INTO provider_selections(scope,version,provider_id,model_id) VALUES (?1,?2,?3,?4) ON CONFLICT(scope) DO UPDATE SET version=excluded.version,provider_id=excluded.provider_id,model_id=excluded.model_id",params![scope,version,target.provider_id,target.model_id])?;
                tx.commit()?;
                Ok(ProviderSelection { provider_id: target.provider_id, model_id: target.model_id, version })
            })())).await?
        })).await
    }
    pub async fn configuration(
        self: &Arc<Self>,
        explicit: Option<ModelId>,
        cwd: Option<&Path>,
        preferences: ModelPreferences,
        cancel: &CancellationToken,
    ) -> Result<ModelSelection, HostError> {
        self.check(cancel)?;
        let explicit_target = explicit.is_some();
        let mut candidates = vec![];
        if let Some(id) = explicit {
            candidates.push((id.provider_id, Some(id.model_id)));
        } else {
            for scope in [cwd, None] {
                if let Some(selection) = self.selection(scope, cancel).await? {
                    candidates.push((selection.provider_id, Some(selection.model_id)));
                }
            }
            candidates.extend(
                self.oauth
                    .providers(cancel)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| (p.provider_id, None)),
            );
            candidates.extend(
                self.list(cancel)
                    .await?
                    .providers
                    .into_iter()
                    .map(|p| (p.provider_id, None)),
            );
        }
        for (id, model_id) in candidates {
            let catalog = match self.catalog_for(id.clone(), cancel).await {
                Ok(Some(c)) => c,
                Ok(None) => continue,
                Err(_) if !explicit_target => continue,
                Err(e) => return Err(e),
            };
            let model = match model_id {
                Some(ref m) => catalog.selectable(m),
                None => catalog
                    .models
                    .iter()
                    .find(|m| catalog.selectable(&m.id).is_some()),
            };
            let Some(model) = model else { continue };
            let protocol = model
                .endpoint
                .ok_or_else(|| failure("Missing model protocol"))?;
            let snapshot = capability_snapshot(
                model,
                ModelId {
                    provider_id: id.clone(),
                    model_id: model.id.clone(),
                },
            )
            .map_err(|error| failure(error.to_string()))?;
            let config = preferences::configure(&preferences, &snapshot, protocol, &model.config)
                .map_err(failure)?;
            return Ok(ModelSelection {
                provider_id: id.clone(),
                model_id: model.id.clone(),
                capability_snapshot: Some(snapshot),
                config: Some(config),
                metadata: Some(dim_agent_types::RequestMetadata {
                    extensions: serde_json::Map::from_iter([(
                        "providerProtocol".into(),
                        serde_json::json!(protocol),
                    )]),
                    ..Default::default()
                }),
            });
        }
        Err(failure(if explicit_target {
            "Explicit model is unavailable"
        } else {
            "No available model"
        }))
    }

    /// Applies a preference patch to the adopted model without reselecting
    /// defaults. A cleared field takes the model's declared configuration.
    pub async fn apply_preferences(
        self: &Arc<Self>,
        current: &dim_agent_types::ModelConfiguration,
        update: &crate::protocol::ModelPreferenceUpdate,
        cancel: &CancellationToken,
    ) -> Result<ModelSelection, HostError> {
        self.check(cancel)?;
        let catalog = self
            .catalog_for(current.provider_id.clone(), cancel)
            .await?
            .ok_or_else(|| failure("Provider is unavailable"))?;
        let model = catalog
            .selectable(&current.model_id)
            .ok_or_else(|| failure("Model is unavailable"))?;
        let protocol = model
            .endpoint
            .ok_or_else(|| failure("Missing model protocol"))?;
        if current
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.extensions.get("providerProtocol"))
            .is_some_and(|adopted| adopted != &serde_json::json!(protocol))
        {
            return Err(HostError::classified(
                HostErrorKind::InvalidRequest,
                "Model route changed; select the model again",
            ));
        }
        let mut base = current.config.clone().unwrap_or_default();
        if update.clears_reasoning() {
            preferences::restore_reasoning(&mut base, &model.config, protocol);
        }
        if update.clears_service_tier() {
            preferences::restore_service_tier(&mut base, &model.config, protocol);
        }
        let snapshot = capability_snapshot(
            model,
            ModelId {
                provider_id: current.provider_id.clone(),
                model_id: current.model_id.clone(),
            },
        )
        .map_err(|error| failure(error.to_string()))?;
        let config = preferences::configure(&update.values(), &snapshot, protocol, &base)
            .map_err(failure)?;
        let unchanged = current
            .config
            .as_ref()
            .map_or(config == ModelConfig::default(), |current| {
                current == &config
            });
        Ok(ModelSelection {
            provider_id: current.provider_id.clone(),
            model_id: current.model_id.clone(),
            capability_snapshot: current.capability_snapshot.clone(),
            config: if unchanged {
                current.config.clone()
            } else {
                Some(config)
            },
            metadata: current.metadata.clone(),
        })
    }
}
struct ProductResolver(Arc<ProviderService>);
impl ResolveModelConnection for ProductResolver {
    fn resolve(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async move {
            self.0.check(&cancel)?;
            request
                .capability_snapshot
                .as_ref()
                .ok_or_else(|| {
                    HostError::classified(
                        HostErrorKind::InvalidRequest,
                        "Model capabilities are not adopted; select the model again",
                    )
                })?
                .validate_for(&request.provider_id, &request.model_id)
                .map_err(|message| HostError::classified(HostErrorKind::InvalidRequest, message))?;
            if !self
                .0
                .oauth
                .contains(&request.provider_id, &cancel)
                .await
                .map_err(failure)?
            {
                let provider_id = request.provider_id.clone();
                let model_id = request.model_id.clone();
                let db = self.0.db.clone();
                let record = self
                    .0
                    .owned(&cancel, move |_| {
                        Box::pin(async move {
                            db.run(move |connection| read_static(connection, &provider_id))
                                .await
                                .map_err(failure)
                        })
                    })
                    .await?
                    .ok_or_else(|| failure("Provider not found"))?;
                self.0.check(&cancel)?;
                let catalog = static_catalog(&self.0.bundled, &request.provider_id, &record, true);
                if catalog.selectable(&model_id).is_none() {
                    let message = catalog
                        .models
                        .iter()
                        .find(|model| model.id == model_id)
                        .filter(|model| !model.missing_limits.is_empty())
                        .map(|model| undeclared_limits(&model.capabilities))
                        .unwrap_or_else(|| "Model is unavailable".into());
                    return Err(failure(message));
                }
                let protocol = record.routes[&model_id];
                if record.driver_kind == "new-api-router"
                    && request
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.extensions.get("providerProtocol"))
                        .is_some_and(|adopted| adopted != &serde_json::json!(protocol))
                {
                    return Err(HostError::classified(
                        HostErrorKind::InvalidRequest,
                        "Model route changed; select the model again",
                    ));
                }
                if record.credential_authority != CredentialAuthority::Static {
                    let lease = self
                        .0
                        .managed_credential(record.credential_authority, None, &cancel)
                        .await?;
                    let recovered = managed_connection(
                        self.0.clone(),
                        record.credential_authority,
                        lease,
                        record.network,
                    );
                    return Ok(ResolvedModelConnection {
                        connection: recovered.connection,
                        recovery: Some(recovered.recovery),
                        failure_policy: None,
                    });
                }
                let credential = self
                    .0
                    .credential(
                        &request.provider_id,
                        &record.credential,
                        &record.auth,
                        &cancel,
                    )
                    .await?;
                let mut headers = record.headers;
                let mut bearer = None;
                match record.auth {
                    StaticAuth::Anonymous => {}
                    StaticAuth::Bearer => {
                        let ProviderSecret::Token(value) =
                            credential.ok_or_else(|| failure("Provider credential is required"))?
                        else {
                            return Err(failure("Invalid provider credential type"));
                        };
                        bearer = Some(value)
                    }
                    StaticAuth::ApiKey {
                        header_name,
                        prefix,
                    } => {
                        let ProviderSecret::Token(value) =
                            credential.ok_or_else(|| failure("Provider credential is required"))?
                        else {
                            return Err(failure("Invalid provider credential type"));
                        };
                        if header_name.eq_ignore_ascii_case("authorization") && prefix == "Bearer "
                        {
                            bearer = Some(value);
                        } else {
                            headers.insert(header_name, format!("{prefix}{value}"));
                        }
                    }
                    StaticAuth::CustomHeader { header_name } => {
                        let ProviderSecret::Token(value) =
                            credential.ok_or_else(|| failure("Provider credential is required"))?
                        else {
                            return Err(failure("Invalid provider credential type"));
                        };
                        headers.insert(header_name, value);
                    }
                    StaticAuth::AwsBedrock => {
                        let auth = match credential
                            .ok_or_else(|| failure("Provider credential is required"))?
                        {
                            ProviderSecret::Token(value) => BedrockAuth::Bearer(value),
                            ProviderSecret::Aws(credentials) => BedrockAuth::SigV4 {
                                credentials,
                                region: record
                                    .bedrock_region
                                    .ok_or_else(|| failure("Amazon Bedrock region is missing"))?,
                            },
                        };
                        return Ok(ResolvedModelConnection {
                            connection: ModelConnection::Bedrock(BedrockModelConnection {
                                base_url: record.base_url,
                                network: record.network,
                                headers,
                                auth,
                            }),
                            failure_policy: None,
                            recovery: None,
                        });
                    }
                }
                let mut connection =
                    drivers::connection(&record.driver_kind, protocol, &record.base_url, headers);
                if request.provider_id == "opencode-go" {
                    let session_id = request
                        .metadata
                        .as_ref()
                        .and_then(|m| m.session_id.as_deref())
                        .filter(|id| {
                            !id.is_empty()
                                && id.trim() == *id
                                && id.bytes().all(|b| (0x20..=0x7e).contains(&b))
                        })
                        .ok_or_else(|| failure("OpenCode Go requires a printable session ID"))?;
                    connection
                        .headers
                        .retain(|name, _| !name.eq_ignore_ascii_case("x-opencode-session"));
                    connection
                        .headers
                        .insert("x-opencode-session".into(), session_id.into());
                }
                connection.bearer = bearer;
                connection.network = record.network;
                return Ok(ResolvedModelConnection {
                    connection: ModelConnection::Http(connection),
                    failure_policy: None,
                    recovery: None,
                });
            }
            let catalog = self
                .0
                .catalog(request.provider_id.clone(), &cancel)
                .await?
                .ok_or_else(|| failure("No current managed catalog"))?;
            let model = catalog.selectable(&request.model_id).ok_or_else(|| {
                let message = catalog
                    .models
                    .iter()
                    .find(|model| model.id == request.model_id)
                    .filter(|model| !model.missing_limits.is_empty())
                    .map(|model| undeclared_limits(&model.capabilities))
                    .unwrap_or_else(|| "Model is unavailable".into());
                failure(message)
            })?;
            let lease = self
                .0
                .oauth
                .credential(&request.provider_id, &request.model_id, &cancel)
                .await
                .map_err(failure)?;
            if catalog.account.as_ref() != Some(&lease.account)
                || model.endpoint != Some(lease.protocol)
            {
                return Err(failure("Managed account or model route changed"));
            }
            let recovered = plugin_connection(self.0.clone(), lease);
            Ok(ResolvedModelConnection {
                connection: recovered.connection,
                recovery: Some(recovered.recovery),
                failure_policy: None,
            })
        })
    }
}
impl ResolveModelSelection for ProductResolver {
    fn resolve(
        &self,
        explicit: Option<ModelId>,
        cwd: Option<std::path::PathBuf>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelSelection> {
        Box::pin(async move {
            self.0
                .configuration(
                    explicit,
                    cwd.as_deref(),
                    ModelPreferences::default(),
                    &cancel,
                )
                .await
        })
    }
}
impl SelectableModels for ProductResolver {
    fn selectable(
        &self,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Vec<ModelCapabilitySnapshot>> {
        let owner = self.0.clone();
        Box::pin(async move { owner.selectable_models(&cancel).await })
    }
}
struct ManagedRecovery {
    owner: Arc<ProviderService>,
    authority: CredentialAuthority,
    lease: managed_oauth::CredentialLease,
    network: ModelNetwork,
}

fn managed_connection(
    owner: Arc<ProviderService>,
    authority: CredentialAuthority,
    lease: managed_oauth::CredentialLease,
    network: ModelNetwork,
) -> RecoveredModelConnection {
    let mut headers = HashMap::new();
    let (protocol, preset) = match authority {
        CredentialAuthority::OpenaiCodex => {
            headers.insert("OAI-Product-Sku".into(), "codex".into());
            headers.insert("originator".into(), "codex-tui".into());
            headers.insert(
                "User-Agent".into(),
                "codex-tui/0.144.1 (Ubuntu 22.4.0; x86_64) xterm-256color (codex-tui; 0.144.1)"
                    .into(),
            );
            if let Some(account_id) = &lease.account_id {
                headers.insert("ChatGPT-Account-ID".into(), account_id.clone());
            }
            (ModelProtocol::OpenaiResponses, Some("openai-codex".into()))
        }
        CredentialAuthority::Grok => (ModelProtocol::OpenaiCompatible, None),
        CredentialAuthority::Static => unreachable!(),
    };
    RecoveredModelConnection {
        connection: ModelConnection::Http(HttpModelConnection {
            protocol,
            base_url: lease.relay_base_url.clone(),
            headers,
            network: network.clone(),
            bearer: Some(lease.bearer().into()),
            api_key: None,
            preset,
        }),
        recovery: Arc::new(ManagedRecovery {
            owner,
            authority,
            lease,
            network,
        }),
    }
}

impl RecoverModelConnection for ManagedRecovery {
    fn rejection(&self, error: &HostError, phase: CredentialRecoveryPhase) -> CredentialRejection {
        oauth_rejection(error, phase)
    }
    fn recover(&self, cancel: CancellationToken) -> HostFuture<'_, RecoveredModelConnection> {
        Box::pin(async move {
            let lease = self
                .owner
                .managed_credential(self.authority, Some(self.lease.clone()), &cancel)
                .await?;
            Ok(managed_connection(
                self.owner.clone(),
                self.authority,
                lease,
                self.network.clone(),
            ))
        })
    }
    fn finish_rejected(&self, cancel: CancellationToken) -> HostFuture<'_, ()> {
        let owner = self.owner.clone();
        let authority = self.authority;
        let lease = self.lease.clone();
        Box::pin(async move {
            self.owner
                .owned(&cancel, move |cancel| {
                    Box::pin(async move {
                        let result = match authority {
                            CredentialAuthority::OpenaiCodex => {
                                owner.codex.finish_rejected(&lease, &cancel).await
                            }
                            CredentialAuthority::Grok => {
                                owner.grok.finish_rejected(&lease, &cancel).await
                            }
                            CredentialAuthority::Static => unreachable!(),
                        };
                        owner.check(&cancel)?;
                        result.map_err(failure)
                    })
                })
                .await
        })
    }
}

struct PluginRecovery {
    owner: Arc<ProviderService>,
    lease: CredentialLease,
}
fn plugin_connection(
    owner: Arc<ProviderService>,
    lease: CredentialLease,
) -> RecoveredModelConnection {
    RecoveredModelConnection {
        connection: ModelConnection::Http(HttpModelConnection {
            protocol: lease.protocol,
            base_url: lease.base_url.clone(),
            headers: lease.headers.clone(),
            network: ModelNetwork::default(),
            bearer: Some(lease.bearer.clone()),
            api_key: None,
            preset: None,
        }),
        recovery: Arc::new(PluginRecovery { owner, lease }),
    }
}
impl RecoverModelConnection for PluginRecovery {
    fn rejection(&self, error: &HostError, phase: CredentialRecoveryPhase) -> CredentialRejection {
        oauth_rejection(error, phase)
    }
    fn recover(&self, cancel: CancellationToken) -> HostFuture<'_, RecoveredModelConnection> {
        Box::pin(async move {
            self.owner.check(&cancel)?;
            let lease = self
                .owner
                .oauth
                .recover(&self.lease, &cancel)
                .await
                .map_err(failure)?;
            self.owner.check(&cancel)?;
            Ok(plugin_connection(self.owner.clone(), lease))
        })
    }
    fn finish_rejected(&self, cancel: CancellationToken) -> HostFuture<'_, ()> {
        Box::pin(async move {
            self.owner.check(&cancel)?;
            self.owner
                .oauth
                .finish_rejected(&self.lease, &cancel)
                .await
                .map_err(failure)
        })
    }
}

fn oauth_rejection(error: &HostError, phase: CredentialRecoveryPhase) -> CredentialRejection {
    if !matches!(
        error,
        HostError::Failed {
            kind: HostErrorKind::Http {
                status: Some(401),
                ..
            },
            ..
        }
    ) {
        return CredentialRejection::Unrelated;
    }
    match phase {
        CredentialRecoveryPhase::Initial => CredentialRejection::Recoverable,
        CredentialRecoveryPhase::Recovered => CredentialRejection::Terminal,
    }
}

pub struct ProviderPlugin {
    db: Key<SqliteStorage>,
    oauth: Key<ManagedProviders>,
    codex: Key<OpenAICodexOAuthService>,
    grok: Key<GrokOAuthService>,
    /// ProviderSecretHost only when a plugin supplies it.
    secret_host: Option<Key<ProviderSecretHost>>,
    host: Key<ModelHost>,
    methods: Key<crate::methods::ManagementMethods>,
}
impl ProviderPlugin {
    /// The Provider part without a ProviderSecretHost.
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Self::keyed(names, false)
    }
    /// The supplying plugin must provide ProviderSecretHost before activation.
    pub fn with_secret_host(names: &Names<'_>) -> rutis::Result<Self> {
        Self::keyed(names, true)
    }
    fn keyed(names: &Names<'_>, secret_host: bool) -> rutis::Result<Self> {
        Ok(Self {
            db: names.key()?,
            oauth: names.key()?,
            codex: names.key()?,
            grok: names.key()?,
            secret_host: secret_host.then(|| names.key()).transpose()?,
            host: names.key()?,
            methods: names.key()?,
        })
    }
}
impl DynPlugin for ProviderPlugin {
    fn name(&self) -> &str {
        "Provider"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut keys = vec![
            self.db.erase(),
            self.oauth.erase(),
            self.codex.erase(),
            self.grok.erase(),
            self.host.erase(),
            self.methods.erase(),
        ];
        keys.extend(self.secret_host.as_ref().map(Key::erase));
        keys
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let db = ctx.require_at(&self.db)?;
            let oauth = ctx.require_at(&self.oauth)?;
            let codex = ctx.require_at(&self.codex)?;
            let grok = ctx.require_at(&self.grok)?;
            let secret_host = match &self.secret_host {
                Some(key) => Some(ctx.require_at(key)?),
                None => None,
            };
            let host = ctx.require_at(&self.host)?;
            let methods = ctx.require_at(&self.methods)?;
            let service =
                Arc::new(ProviderService {
                    db,
                    oauth,
                    codex,
                    grok,
                    secret_host,
                    bundled: Arc::new(BundledCatalog::load().map_err(|error| {
                        rutis::Error::from(ErrorKind::PluginFailed(error.into()))
                    })?),
                    client: remote::client(&ModelNetwork::default()).map_err(|error| {
                        rutis::Error::from(ErrorKind::PluginFailed(error.to_string().into()))
                    })?,
                    stop: CancellationToken::new(),
                    tasks: TaskTracker::new(),
                    admission: Mutex::new(()),
                });
            let owner = service.clone();
            ctx.effect(move || async move {
                {
                    let _guard = owner.admission.lock().unwrap();
                    owner.stop.cancel();
                    owner.tasks.close();
                }
                owner.tasks.wait().await;
                Ok(())
            })?;
            service.seed_bundled().await.map_err(|error| {
                rutis::Error::from(ErrorKind::PluginFailed(error.to_string().into()))
            })?;
            let resolver = Arc::new(ProductResolver(service.clone()));
            ctx.provide(ProviderConnectionResolver(resolver.clone()))?;
            ctx.provide(ModelSelectionHost(resolver.clone()))?;
            ctx.provide(SelectableModelsHost(resolver))?;
            let auxiliary = Arc::new(auxiliary::AuxiliaryModel {
                owner: service.clone(),
                host,
            });
            ctx.provide(AuxiliaryModelHost(auxiliary.clone()))?;
            ctx.provide_at(
                rutis_bridge::session::host_key("auxiliaryModel"),
                auxiliary as Arc<dyn rutis_bridge::session::HostDispatch>,
            )?;
            management::provide(ctx, &methods, service.clone())?;
            ctx.provide_as::<ProviderService>(service)?;
            Ok(Effect::Done)
        })
    }
}

/// The model protocol recorded on an adopted selection, when it carries a
/// readable one. For readback only: the route guards compare the recorded
/// value itself, so an unreadable one is rejected rather than ignored.
pub(crate) fn provider_protocol(
    metadata: Option<&dim_agent_types::RequestMetadata>,
) -> Option<ModelProtocol> {
    metadata?
        .extensions
        .get("providerProtocol")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// The preferences an adopted configuration sends, derived from the
/// configuration and the protocol recorded beside it. Absent when either is.
pub(crate) fn adopted_selection_preferences(
    config: Option<&ModelConfig>,
    metadata: Option<&dim_agent_types::RequestMetadata>,
) -> Option<ModelPreferences> {
    dim_agent_capabilities::host::adopted_preferences(config?, provider_protocol(metadata)?)
}
