//! Safe edit snapshots are projected from one admitted Provider record.
use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementView {
    pub next: Option<String>,
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
pub struct ManagementInfo {
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementProvider {
    #[serde(flatten)]
    pub info: ManagementInfo,
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
    /// The prefix this Provider's listed ids carry, absent when it states
    /// none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glued_prefix: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementModel {
    pub model: ModelEntry,
    pub source: &'static str,
    pub enabled: bool,
    pub endpoint: Option<ModelProtocol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_limits: Vec<String>,
    pub unavailable_reason: Option<String>,
    pub actions: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_override: Option<NewApiEndpoint>,
    /// The model this entry's id stands for, when the provider says so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// The name `provider.catalog` publishes for this row, when it differs from
    /// `model.displayName`: a client that picks models from this read shows
    /// what the catalog shows, and an absent field means the display name is
    /// already the one to show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementModelPage {
    pub after: Option<String>,
    pub limit: Option<usize>,
    pub query: Option<String>,
}

impl ProviderService {
    pub(super) fn remote_model_list(
        &self,
        provider_id: &str,
        record: &StaticProviderRecord,
    ) -> bool {
        super::can_list_models(&self.bundled, provider_id, record)
    }

    pub(super) async fn static_management_info(
        &self,
        provider_id: String,
        record: &StaticProviderRecord,
        cancel: &CancellationToken,
    ) -> Result<ManagementInfo, ProviderError> {
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
            if self.remote_model_list(&provider_id, record)
                && (record.auth == StaticAuth::Anonymous || record.credential.is_some())
            {
                actions.push("refresh");
            }
        } else {
            actions.push("login");
            if available {
                actions.push("logout");
            }
        }
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
        let mut header_names: Vec<_> = record.headers.keys().cloned().collect();
        header_names.sort();
        Ok(ManagementInfo {
            provider_id,
            display_name: record.display_name.clone(),
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
            connection: Some(ManagementConnection {
                driver_kind: record.driver_kind.clone(),
                auth: record.auth.clone(),
                base_url: record.base_url.clone(),
                credential_configured: if authority == "static" {
                    record.credential.is_some()
                } else {
                    available
                },
                header_names,
                proxy_url,
                proxy_auth_configured,
                timeout_ms: record.network.timeout_ms,
                glued_prefix: record.glued_prefix.clone(),
            }),
        })
    }

    pub(super) async fn static_management(
        &self,
        provider_id: String,
        record: StaticProviderRecord,
        cancel: &CancellationToken,
    ) -> Result<ManagementProvider, ProviderError> {
        let info = self
            .static_management_info(provider_id.clone(), &record, cancel)
            .await?;
        let available = info
            .connection
            .as_ref()
            .is_some_and(|c| c.credential_configured)
            || record.credential_authority == CredentialAuthority::Static;
        let entries = static_entries(&self.bundled, &provider_id, &record);
        let catalog = static_models_from_entries(&record, available, entries.clone());
        let models = entries
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
                // The set governs the entries the provider can use: an
                // unavailable entry keeps the enablement it has, so the
                // owner offers no toggle for it.
                let mut actions = if effective.unavailable_reason.is_none() {
                    vec!["set_enabled", "edit"]
                } else {
                    vec!["edit"]
                };
                if user {
                    actions.push("delete");
                } else if overridden {
                    actions.push("reset");
                }
                let catalog_name = (effective.name != model.display_name).then_some(effective.name);
                ManagementModel {
                    enabled: !record.disabled_model_ids.contains(&model.model_id),
                    endpoint_override: record.endpoint_overrides.get(&model.model_id).copied(),
                    alias: record.model_aliases.get(&model.model_id).cloned(),
                    model,
                    source: if user {
                        "user"
                    } else if overridden {
                        "override"
                    } else {
                        "default"
                    },
                    endpoint: effective.endpoint,
                    missing_limits: effective.missing_limits,
                    unavailable_reason: effective.unavailable_reason,
                    actions,
                    catalog_name,
                }
            })
            .collect();
        Ok(ManagementProvider {
            info,
            models: Some(models),
            preferences: record.model_preferences,
            models_listed: None,
        })
    }

    pub(super) fn managed_management_info(
        descriptor: crate::managed_provider::ProviderDescriptor,
        account: AccountSnapshot,
        record: Option<&ProviderRecord>,
    ) -> ManagementInfo {
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
        ManagementInfo {
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
        }
    }

    pub(super) fn managed_management(
        descriptor: crate::managed_provider::ProviderDescriptor,
        account: AccountSnapshot,
        record: Option<ProviderRecord>,
        bundled: &BundledCatalog,
    ) -> ManagementProvider {
        let info = Self::managed_management_info(descriptor, account.clone(), record.as_ref());
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
        let (models, preferences) = match record {
            Some(record) => {
                let effective = record.effective_models(bundled);
                let names = naming::unique(effective.iter().map(|model| &model.entry));
                let models = effective
                    .into_iter()
                    .zip(names)
                    .map(|(effective, name)| {
                        // The account itself can be unavailable, a fact the
                        // record does not hold; that reason stands on top of
                        // the one the effective limits derive.
                        let unavailable_reason = unavailable.map(str::to_owned).or_else(|| {
                            effective.unavailable.map(|unavailable| unavailable.reason)
                        });
                        // The set governs the entries the account can use: an
                        // unavailable entry keeps the enablement it has, so the
                        // owner offers no toggle for it.
                        let mut actions = if unavailable.is_some() {
                            Vec::new()
                        } else if unavailable_reason.is_none() {
                            vec!["set_enabled", "edit"]
                        } else {
                            vec!["edit"]
                        };
                        if effective.overridden && unavailable.is_none() {
                            actions.push("reset");
                        }
                        let enabled = !record
                            .disabled_model_ids
                            .contains(&effective.entry.model_id);
                        let alias = record.model_aliases.get(&effective.entry.model_id).cloned();
                        let catalog_name = (name != effective.entry.display_name).then_some(name);
                        ManagementModel {
                            enabled,
                            model: effective.entry,
                            source: if effective.overridden {
                                "override"
                            } else {
                                "default"
                            },
                            endpoint: Some(effective.source.protocol),
                            missing_limits: effective.missing_limits,
                            unavailable_reason,
                            actions,
                            endpoint_override: None,
                            alias,
                            catalog_name,
                        }
                    })
                    .collect();
                (Some(models), record.model_preferences)
            }
            None => (None, BTreeMap::new()),
        };
        ManagementProvider {
            info,
            models,
            preferences,
            models_listed: None,
        }
    }

    pub async fn management(
        self: &Arc<Self>,
        provider_id: String,
        model_id: Option<String>,
        cancel: &CancellationToken,
    ) -> Result<ManagementView, ProviderError> {
        self.management_window(provider_id, model_id, None, cancel)
            .await
    }

    pub async fn management_window(
        self: &Arc<Self>,
        provider_id: String,
        model_id: Option<String>,
        page: Option<ManagementModelPage>,
        cancel: &CancellationToken,
    ) -> Result<ManagementView, ProviderError> {
        if page
            .as_ref()
            .is_some_and(|p| !(1..=128).contains(&p.limit.unwrap_or(32)))
            || (page.is_some() && model_id.is_some())
        {
            return Err(ProviderError::InvalidConfig(
                "A model page requires a limit in 1..=128 and no modelId".into(),
            ));
        }
        self.adopt_provider(&provider_id, cancel).await?;
        let db = self.db.clone();
        let id = provider_id.clone();
        let target = model_id.clone();
        let bundled = self.bundled.clone();
        let (record,target_name,pending,next,page_names) = self
            .owned(cancel, move |cancel| {
                Box::pin(async move {
                    db.run_read(move |connection| {
                        let tx=connection.transaction().map_err(storage)?;
                        let pending = tx
                            .query_row(
                                "SELECT pending FROM provider_bundle WHERE provider_id=?1",
                                [&id],
                                |r| r.get::<_, bool>(0),
                            )
                            .optional()
                            .map_err(storage)?
                            .unwrap_or(false);
                        let mut collisions = Vec::new();
                        if pending {
                            let mut statement = tx
                                .prepare(
                                    "SELECT model_id FROM provider_models WHERE provider_id=?1 AND source='user' ORDER BY position",
                                )
                                .map_err(storage)?;
                            let rows = statement.query_map([&id], |r| r.get::<_, String>(0));
                            for model in rows.map_err(storage)? {
                                let model = model.map_err(storage)?;
                                let bundled_match = bundled
                                    .source()
                                    .matching(&format!("t\0{id}\0{model}"))
                                    .next()
                                    .is_some();
                                if bundled_match {
                                    collisions.push(model);
                                }
                            }
                        }
                        let pending = pending.then_some(collisions);
                        if let Some(target) = target {
                            let record = read_model_record(&tx, &id, &target)?;
                            let targets = std::slice::from_ref(&target);
                            let mut listed = persistence::names(&tx, &bundled, &id, targets)?;
                            let name = listed.remove(&target);
                            return Ok((record, name, pending, None, None));
                        }
                        if let Some(page) = page {
                            let limit = page.limit.unwrap_or(32);
                            let query = page.query.unwrap_or_default().to_lowercase();
                            let mut after = page.after.unwrap_or_default();
                            let mut targets = Vec::new();
                            loop {
                                if cancel.is_cancelled() { return Err(storage("Provider query cancelled")); }
                                let keys = persistence::model_keys(
                                    &tx,
                                    &bundled,
                                    &id,
                                    None,
                                    &after,
                                    limit + 1 - targets.len(),
                                )?;
                                if keys.is_empty() { break; }
                                for key in keys {
                                    after = key.clone();
                                    if !query.is_empty() {
                                        let name = persistence::name(&tx, &bundled, &id, &key)?;
                                        let hit = name.is_some_and(|n| {
                                            n.model_id.to_lowercase().contains(&query)
                                                || n.display_name.to_lowercase().contains(&query)
                                        });
                                        if !hit {
                                            continue;
                                        }
                                    }
                                    targets.push(key);
                                }
                                if targets.len() > limit { break; }
                            }
                            let more = targets.len() > limit;
                            targets.truncate(limit);
                            let next = more.then(|| targets.last().cloned()).flatten();
                            let names = persistence::names(&tx, &bundled, &id, &targets)?;
                            let selected = persistence::Models::Selected(&targets);
                            let record = persistence::read(&tx, &id, selected)?;
                            return Ok((record, None, pending, next, Some(names)));
                        }
                        let all = persistence::read(&tx, &id, persistence::Models::All)?;
                        Ok((all, None, pending, None, None))
                    })
                    .await
                    .map_err(ProviderError::from)
                })
            })
            .await?;
        let mut errors = Vec::new();
        if let Some(collisions) = pending {
            errors.push(ProviderRecordError {provider_id:provider_id.clone(),message:if collisions.is_empty() {"Bundled definition changed; restart to apply it".into()} else {format!("Bundled definition conflicts with user models: {}; delete them and restart",collisions.join(", "))}});
        }
        let mut provider = match record {
            Some(PersistedProviderRecord::Static(record)) => {
                self.static_management(provider_id, *record, cancel).await?
            }
            record => {
                let descriptor = self
                    .oauth
                    .providers(cancel)
                    .await?
                    .into_iter()
                    .find(|descriptor| descriptor.provider_id == provider_id)
                    .ok_or(if record.is_some() {
                        ProviderError::Unsupported("Managed account package is unavailable".into())
                    } else {
                        ProviderError::NotFound
                    })?;
                let account = self.oauth.status(&provider_id, cancel).await?;
                let record = record.map(|record| match record {
                    PersistedProviderRecord::Managed(record) => *record,
                    PersistedProviderRecord::Static(_) => unreachable!(),
                });
                Self::managed_management(descriptor, account, record, &self.bundled)
            }
        };
        if let Some(model_id) = model_id {
            let models = provider.models.as_mut().ok_or(ProviderError::NotFound)?;
            models.retain(|model| model.model.model_id == model_id);
            if models.is_empty() {
                return Err(ProviderError::NotFound);
            }
            let name = target_name.ok_or(ProviderError::NotFound)?;
            for model in models {
                model.catalog_name = (model.model.display_name != name).then(|| name.clone());
            }
            provider.preferences.retain(|id, _| id == &model_id);
        }
        if let Some(names) = page_names
            && let Some(models) = &mut provider.models
        {
            models.sort_by(|a, b| a.model.model_id.cmp(&b.model.model_id));
            for model in models {
                if let Some(name) = names.get(&model.model.model_id) {
                    model.catalog_name = (model.model.display_name != *name).then(|| name.clone());
                }
            }
        }
        self.check(cancel)?;
        Ok(ManagementView {
            next,
            drivers: management_drivers(),
            providers: vec![provider],
            errors,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementPageRequest {
    pub provider_id: Option<String>,
    pub after: Option<String>,
    pub limit: Option<u32>,
    #[serde(default)]
    pub accounts_only: bool,
    pub query: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementPage {
    pub drivers: Vec<ManagementDriver>,
    pub providers: Vec<ManagementInfo>,
    pub errors: Vec<ProviderRecordError>,
    pub next: Option<String>,
}

impl ProviderService {
    pub async fn management_page(
        self: &Arc<Self>,
        request: ManagementPageRequest,
        cancel: &CancellationToken,
    ) -> Result<ManagementPage, ProviderError> {
        let limit = request.limit.unwrap_or(32);
        if !(1..=128).contains(&limit) || (request.provider_id.is_some() && request.after.is_some())
        {
            return Err(ProviderError::InvalidConfig(
                "Use a page size of 1..128 and either providerId or after".into(),
            ));
        }
        let mut descriptors = self.oauth.providers(cancel).await?;
        if let Some(id) = &request.provider_id {
            self.adopt_provider(id, cancel).await?;
        }
        let db = self.db.clone();
        let bundled = self.bundled.clone();
        let target = request.provider_id.clone();
        let after = request.after.clone();
        let accounts_only = request.accounts_only;
        let query_text = request.query.clone().unwrap_or_default().to_lowercase();
        let account_ids: BTreeMap<_, _> = descriptors
            .iter()
            .map(|d| (d.provider_id.clone(), d.display_name.clone()))
            .collect();
        let (records, next) = self
            .owned(cancel, move |cancel| {
                Box::pin(async move {
                    db.run(move |connection| {
                        let tx = connection.transaction().map_err(storage)?;
                        let mut ids = if let Some(target) = &target {
                            BTreeSet::from([target.clone()])
                        } else {
                            persistence::ids(&tx, &bundled)?
                        };
                        if target.is_none() {
                            ids.extend(account_ids.keys().cloned());
                        }
                        let mut records = Vec::new();
                        for id in ids {
                            if after.as_ref().is_some_and(|after| &id <= after) {
                                continue;
                            }
                            let header = persistence::header(&tx, &id)?;
                            if target.is_none() {
                                if header.is_none() && !account_ids.contains_key(&id) {
                                    continue;
                                }
                                if let Some((_, body)) = &header {
                                    if let Ok(body) = serde_json::from_str::<Value>(body) {
                                        let account = account_ids.contains_key(&id)
                                            || body["kind"] == "managed"
                                            || matches!(
                                                body["credentialAuthority"].as_str(),
                                                Some("openai_codex" | "grok")
                                            );
                                        let display = account_ids
                                            .get(&id)
                                            .map(String::as_str)
                                            .or_else(|| body["displayName"].as_str())
                                            .unwrap_or_default();
                                        if (accounts_only && !account)
                                            || (!id.to_lowercase().contains(&query_text)
                                                && !display.to_lowercase().contains(&query_text))
                                        {
                                            continue;
                                        }
                                    }
                                } else if !id.to_lowercase().contains(&query_text)
                                    && !account_ids.get(&id).is_some_and(|name| {
                                        name.to_lowercase().contains(&query_text)
                                    })
                                {
                                    continue;
                                }
                            }
                            persistence::adopt(&tx, &id, &bundled, &cancel)?;
                            let record = if header.is_some() {
                                Some(
                                    persistence::read(&tx, &id, persistence::Models::Header)
                                        .map_err(ProviderError::from)
                                        .and_then(|record| record.ok_or(ProviderError::NotFound)),
                                )
                            } else {
                                None
                            };
                            records.push((id, record));
                            if records.len() > limit as usize {
                                break;
                            }
                        }
                        let next = (records.len() > limit as usize)
                            .then(|| records[limit as usize - 1].0.clone());
                        records.truncate(limit as usize);
                        tx.commit().map_err(storage)?;
                        Ok((records, next))
                    })
                    .await
                    .map_err(ProviderError::from)
                })
            })
            .await?;
        let mut page = ManagementPage {
            drivers: management_drivers(),
            providers: Vec::new(),
            errors: Vec::new(),
            next,
        };
        for (id, record) in records {
            let result = match record.transpose() {
                Ok(Some(PersistedProviderRecord::Static(record))) => {
                    self.static_management_info(id.clone(), &record, cancel)
                        .await
                }
                Ok(record) => {
                    if let Some(index) = descriptors
                        .iter()
                        .position(|descriptor| descriptor.provider_id == id)
                    {
                        let descriptor = descriptors.remove(index);
                        match self.oauth.status(&id, cancel).await {
                            Ok(account) => Ok(Self::managed_management_info(
                                descriptor,
                                account,
                                record.as_ref().and_then(|record| match record {
                                    PersistedProviderRecord::Managed(record) => {
                                        Some(record.as_ref())
                                    }
                                    _ => None,
                                }),
                            )),
                            Err(error) => Err(error.into()),
                        }
                    } else {
                        Err(if record.is_some() {
                            ProviderError::Unsupported(
                                "Managed account package is unavailable".into(),
                            )
                        } else {
                            ProviderError::NotFound
                        })
                    }
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(info) => page.providers.push(info),
                Err(error) if request.provider_id.is_some() => return Err(error),
                Err(error) => page.errors.push(ProviderRecordError {
                    provider_id: id,
                    message: error.to_string(),
                }),
            }
        }
        if request.provider_id.is_some() && page.providers.is_empty() {
            return Err(ProviderError::NotFound);
        }
        self.check(cancel)?;
        Ok(page)
    }
}

fn management_drivers() -> Vec<ManagementDriver> {
    drivers::custom_drivers()
        .map(|driver| ManagementDriver {
            driver_kind: driver,
            auth_types: if driver == "amazon-bedrock" {
                vec!["aws_bedrock"]
            } else {
                vec!["api_key", "bearer", "custom_header", "anonymous"]
            },
            remote_model_list: drivers::remote_model_list(driver),
        })
        .collect()
}
