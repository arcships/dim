use super::*;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelQuery {
    #[serde(default)]
    pub selectable_only: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub query: Option<String>,
    pub after: Option<ModelId>,
    pub limit: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPage {
    #[cfg(feature = "testing")]
    #[serde(skip)]
    pub work: super::read_work::ReadWork,
    pub models: Vec<ModelRow>,
    pub next: Option<ModelId>,
    pub empty: Vec<EmptyModelProvider>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub provider_id: String,
    pub provider_name: String,
    pub model: ProviderModel,
    pub version: u32,
    pub preferences: ModelPreferences,
    pub disabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyModelProvider {
    pub provider_id: String,
    pub provider_name: String,
    pub reason: EmptyModelReason,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmptyModelReason {
    NoModels { listable: bool },
    Unreadable,
}

impl ProviderService {
    pub async fn query_models(
        self: &Arc<Self>,
        input: ModelQuery,
        cancel: &CancellationToken,
    ) -> Result<ModelPage, ProviderError> {
        let limit = input.limit.unwrap_or(32) as usize;
        if !(1..=128).contains(&limit) {
            return Err(ProviderError::InvalidConfig(
                "limit must be between 1 and 128".into(),
            ));
        }
        let descriptors = self.oauth.providers(cancel).await?;
        let mut accounts = HashMap::new();
        for descriptor in &descriptors {
            if input
                .provider_id
                .as_ref()
                .is_none_or(|id| id == &descriptor.provider_id)
            {
                accounts.insert(
                    descriptor.provider_id.clone(),
                    self.oauth.status(&descriptor.provider_id, cancel).await?,
                );
            }
        }
        let show_legacy = input.provider_id.is_some() || self.legacy_codex_visible(cancel).await;
        let mut native = HashMap::new();
        for id in [LEGACY_CODEX_ID, "grok-build"] {
            if id == LEGACY_CODEX_ID && !show_legacy {
                continue;
            }
            if input.provider_id.as_ref().is_none_or(|p| p == id) {
                native.insert(id.to_owned(), self.account_available(id, cancel).await?);
            }
        }
        let db = self.db.clone();
        let bundled = self.bundled.clone();
        self.owned(cancel,move |cancel| Box::pin(async move {
            db.run(move |connection| {
                #[cfg(feature="testing")]
                read_work::reset();
                let tx = connection.transaction().map_err(storage)?;
                let provider_ids = if let Some(provider) = &input.provider_id {
                    BTreeSet::from([provider.clone()])
                } else if let Some(model) = &input.model_id {
                    persistence::providers_for_model(&tx,&bundled,model)?
                } else {
                    let mut ids = persistence::ids(&tx,&bundled)?;
                    ids.extend(descriptors.iter().map(|d|d.provider_id.clone()));
                    ids
                };
                let query = input.query.as_ref().map(|q|q.to_lowercase());
                let mut candidates = Vec::new();
                for id in provider_ids {
                    if id == LEGACY_CODEX_ID && !show_legacy { continue; }
                    if cancel.is_cancelled() { return Err(storage("Provider query cancelled")); }
                    if input.after.as_ref().is_some_and(|a| id < a.provider_id) { continue; }
                    let mut header = persistence::read(&tx,&id,persistence::Models::Header);
                    if !matches!(
                        &header,
                        Ok(Some(PersistedProviderRecord::Static(record))) if !record.enabled
                    ) {
                        persistence::adopt(&tx,&id,&bundled,&cancel)?;
                        header=persistence::read(&tx,&id,persistence::Models::Header);
                    }
                    let display = descriptors
                        .iter()
                        .find(|d| d.provider_id == id)
                        .map(|d| d.display_name.clone())
                        .or_else(|| {
                            header
                                .as_ref()
                                .ok()
                                .and_then(|r| r.as_ref())
                                .and_then(|r| match r {
                                    PersistedProviderRecord::Static(r) => {
                                        Some(r.display_name.clone())
                                    }
                                    _ => None,
                                })
                        })
                        .unwrap_or_else(|| id.clone());
                    if matches!(
                        &header,
                        Ok(Some(PersistedProviderRecord::Static(record))) if !record.enabled
                    ) {
                        continue;
                    }
                    if matches!(&header, Ok(None)) && !accounts.contains_key(&id) {
                        continue;
                    }
                    let provider_matches = query
                        .as_ref()
                        .is_none_or(|q| display.to_lowercase().contains(q));
                    let mut after = input
                        .after
                        .as_ref()
                        .filter(|a| a.provider_id == id)
                        .map(|a| a.model_id.clone())
                        .unwrap_or_default();
                    let mut saw_model = false;
                    loop {
                        let batch = persistence::model_keys(
                            &tx,
                            &bundled,
                            &id,
                            input.model_id.as_deref(),
                            &after,
                            (limit + 1 - candidates.len()).max(1),
                        )?;
                        if batch.is_empty() { break; }
                        saw_model = true;
                        for model in batch {
                            after = model.clone();
                            if !provider_matches {
                                let name = persistence::name(&tx, &bundled, &id, &model)?;
                                let wanted = query.as_deref().unwrap_or_default();
                                let matches = name.is_some_and(|n| {
                                    n.model_id.to_lowercase().contains(wanted)
                                        || n.display_name.to_lowercase().contains(wanted)
                                });
                                let zdr = query.as_deref() == Some("zdr")
                                    && tx
                                        .query_row(
                                            "SELECT zdr FROM provider_names WHERE provider_id=?1 AND model_id=?2",
                                            params![id, model],
                                            |r| r.get::<_, bool>(0),
                                        )
                                        .optional()
                                        .map_err(storage)?
                                        .unwrap_or(false);
                                if !matches && !zdr { continue; }
                            }
                            candidates.push((id.clone(),model,display.clone()));
                            if candidates.len() > limit { break; }
                        }
                        if candidates.len() > limit || input.model_id.is_some() { break; }
                    }
                    if !saw_model
                        && input.model_id.is_none()
                        && input.after.as_ref().is_none_or(|a| id > a.provider_id)
                        && (provider_matches
                            || query
                                .as_ref()
                                .is_some_and(|q| id.to_lowercase().contains(q)))
                    {
                        candidates.push((id.clone(), String::new(), display));
                    }
                    if candidates.len() > limit { break; }
                }
                let more = candidates.len() > limit;
                candidates.truncate(limit);
                let last = candidates.last().map(|(provider_id, model_id, _)| ModelId {
                    provider_id: provider_id.clone(),
                    model_id: model_id.clone(),
                });
                let next = more.then_some(last).flatten();
                let mut page = ModelPage {
                    #[cfg(feature = "testing")]
                    work: Default::default(),
                    models: Vec::new(),
                    empty: Vec::new(),
                    next,
                };
                let mut names = HashMap::new();
                let mut grouped = BTreeMap::<String, Vec<String>>::new();
                for (provider, model, _) in &candidates {
                    if !model.is_empty() {
                        grouped.entry(provider.clone()).or_default().push(model.clone());
                    }
                }
                for (provider, targets) in grouped {
                    names.insert(
                        provider.clone(),
                        persistence::names(&tx, &bundled, &provider, &targets)?,
                    );
                }
                for (provider_id, model_id, provider_name) in candidates {
                    if cancel.is_cancelled() {
                        return Err(storage("Provider query cancelled"));
                    }
                    let fields = if model_id.is_empty() {
                        persistence::Models::Header
                    } else {
                        persistence::Models::One(&model_id)
                    };
                    let record = persistence::read(&tx, &provider_id, fields);
                    if model_id.is_empty() {
                        let reason = match &record {
                            Err(_) => EmptyModelReason::Unreadable,
                            Ok(Some(PersistedProviderRecord::Static(r))) => {
                                if !native.get(&provider_id).copied().unwrap_or(true) {
                                    continue;
                                }
                                EmptyModelReason::NoModels {
                                    listable: can_list_models(&bundled, &provider_id, r),
                                }
                            }
                            _ if accounts
                                .get(&provider_id)
                                .is_some_and(|a| a.account.is_some()) =>
                            {
                                EmptyModelReason::NoModels { listable: true }
                            }
                            _ if accounts.contains_key(&provider_id) => continue,
                            Ok(None) => continue,
                            _ => EmptyModelReason::Unreadable,
                        };
                        page.empty.push(EmptyModelProvider {
                            provider_id,
                            provider_name,
                            reason,
                        });
                        continue;
                    }
                    let (catalog, preferences) = match record {
                        Ok(Some(PersistedProviderRecord::Static(record))) => {
                            let catalog = static_catalog(
                                &bundled,
                                &provider_id,
                                &record,
                                native.get(&provider_id).copied().unwrap_or(true),
                            );
                            let preferences = record
                                .model_preferences
                                .get(&model_id)
                                .cloned()
                                .unwrap_or_default();
                            (catalog, preferences)
                        }
                        Ok(Some(PersistedProviderRecord::Managed(record)))
                            if accounts.get(&provider_id).is_some_and(|a| {
                                a.account.is_some() && same_account(&record.account, a)
                            }) =>
                        {
                            let catalog = record.catalog(&bundled);
                            let preferences = record
                                .model_preferences
                                .get(&model_id)
                                .cloned()
                                .unwrap_or_default();
                            (catalog, preferences)
                        }
                        Ok(_) => continue,
                        Err(_) => {
                            page.empty.push(EmptyModelProvider {
                                provider_id,
                                provider_name,
                                reason: EmptyModelReason::Unreadable,
                            });
                            continue;
                        }
                    };
                    if input.selectable_only && catalog.selectable(&model_id).is_none() {
                        continue;
                    }
                    let disabled = catalog.disabled_model_ids.contains(&model_id);
                    let found = catalog.models.into_iter().find(|m| m.id == model_id);
                    let Some(mut model) = found else {
                        continue;
                    };
                    model.name = names
                        .get_mut(&provider_id)
                        .and_then(|names| names.remove(&model_id))
                        .ok_or_else(|| storage("Selected model name disappeared"))?;
                    page.models.push(ModelRow {
                        provider_id,
                        provider_name,
                        model,
                        version: catalog.version,
                        preferences,
                        disabled,
                    });
                }
                #[cfg(feature = "testing")]
                {
                    page.work = read_work::get();
                }
                tx.commit().map_err(storage)?;
                Ok(page)
            }).await.map_err(ProviderError::from)
        })).await
    }
}
