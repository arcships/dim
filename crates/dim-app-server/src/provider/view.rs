//! Safe edit snapshots are projected from one admitted Provider record.
use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementView {
    pub drivers: Vec<ManagementDriver>,
    pub providers: Vec<ManagementProvider>,
    pub errors: Vec<ProviderRecordError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementDriver {
    pub driver_kind: &'static str,
    pub auth_types: Vec<&'static str>,
    pub remote_model_list: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementProvider {
    pub provider_id: String,
    pub display_name: String,
    pub source: &'static str,
    pub credential_authority: &'static str,
    pub version: Option<u32>,
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<ManagementAccount>,
    pub actions: Vec<&'static str>,
    pub connection: Option<ManagementConnection>,
    pub models: Option<Vec<ManagementModel>>,
    pub preferences: BTreeMap<String, ModelPreferences>,
    /// What an update did with the record's models; absent from the reads and
    /// from a patch that did not change `baseUrl`, `credential` or `headers`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_listed: Option<ModelsListed>,
}

/// What `provider.update` did with the record's models when the patch changed
/// the endpoint or the account it reaches.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ModelsListed {
    /// It listed the new endpoint's models and stored them in the same write
    /// as the endpoint.
    Listed,
    /// The record keeps its models, which are the bundled catalog's.
    Bundled,
    /// The driver cannot list models, so the connection was saved alone.
    Unsupported,
    /// The record is disabled, so nothing was listed.
    Disabled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementAccount {
    pub namespace: &'static str,
    pub login_methods: Vec<String>,
    pub state: AccountState,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AccountState {
    Native(managed_oauth::AuthStatus),
    Package(AccountSnapshot),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementConnection {
    pub driver_kind: String,
    pub auth: StaticAuth,
    pub base_url: String,
    pub credential_configured: bool,
    pub header_names: Vec<String>,
    pub proxy_url: Option<String>,
    pub proxy_auth_configured: bool,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementModel {
    pub model: ModelEntry,
    pub source: &'static str,
    pub enabled: bool,
    pub endpoint: Option<ModelProtocol>,
    pub unavailable_reason: Option<String>,
    pub actions: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_override: Option<NewApiEndpoint>,
}

impl ProviderService {
    pub(super) fn remote_model_list(
        &self,
        provider_id: &str,
        record: &StaticProviderRecord,
    ) -> bool {
        super::can_list_models(&self.bundled, provider_id, record)
    }

    pub(super) async fn static_management(
        &self,
        provider_id: String,
        record: StaticProviderRecord,
        cancel: &CancellationToken,
    ) -> Result<ManagementProvider, ProviderError> {
        let (authority, account, available) = match record.credential_authority {
            CredentialAuthority::Static => ("static", None, true),
            authority => {
                let (namespace, method, state) = match authority {
                    CredentialAuthority::OpenaiCodex => {
                        ("account.codex", "browser", self.codex.status(cancel).await?)
                    }
                    CredentialAuthority::Grok => {
                        ("account.grok", "device", self.grok.status(cancel).await?)
                    }
                    CredentialAuthority::Static => unreachable!(),
                };
                let available = state.authenticated;
                (
                    if authority == CredentialAuthority::OpenaiCodex {
                        "openai_codex"
                    } else {
                        "grok"
                    },
                    Some(ManagementAccount {
                        namespace,
                        login_methods: vec![method.into()],
                        state: AccountState::Native(state),
                    }),
                    available,
                )
            }
        };
        let mut actions = vec![
            "update",
            "delete",
            "models.edit",
            "models.reset",
            "models.set_enabled",
            "preferences.update",
        ];
        if authority == "static" {
            actions.extend(["models.create", "models.delete"]);
            if record.enabled && self.remote_model_list(&provider_id, &record) {
                actions.push("refresh");
            }
        } else {
            actions.push("login");
            if available {
                actions.push("logout");
            }
        }
        let catalog = static_models(&self.bundled, &provider_id, &record, available);
        let models = static_entries(&self.bundled, &provider_id, &record)
            .into_iter()
            .zip(catalog)
            .map(|(model, effective)| {
                let user = record
                    .user_models
                    .iter()
                    .any(|entry| entry.model_id == model.model_id);
                let overridden = record
                    .model_config_overrides
                    .iter()
                    .any(|entry| entry.model_id == model.model_id);
                let mut actions = vec!["set_enabled", "edit"];
                if user {
                    actions.push("delete");
                } else if overridden {
                    actions.push("reset");
                }
                ManagementModel {
                    enabled: !record.disabled_model_ids.contains(&model.model_id),
                    endpoint_override: record.endpoint_overrides.get(&model.model_id).copied(),
                    model,
                    source: if user {
                        "user"
                    } else if overridden {
                        "override"
                    } else {
                        "default"
                    },
                    endpoint: effective.endpoint,
                    unavailable_reason: effective.unavailable_reason,
                    actions,
                }
            })
            .collect();
        let (proxy_url, proxy_auth_configured) = match &record.network.proxy {
            Some(proxy) => {
                let mut url = reqwest::Url::parse(proxy).map_err(|_| ProviderError::Failed)?;
                let authenticated = !url.username().is_empty() || url.password().is_some();
                url.set_username("").map_err(|_| ProviderError::Failed)?;
                url.set_password(None).map_err(|_| ProviderError::Failed)?;
                (Some(url.to_string()), authenticated)
            }
            None => (None, false),
        };
        let mut header_names: Vec<_> = record.headers.into_keys().collect();
        header_names.sort();
        Ok(ManagementProvider {
            provider_id,
            display_name: record.display_name,
            source: if record.kind == StaticRecordKind::Builtin {
                "builtin"
            } else {
                "custom"
            },
            credential_authority: authority,
            version: Some(record.version),
            enabled: Some(record.enabled),
            account,
            actions,
            models: Some(models),
            preferences: record.model_preferences,
            connection: Some(ManagementConnection {
                driver_kind: record.driver_kind,
                auth: record.auth,
                base_url: record.base_url,
                credential_configured: if authority == "static" {
                    record.credential.is_some()
                } else {
                    available
                },
                header_names,
                proxy_url,
                proxy_auth_configured,
                timeout_ms: record.network.timeout_ms,
            }),
            models_listed: None,
        })
    }

    pub(super) fn managed_management(
        descriptor: crate::managed_provider::ProviderDescriptor,
        account: AccountSnapshot,
        record: Option<ProviderRecord>,
    ) -> ManagementProvider {
        let unavailable = if account.account.is_none() {
            Some("account_required")
        } else if record
            .as_ref()
            .is_some_and(|record| !same_account(&record.account, &account))
        {
            Some("account_changed")
        } else {
            None
        };
        let mut actions = vec!["login", "usage"];
        if account.account.is_some() {
            actions.extend(["logout", "refresh"]);
        }
        if record.is_some() && unavailable.is_none() {
            actions.extend([
                "models.edit",
                "models.reset",
                "models.set_enabled",
                "preferences.update",
            ]);
        }
        let version = record.as_ref().map(|record| record.version);
        let (models, preferences) = match record {
            Some(record) => {
                let models = record
                    .models
                    .into_iter()
                    .map(|source| {
                        let replacement = record
                            .model_config_overrides
                            .iter()
                            .find(|entry| entry.model_id == source.entry.model_id);
                        let mut actions = if unavailable.is_none() {
                            vec!["set_enabled", "edit"]
                        } else {
                            Vec::new()
                        };
                        if replacement.is_some() && unavailable.is_none() {
                            actions.push("reset");
                        }
                        ManagementModel {
                            enabled: !record.disabled_model_ids.contains(&source.entry.model_id),
                            model: replacement.cloned().unwrap_or(source.entry),
                            source: if replacement.is_some() {
                                "override"
                            } else {
                                "default"
                            },
                            endpoint: Some(source.protocol),
                            unavailable_reason: unavailable
                                .map(str::to_owned)
                                .or(source.unavailable_reason),
                            actions,
                            endpoint_override: None,
                        }
                    })
                    .collect();
                (Some(models), record.model_preferences)
            }
            None => (None, BTreeMap::new()),
        };
        ManagementProvider {
            provider_id: descriptor.provider_id,
            display_name: descriptor.display_name,
            source: "managed",
            credential_authority: "account",
            version,
            enabled: None,
            account: Some(ManagementAccount {
                namespace: "account",
                login_methods: descriptor.login_methods,
                state: AccountState::Package(account),
            }),
            actions,
            connection: None,
            models,
            preferences,
            models_listed: None,
        }
    }

    pub async fn management(
        self: &Arc<Self>,
        provider_id: Option<String>,
        cancel: &CancellationToken,
    ) -> Result<ManagementView, ProviderError> {
        let db = self.db.clone();
        let target = provider_id.clone();
        let records = self.owned(cancel, move |_| Box::pin(async move {
            db.run(move |connection| {
                let mut query = connection.prepare("SELECT provider_id,version,record FROM providers WHERE ?1 IS NULL OR provider_id=?1 ORDER BY provider_id").map_err(storage)?;
                let rows = query.query_map([target], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))).map_err(storage)?;
                rows.map(|row| {
                    let (id, version, body) = row.map_err(storage)?;
                    let record = parse_provider_row(&id, version, &body).map_err(ProviderError::from);
                    Ok((id, record))
                }).collect::<dim_agent_runtime::Result<Vec<_>>>()
            }).await.map_err(ProviderError::from)
        })).await?;
        let mut descriptors = self.oauth.providers(cancel).await?;
        let seeds = bundled_seeds(&self.bundled)?;
        let mut view = ManagementView {
            drivers: drivers::custom_drivers()
                .map(|driver| ManagementDriver {
                    driver_kind: driver,
                    auth_types: if driver == "amazon-bedrock" {
                        vec!["aws_bedrock"]
                    } else {
                        vec!["api_key", "bearer", "custom_header", "anonymous"]
                    },
                    remote_model_list: drivers::remote_model_list(driver),
                })
                .collect(),
            providers: Vec::new(),
            errors: Vec::new(),
        };
        for (id, record) in records {
            let result = match record {
                Ok(PersistedProviderRecord::Static(record)) => {
                    if let Some(definition) = seeds.get(&id)
                        && record.kind == StaticRecordKind::Builtin
                        && record.definition_fingerprint != definition.definition_fingerprint
                    {
                        let candidate = reseed_candidate(&record, definition.clone());
                        let collisions: Vec<_> = candidate
                            .user_models
                            .iter()
                            .filter(|user| {
                                candidate
                                    .models
                                    .iter()
                                    .any(|model| model.model_id == user.model_id)
                            })
                            .map(|entry| entry.model_id.as_str())
                            .collect();
                        view.errors.push(ProviderRecordError { provider_id: id.clone(), message: if collisions.is_empty() {
                            "Bundled definition changed; restart to apply it".into()
                        } else { format!("Bundled definition conflicts with user models: {}; delete them and restart", collisions.join(", ")) } });
                    }
                    self.static_management(id.clone(), *record, cancel).await
                }
                Ok(PersistedProviderRecord::Managed(record)) => {
                    match descriptors
                        .iter()
                        .position(|descriptor| descriptor.provider_id == id)
                    {
                        Some(index) => {
                            let descriptor = descriptors.remove(index);
                            match self.oauth.status(&id, cancel).await {
                                Ok(account) => {
                                    Ok(Self::managed_management(descriptor, account, Some(*record)))
                                }
                                Err(error) => Err(error.into()),
                            }
                        }
                        None => Err(ProviderError::Unsupported(
                            "Managed account package is unavailable".into(),
                        )),
                    }
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(provider) => view.providers.push(provider),
                Err(error) if provider_id.is_some() => return Err(error),
                Err(error) => {
                    descriptors.retain(|descriptor| descriptor.provider_id != id);
                    view.errors.push(ProviderRecordError {
                        provider_id: id,
                        message: error.to_string(),
                    });
                }
            }
        }
        for descriptor in descriptors.into_iter().filter(|descriptor| {
            provider_id
                .as_ref()
                .is_none_or(|id| id == &descriptor.provider_id)
        }) {
            let id = descriptor.provider_id.clone();
            let result = match self.oauth.status(&id, cancel).await {
                Ok(account) => Ok(Self::managed_management(descriptor, account, None)),
                Err(error) => Err(error.into()),
            };
            match result {
                Ok(provider) => view.providers.push(provider),
                Err(error) if provider_id.is_some() => return Err(error),
                Err(error) => view.errors.push(ProviderRecordError {
                    provider_id: id,
                    message: error.to_string(),
                }),
            }
        }
        if provider_id.is_some() && view.providers.is_empty() {
            return Err(ProviderError::NotFound);
        }
        view.providers
            .sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        self.check(cancel)?;
        Ok(view)
    }
}
