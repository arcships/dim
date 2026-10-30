//! Connection admission preserves omitted secrets and commits one partial update.
use super::*;
use crate::model_catalog::FieldUpdate;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionInput {
    pub driver_kind: String,
    pub auth: StaticAuth,
    pub base_url: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub network: Option<ModelNetwork>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub credential: Option<CredentialUpdate>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProvider {
    pub provider_id: String,
    pub display_name: String,
    pub connection: ConnectionInput,
    pub models: Vec<ModelEntry>,
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProbeProvider {
    New {
        #[serde(rename = "providerId")]
        provider_id: String,
        connection: ConnectionInput,
    },
    Existing {
        #[serde(rename = "providerId")]
        provider_id: String,
        connection: ProviderPatch,
    },
}

#[derive(Debug, Serialize)]
pub struct ProbeResult {
    pub models: Vec<ModelEntry>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPatch {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub base_url: Option<String>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub enabled: Option<bool>,
    /// `{"action": "keep"}` is the boundary's "no change", the same as an
    /// omitted credential, so `is_empty`, `changes_endpoint` and `apply` never
    /// read a kept credential as a change.
    #[serde(default, deserialize_with = "credential_change")]
    pub credential: Option<CredentialUpdate>,
    #[serde(default, deserialize_with = "header_updates")]
    pub headers: Option<Vec<HeaderUpdate>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub network: Option<NetworkPatch>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum HeaderUpdate {
    Set { name: String, value: String },
    Remove { name: String },
}

fn header_updates<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<HeaderUpdate>>, D::Error> {
    let updates = Vec::<HeaderUpdate>::deserialize(deserializer)?;
    let mut names = HashSet::new();
    for update in &updates {
        let name = match update {
            HeaderUpdate::Set { name, .. } | HeaderUpdate::Remove { name } => name,
        };
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| serde::de::Error::custom("Invalid HTTP header name"))?;
        if !names.insert(name) {
            return Err(serde::de::Error::custom(
                "Duplicate or conflicting header operations",
            ));
        }
        if let HeaderUpdate::Set { value, .. } = update {
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| serde::de::Error::custom("Invalid HTTP header value"))?;
        }
    }
    Ok(Some(updates))
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPatch {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub proxy: Option<FieldUpdate<String>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub timeout_ms: Option<FieldUpdate<std::num::NonZeroU64>>,
}

/// Reads a patch's credential, where a kept one is no change: the field is
/// `None` for `{"action": "keep"}` and for an omitted credential alike.
fn credential_change<'de, D>(deserializer: D) -> Result<Option<CredentialUpdate>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(optional_non_null(deserializer)?.filter(|update| !matches!(update, CredentialUpdate::Keep)))
}

fn credential_update(
    previous: Option<StaticCredential>,
    update: Option<CredentialUpdate>,
) -> Result<Option<StaticCredential>, ProviderError> {
    Ok(match update {
        None | Some(CredentialUpdate::Keep) => previous,
        Some(CredentialUpdate::Clear) => None,
        Some(CredentialUpdate::Set { value }) => {
            if value.is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "Credential must not be empty".into(),
                ));
            }
            Some(StaticCredential::Inline(ProviderSecret::Token(value)))
        }
        Some(CredentialUpdate::SetAws { value }) => {
            Some(StaticCredential::Inline(ProviderSecret::Aws(value)))
        }
        Some(CredentialUpdate::SetRef { reference }) => {
            Some(StaticCredential::Reference { reference })
        }
    })
}

impl ConnectionInput {
    pub(super) fn record(
        self,
        display_name: String,
        models: Vec<ModelEntry>,
        enabled: bool,
    ) -> Result<StaticProviderRecord, ProviderError> {
        if !drivers::custom_drivers().any(|driver| driver == self.driver_kind) {
            return Err(ProviderError::Unsupported(
                "Driver cannot create a custom Provider".into(),
            ));
        }
        let credential = credential_update(None, self.credential)?;
        if enabled && self.auth != StaticAuth::Anonymous && credential.is_none() {
            return Err(ProviderError::AuthRequired);
        }
        Ok(StaticProviderRecord {
            kind: StaticRecordKind::Custom,
            display_name,
            driver_kind: self.driver_kind,
            auth: self.auth,
            credential_authority: CredentialAuthority::Static,
            base_url: self.base_url,
            network: self.network.unwrap_or_default(),
            headers: self.headers.unwrap_or_default(),
            credential,
            models,
            remote_models: false,
            endpoint_types: BTreeMap::new(),
            routes: HashMap::new(),
            bedrock_region: None,
            model_config_overrides: Vec::new(),
            user_models: Vec::new(),
            model_preferences: BTreeMap::new(),
            endpoint_overrides: BTreeMap::new(),
            disabled_model_ids: Vec::new(),
            enabled,
            definition_fingerprint: None,
            version: 1,
        })
    }
}

impl ProviderPatch {
    fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.base_url.is_none()
            && self.enabled.is_none()
            && self.credential.is_none()
            && self.headers.as_ref().is_none_or(Vec::is_empty)
            && self
                .network
                .as_ref()
                .is_none_or(|network| network.proxy.is_none() && network.timeout_ms.is_none())
    }

    /// Whether the patch changes the endpoint or the account the record
    /// reaches: `baseUrl`, `credential` or non-empty `headers`, which can carry
    /// the credential or an organisation route. A `network`-only change (a
    /// proxy or a timeout) changes how the endpoint is reached, not what it
    /// serves, so it saves alone and never needs a listing.
    fn changes_endpoint(&self) -> bool {
        self.base_url.is_some()
            || self.credential.is_some()
            || self
                .headers
                .as_ref()
                .is_some_and(|headers| !headers.is_empty())
    }

    pub(super) fn apply(self, record: &mut StaticProviderRecord) -> Result<(), ProviderError> {
        if let Some(name) = self.display_name {
            if record.kind != StaticRecordKind::Custom {
                return Err(ProviderError::Unsupported(
                    "Builtin Provider name is read-only".into(),
                ));
            }
            record.display_name = name;
        }
        if record.credential_authority != CredentialAuthority::Static
            && (self.base_url.is_some() || self.headers.is_some() || self.credential.is_some())
        {
            return Err(ProviderError::Unsupported(
                "Account connection belongs to its credential owner".into(),
            ));
        }
        if let Some(base_url) = self.base_url {
            record.base_url = base_url;
        }
        if let Some(enabled) = self.enabled {
            record.enabled = enabled;
        }
        record.credential = credential_update(record.credential.take(), self.credential)?;
        if let Some(updates) = self.headers {
            for update in updates {
                let name = match &update {
                    HeaderUpdate::Set { name, .. } | HeaderUpdate::Remove { name } => name,
                };
                record
                    .headers
                    .retain(|stored, _| !stored.eq_ignore_ascii_case(name));
                if let HeaderUpdate::Set { name, value } = update {
                    record.headers.insert(name, value);
                }
            }
        }
        if let Some(network) = self.network {
            if let Some(proxy) = network.proxy {
                record.network.proxy = match proxy {
                    FieldUpdate::Set { value } => Some(value),
                    FieldUpdate::Clear => None,
                };
            }
            if let Some(timeout) = network.timeout_ms {
                record.network.timeout_ms = match timeout {
                    FieldUpdate::Set { value } => Some(value.get()),
                    FieldUpdate::Clear => None,
                };
            }
        }
        Ok(())
    }
}

impl ProviderService {
    pub async fn probe(
        self: &Arc<Self>,
        input: ProbeProvider,
        cancel: &CancellationToken,
    ) -> Result<ProbeResult, ProviderError> {
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let (id, mut record) = match input {
                    ProbeProvider::New {
                        provider_id,
                        connection,
                    } => {
                        if !uuid::Uuid::parse_str(&provider_id)
                            .is_ok_and(|id| id.to_string() == provider_id)
                        {
                            return Err(ProviderError::InvalidConfig(
                                "New Provider ID must be a canonical lowercase UUID".into(),
                            ));
                        }
                        (
                            provider_id,
                            connection.record("Unsaved Provider".into(), Vec::new(), false)?,
                        )
                    }
                    ProbeProvider::Existing {
                        provider_id,
                        connection,
                    } => {
                        if owner.oauth.contains(&provider_id, &cancel).await? {
                            return Err(ProviderError::Unsupported(
                                "Managed Provider connection belongs to its account owner".into(),
                            ));
                        }
                        let id = provider_id.clone();
                        let mut record = owner
                            .db
                            .run(move |db| read_static(db, &id))
                            .await?
                            .ok_or(ProviderError::NotFound)?;
                        connection.apply(&mut record)?;
                        (provider_id, record)
                    }
                };
                validate_static_record(&id, &mut record)
                    .map_err(|error| ProviderError::InvalidConfig(error.to_string()))?;
                Ok(ProbeResult {
                    models: owner.discover(&id, &record, &cancel).await?,
                })
            })
        })
        .await
    }

    pub async fn create(
        self: &Arc<Self>,
        input: CreateProvider,
        cancel: &CancellationToken,
    ) -> Result<ManagementProvider, ProviderError> {
        if !uuid::Uuid::parse_str(&input.provider_id)
            .is_ok_and(|id| id.to_string() == input.provider_id)
        {
            return Err(ProviderError::InvalidConfig(
                "New Provider ID must be a canonical lowercase UUID".into(),
            ));
        }
        if self.oauth.contains(&input.provider_id, cancel).await? {
            return Err(ProviderError::Conflict("Provider ID already exists".into()));
        }
        let mut record =
            input
                .connection
                .record(input.display_name, input.models, input.enabled)?;
        validate_static_record(&input.provider_id, &mut record)
            .map_err(|error| ProviderError::InvalidConfig(error.to_string()))?;
        let provider_id = input.provider_id;
        let id = provider_id.clone();
        let db = self.db.clone();
        let record = self.owned(cancel, move |_| Box::pin(async move {
            db.run(move |connection| Ok((|| -> Result<_, ProviderError> {
                let tx = connection.transaction()?;
                if tx.query_row("SELECT 1 FROM providers WHERE provider_id=?1", [&id], |_| Ok(())).optional()?.is_some() {
                    return Err(ProviderError::Conflict("Provider ID already exists".into()));
                }
                tx.execute("INSERT INTO providers(provider_id,version,record) VALUES (?1,?2,?3)", params![id, record.version, serde_json::to_string(&record)?])?;
                tx.commit()?;
                Ok(record)
            })())).await?
        })).await?;
        self.static_management(provider_id, record, cancel).await
    }

    pub async fn update(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        patch: ProviderPatch,
        cancel: &CancellationToken,
    ) -> Result<ManagementProvider, ProviderError> {
        if self.oauth.contains(&provider_id, cancel).await? {
            return Err(ProviderError::Unsupported(
                "Managed Provider configuration belongs to its account owner".into(),
            ));
        }
        if patch.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Provider patch must not be empty".into(),
            ));
        }
        let (record, models_listed) = if patch.changes_endpoint() {
            self.update_connection(provider_id.clone(), expected_version, patch, cancel)
                .await?
        } else {
            let record = self
                .mutate_static(
                    provider_id.clone(),
                    expected_version,
                    move |record, _| patch.apply(record),
                    cancel,
                )
                .await?;
            (record, None)
        };
        let mut provider = self.static_management(provider_id, record, cancel).await?;
        provider.models_listed = models_listed;
        Ok(provider)
    }

    /// Applies a patch that changes the endpoint and, when the record's models
    /// are its own, replaces them with the new endpoint's listing in the same
    /// compare-and-set write. A failed listing fails the update, so a
    /// re-pointing never leaves the old endpoint's models on the new one.
    async fn update_connection(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        patch: ProviderPatch,
        cancel: &CancellationToken,
    ) -> Result<(StaticProviderRecord, Option<ModelsListed>), ProviderError> {
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let id = provider_id.clone();
                let record = owner
                    .db
                    .run(move |db| read_static(db, &id))
                    .await
                    .map_err(ProviderError::from)?
                    .ok_or(ProviderError::NotFound)?;
                if record.version != expected_version {
                    return Err(ProviderError::Conflict(
                        "Provider version changed; reread before retrying".into(),
                    ));
                }
                let expected = serde_json::to_value(&record).map_err(ProviderError::from)?;
                let mut admitted = record.clone();
                patch.apply(&mut admitted)?;
                if admitted.enabled
                    && admitted.credential_authority == CredentialAuthority::Static
                    && admitted.auth != StaticAuth::Anonymous
                    && admitted.credential.is_none()
                {
                    return Err(ProviderError::AuthRequired);
                }
                validate_static_record(&provider_id, &mut admitted)
                    .map_err(|error| ProviderError::InvalidConfig(error.to_string()))?;
                let bundled_models =
                    owner.bundled.provider(&provider_id).is_some() && !admitted.remote_models;
                let own_models =
                    !bundled_models && owner.remote_model_list(&provider_id, &admitted);
                let (models, models_listed) = if !admitted.enabled {
                    // A re-pointed disabled record drops the old endpoint's
                    // models in this same write, without a request: they are
                    // not the new endpoint's, and the empty, listable catalog
                    // is what the picker refills once the record is enabled.
                    (own_models.then(Vec::new), ModelsListed::Disabled)
                } else if bundled_models {
                    // The bundled catalog defines these models; a later
                    // bundled update must still reach the record.
                    (None, ModelsListed::Bundled)
                } else if !own_models {
                    (None, ModelsListed::Unsupported)
                } else {
                    let models = owner.discover(&provider_id, &admitted, &cancel).await?;
                    (Some(models), ModelsListed::Listed)
                };
                if let Some(models) = &models {
                    reject_user_model_conflicts(&admitted, models)?;
                }
                if let Some(models) = models {
                    admitted.models = models;
                    admitted.remote_models = true;
                }
                admitted.version = admitted
                    .version
                    .checked_add(1)
                    .ok_or_else(|| failure("Provider version overflow"))?;
                validate_static_record(&provider_id, &mut admitted)?;
                owner.check(&cancel)?;
                owner
                    .db
                    .run(move |db| {
                        Ok((|| -> Result<_, ProviderError> {
                            let tx = db.transaction()?;
                            let current = read_static(&tx, &provider_id)?
                                .map(serde_json::to_value)
                                .transpose()?;
                            if current.as_ref() != Some(&expected) {
                                return Err(ProviderError::Conflict(
                                    "Provider version or admitted configuration changed".into(),
                                ));
                            }
                            tx.execute(
                                "UPDATE providers SET version=?2,record=?3 WHERE provider_id=?1",
                                params![
                                    provider_id,
                                    admitted.version,
                                    serde_json::to_string(&admitted)?
                                ],
                            )?;
                            tx.commit()?;
                            Ok((admitted, Some(models_listed)))
                        })())
                    })
                    .await?
            })
        })
        .await
    }
}
