//! Provider-owned model and preference writes share the record's CAS transaction.
use super::*;
use crate::model_catalog::FieldUpdate;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferencesPatch {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub reasoning_effort: Option<FieldUpdate<String>>,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub service_tier: Option<FieldUpdate<String>>,
}

impl PreferencesPatch {
    fn validate(&self, model: &ProviderModel, provider_id: &str) -> Result<(), ProviderError> {
        let mut requested = ModelPreferences::default();
        self.clone().apply(&mut requested);
        if requested != ModelPreferences::default() {
            let protocol = model.endpoint.ok_or_else(|| {
                ProviderError::Unsupported("Model has no executable endpoint".into())
            })?;
            let snapshot = capability_snapshot(
                model,
                ModelId {
                    provider_id: provider_id.to_owned(),
                    model_id: model.id.clone(),
                },
            )?;
            super::preferences::configure(&requested, &snapshot, protocol, &model.config)
                .map_err(|message| ProviderError::InvalidConfig(message.into()))?;
        }
        Ok(())
    }

    fn apply(self, preferences: &mut ModelPreferences) {
        for (update, field) in [
            (self.reasoning_effort, &mut preferences.reasoning_effort),
            (self.service_tier, &mut preferences.service_tier),
        ] {
            if let Some(update) = update {
                *field = match update {
                    FieldUpdate::Set { value } => Some(value),
                    FieldUpdate::Clear => None,
                };
            }
        }
    }
}

impl ProviderService {
    pub(super) async fn mutate_static(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        change: impl FnOnce(
            &mut StaticProviderRecord,
            &rusqlite::Transaction<'_>,
        ) -> Result<(), ProviderError>
        + Send
        + 'static,
        cancel: &CancellationToken,
    ) -> Result<StaticProviderRecord, ProviderError> {
        let db = self.db.clone();
        self.owned(cancel, move |_| {
            Box::pin(async move {
                db.run(move |connection| {
                    Ok((|| -> Result<_, ProviderError> {
                        let tx = connection.transaction()?;
                        let mut record =
                            read_static(&tx, &provider_id)?.ok_or(ProviderError::NotFound)?;
                        if record.version != expected_version {
                            return Err(ProviderError::Conflict(
                                "Provider version changed; reread before retrying".into(),
                            ));
                        }
                        change(&mut record, &tx)?;
                        if record.enabled
                            && record.credential_authority == CredentialAuthority::Static
                            && record.auth != StaticAuth::Anonymous
                            && record.credential.is_none()
                        {
                            return Err(ProviderError::AuthRequired);
                        }
                        record.version =
                            record.version.checked_add(1).ok_or(ProviderError::Failed)?;
                        validate_static_record(&provider_id, &mut record)
                            .map_err(|error| ProviderError::InvalidConfig(error.to_string()))?;
                        tx.execute(
                            "UPDATE providers SET version=?2,record=?3 WHERE provider_id=?1",
                            params![provider_id, record.version, serde_json::to_string(&record)?],
                        )?;
                        tx.commit()?;
                        Ok(record)
                    })())
                })
                .await?
            })
        })
        .await
    }

    pub async fn update_preferences(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        model_id: String,
        patch: PreferencesPatch,
        cancel: &CancellationToken,
    ) -> Result<ManagementProvider, ProviderError> {
        if patch.reasoning_effort.is_none() && patch.service_tier.is_none() {
            return Err(ProviderError::InvalidConfig(
                "Preference patch must not be empty".into(),
            ));
        }
        if !self.oauth.contains(&provider_id, cancel).await? {
            let id = provider_id.clone();
            let bundled = self.bundled.clone();
            let record = self
                .mutate_static(
                    provider_id.clone(),
                    expected_version,
                    move |record, _| {
                        let model = static_models(&bundled, &id, record, true)
                            .into_iter()
                            .find(|model| model.id == model_id)
                            .ok_or(ProviderError::NotFound)?;
                        patch.validate(&model, &id)?;
                        let mut preferences = record
                            .model_preferences
                            .get(&model_id)
                            .cloned()
                            .unwrap_or_default();
                        patch.apply(&mut preferences);
                        if preferences == ModelPreferences::default() {
                            record.model_preferences.remove(&model_id);
                        } else {
                            record.model_preferences.insert(model_id, preferences);
                        }
                        Ok(())
                    },
                    cancel,
                )
                .await?;
            return self.static_management(provider_id, record, cancel).await;
        }
        let descriptor = self
            .oauth
            .providers(cancel)
            .await?
            .into_iter()
            .find(|descriptor| descriptor.provider_id == provider_id)
            .ok_or(ProviderError::NotFound)?;
        let account = self.oauth.status(&provider_id, cancel).await?;
        let owner = self.clone();
        let current = account.clone();
        let bundled = self.bundled.clone();
        let catalog_bundled = bundled.clone();
        let record = self
            .owned(cancel, move |_| {
                Box::pin(async move {
                    owner
                        .db
                        .run(move |connection| {
                            Ok((|| -> Result<_, ProviderError> {
                                let tx = connection.transaction()?;
                                let mut record = read_catalog(&tx, &provider_id)?
                                    .ok_or(ProviderError::NotFound)?;
                                if record.version != expected_version
                                    || !same_account(&record.account, &current)
                                {
                                    return Err(ProviderError::Conflict(
                                        "Provider version or account changed".into(),
                                    ));
                                }
                                let model = record
                                    .catalog(&catalog_bundled)
                                    .models
                                    .into_iter()
                                    .find(|model| model.id == model_id)
                                    .ok_or(ProviderError::NotFound)?;
                                patch.validate(&model, &provider_id)?;
                                let mut preferences = record
                                    .model_preferences
                                    .get(&model_id)
                                    .cloned()
                                    .unwrap_or_default();
                                patch.apply(&mut preferences);
                                if preferences == ModelPreferences::default() {
                                    record.model_preferences.remove(&model_id);
                                } else {
                                    record.model_preferences.insert(model_id, preferences);
                                }
                                record.version =
                                    record.version.checked_add(1).ok_or(ProviderError::Failed)?;
                                save_catalog(&tx, &provider_id, &record)?;
                                tx.commit()?;
                                Ok(record)
                            })())
                        })
                        .await?
                })
            })
            .await?;
        Ok(Self::managed_management(
            descriptor,
            account,
            Some(record),
            &bundled,
        ))
    }

    pub async fn create_model(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        mut model: ModelEntry,
        cancel: &CancellationToken,
    ) -> Result<ProviderCatalog, ProviderError> {
        if self.oauth.contains(&provider_id, cancel).await? {
            return Err(ProviderError::Unsupported(
                "Account catalogs do not accept user models".into(),
            ));
        }
        let id = provider_id.clone();
        let record = self
            .mutate_static(
                provider_id,
                expected_version,
                move |record, _| {
                    if record.credential_authority != CredentialAuthority::Static {
                        return Err(ProviderError::Unsupported(
                            "Account catalogs do not accept user models".into(),
                        ));
                    }
                    // A user model states its own limits, so what it declares
                    // is what the bundle must not override.
                    if model.capabilities.context_window.is_some() {
                        declare(&mut model, "contextWindow");
                    }
                    if model.capabilities.max_output_tokens.is_some() {
                        declare(&mut model, "maxOutputTokens");
                    }
                    if record
                        .models
                        .iter()
                        .chain(&record.user_models)
                        .any(|entry| entry.model_id == model.model_id)
                    {
                        return Err(ProviderError::Conflict("Model ID already exists".into()));
                    }
                    record
                        .model_config_overrides
                        .retain(|entry| entry.model_id != model.model_id);
                    record.disabled_model_ids.retain(|id| id != &model.model_id);
                    record.model_preferences.remove(&model.model_id);
                    record.endpoint_overrides.remove(&model.model_id);
                    record.user_models.push(model);
                    Ok(())
                },
                cancel,
            )
            .await?;
        Ok(static_catalog(&self.bundled, &id, &record, true))
    }

    pub async fn delete_model(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        model_id: String,
        cancel: &CancellationToken,
    ) -> Result<ProviderCatalog, ProviderError> {
        if self.oauth.contains(&provider_id, cancel).await? {
            return Err(ProviderError::Unsupported(
                "Account catalogs do not own user models".into(),
            ));
        }
        let id = provider_id.clone();
        let record = self
            .mutate_static(
                provider_id.clone(),
                expected_version,
                move |record, tx| {
                    let Some(index) = record
                        .user_models
                        .iter()
                        .position(|entry| entry.model_id == model_id)
                    else {
                        return Err(
                            if record.models.iter().any(|entry| entry.model_id == model_id) {
                                ProviderError::Unsupported("Only user models can be deleted".into())
                            } else {
                                ProviderError::NotFound
                            },
                        );
                    };
                    record.user_models.remove(index);
                    record.disabled_model_ids.retain(|id| id != &model_id);
                    record.model_preferences.remove(&model_id);
                    record.endpoint_overrides.remove(&model_id);
                    tx.execute(
                        "DELETE FROM provider_selections WHERE provider_id=?1 AND model_id=?2",
                        params![provider_id, model_id],
                    )?;
                    Ok(())
                },
                cancel,
            )
            .await?;
        Ok(static_catalog(&self.bundled, &id, &record, true))
    }

    pub async fn update_model_config(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        model_id: String,
        mut edit: Option<ModelEdit>,
        cancel: &CancellationToken,
    ) -> Result<ProviderCatalog, ProviderError> {
        if edit
            .as_ref()
            .and_then(|edit| edit.display_name.as_ref())
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err(ProviderError::InvalidConfig(
                "Model display name must not be empty".into(),
            ));
        }
        if edit.as_ref().is_some_and(|edit| {
            [edit.context_window, edit.max_output_tokens]
                .into_iter()
                .flatten()
                .any(|limit| !limit_valid(limit.get()))
        }) {
            return Err(ProviderError::InvalidConfig(
                "Model limits must be positive safe JSON integers".into(),
            ));
        }
        if edit
            .as_ref()
            .and_then(|edit| edit.reasoning.as_ref())
            .and_then(|reasoning| reasoning.effort_options.as_ref())
            .is_some_and(|options| {
                options.iter().any(|option| {
                    !matches!(
                        option.as_str(),
                        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                    )
                })
            })
        {
            return Err(ProviderError::InvalidConfig(
                "Invalid reasoning effort option".into(),
            ));
        }
        let endpoint = edit.as_mut().and_then(|edit| edit.endpoint.take());
        let descriptor = edit.as_ref().is_some_and(|edit| {
            endpoint.is_none()
                || edit.display_name.is_some()
                || edit.context_window.is_some()
                || edit.max_output_tokens.is_some()
                || edit.tool_calling.is_some()
                || edit.vision.is_some()
                || edit.reasoning.is_some()
                || edit.modality_capabilities.is_some()
        });
        if !self.oauth.contains(&provider_id, cancel).await? {
            let available = self.account_available(&provider_id, cancel).await?;
            let bundled = self.bundled.clone();
            let id = provider_id.clone();
            let result_id = id.clone();
            let record = self
                .mutate_static(
                    provider_id,
                    expected_version,
                    move |record, _| {
                        let user = record
                            .user_models
                            .iter()
                            .position(|entry| entry.model_id == model_id);
                        if user.is_none()
                            && !record.models.iter().any(|entry| entry.model_id == model_id)
                        {
                            return Err(ProviderError::NotFound);
                        }
                        if let Some(endpoint) = endpoint {
                            if record.driver_kind != "new-api-router"
                                || record.credential_authority != CredentialAuthority::Static
                            {
                                return Err(ProviderError::Unsupported(
                                    "Endpoint edits require a static New API model".into(),
                                ));
                            }
                            match endpoint {
                                FieldUpdate::Set { value } => {
                                    if value.protocol().is_none() {
                                        return Err(ProviderError::InvalidConfig(
                                            "Endpoint is not an executable model protocol".into(),
                                        ));
                                    }
                                    record.endpoint_overrides.insert(model_id.clone(), value);
                                }
                                FieldUpdate::Clear => {
                                    record.endpoint_overrides.remove(&model_id);
                                }
                            }
                        }
                        let index = record
                            .model_config_overrides
                            .iter()
                            .position(|entry| entry.model_id == model_id);
                        match edit {
                            Some(edit) if descriptor => {
                                let current = static_entries(&bundled, &id, record)
                                    .into_iter()
                                    .find(|entry| entry.model_id == model_id)
                                    .ok_or(ProviderError::NotFound)?;
                                let next = bundled.merge_visible_edit(&current, edit);
                                if let Some(user) = user {
                                    record.user_models[user] = next;
                                } else if let Some(index) = index {
                                    record.model_config_overrides[index] = next;
                                } else {
                                    record.model_config_overrides.push(next);
                                }
                            }
                            Some(_) => {}
                            None => {
                                if user.is_some() {
                                    return Err(ProviderError::Unsupported(
                                        "Only default models can be reset".into(),
                                    ));
                                }
                                record
                                    .model_config_overrides
                                    .remove(index.ok_or(ProviderError::NotFound)?);
                            }
                        }
                        Ok(())
                    },
                    cancel,
                )
                .await?;
            return Ok(static_catalog(
                &self.bundled,
                &result_id,
                &record,
                available,
            ));
        }
        if endpoint.is_some() {
            return Err(ProviderError::Unsupported(
                "Account catalogs do not accept endpoint edits".into(),
            ));
        }
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let account = owner.oauth.status(&provider_id, &cancel).await?;
                let bundled = owner.bundled.clone();
                owner
                    .db
                    .run(move |connection| {
                        Ok((|| -> Result<_, ProviderError> {
                            let tx = connection.transaction()?;
                            let mut record =
                                read_catalog(&tx, &provider_id)?.ok_or(ProviderError::NotFound)?;
                            if !same_account(&record.account, &account)
                                || record.version != expected_version
                            {
                                return Err(ProviderError::Conflict(
                                    "Provider version or account changed".into(),
                                ));
                            }
                            let index = record
                                .model_config_overrides
                                .iter()
                                .position(|entry| entry.model_id == model_id);
                            let default = record
                                .models
                                .iter()
                                .find(|entry| entry.entry.model_id == model_id)
                                .ok_or(ProviderError::NotFound)?;
                            match edit {
                                Some(edit) => {
                                    let next = bundled.merge_visible_edit(
                                        index
                                            .map(|index| &record.model_config_overrides[index])
                                            .unwrap_or(&default.entry),
                                        edit,
                                    );
                                    if let Some(index) = index {
                                        record.model_config_overrides[index] = next;
                                    } else {
                                        record.model_config_overrides.push(next);
                                    }
                                }
                                None => {
                                    record
                                        .model_config_overrides
                                        .remove(index.ok_or(ProviderError::NotFound)?);
                                }
                            }
                            record.version =
                                record.version.checked_add(1).ok_or(ProviderError::Failed)?;
                            save_catalog(&tx, &provider_id, &record)?;
                            tx.commit()?;
                            Ok(record.catalog(&bundled))
                        })())
                    })
                    .await?
            })
        })
        .await
    }

    pub async fn set_enabled_models(
        self: &Arc<Self>,
        provider_id: String,
        expected_version: u32,
        ids: Vec<String>,
        cancel: &CancellationToken,
    ) -> Result<ProviderCatalog, ProviderError> {
        self.check(cancel)?;
        if !self.oauth.contains(&provider_id, cancel).await? {
            let available = self.account_available(&provider_id, cancel).await?;
            let id = provider_id.clone();
            let bundled = self.bundled.clone();
            let scoped = provider_id.clone();
            let record = self
                .mutate_static(
                    provider_id,
                    expected_version,
                    move |record, _| {
                        if ids.iter().any(|id| {
                            !record
                                .models
                                .iter()
                                .chain(&record.user_models)
                                .any(|model| &model.model_id == id)
                        }) {
                            return Err(ProviderError::InvalidConfig(
                                "Model is unavailable".into(),
                            ));
                        }
                        // A whole-set write governs the entries the provider
                        // can use: an unavailable entry keeps the enablement
                        // it has, whether the set names it or omits it.
                        let governed: Vec<String> =
                            static_models(&bundled, &scoped, record, available)
                                .into_iter()
                                .filter(|model| model.unavailable_reason.is_none())
                                .map(|model| model.id)
                                .collect();
                        record
                            .disabled_model_ids
                            .retain(|id| !governed.contains(id));
                        record
                            .disabled_model_ids
                            .extend(governed.iter().filter(|id| !ids.contains(id)).cloned());
                        Ok(())
                    },
                    cancel,
                )
                .await?;
            return Ok(static_catalog(&self.bundled, &id, &record, available));
        }
        let owner = self.clone();
        self.owned(cancel, move |cancel| {
            Box::pin(async move {
                let account = owner.oauth.status(&provider_id, &cancel).await?;
                let bundled = owner.bundled.clone();
                owner
                    .db
                    .run(move |connection| {
                        Ok((|| -> Result<_, ProviderError> {
                            let tx = connection.transaction()?;
                            let mut record =
                                read_catalog(&tx, &provider_id)?.ok_or(ProviderError::NotFound)?;
                            if !same_account(&record.account, &account)
                                || record.version != expected_version
                            {
                                return Err(ProviderError::Conflict(
                                    "Provider version or account changed".into(),
                                ));
                            }
                            if ids.iter().any(|id| {
                                !record
                                    .models
                                    .iter()
                                    .any(|model| &model.entry.model_id == id)
                            }) {
                                return Err(ProviderError::InvalidConfig(
                                    "Model is unavailable".into(),
                                ));
                            }
                            // The set governs the entries this account can use,
                            // as the effective limits make them: a model this
                            // area's own edit made usable is governed even
                            // while the stored reason still records the base
                            // entry.
                            let governed: Vec<String> = record
                                .effective_models(&bundled)
                                .into_iter()
                                .filter(|model| model.unavailable.is_none())
                                .map(|model| model.entry.model_id)
                                .collect();
                            record
                                .disabled_model_ids
                                .retain(|id| !governed.contains(id));
                            record
                                .disabled_model_ids
                                .extend(governed.iter().filter(|id| !ids.contains(id)).cloned());
                            record.version =
                                record.version.checked_add(1).ok_or(ProviderError::Failed)?;
                            save_catalog(&tx, &provider_id, &record)?;
                            tx.commit()?;
                            Ok(record.catalog(&bundled))
                        })())
                    })
                    .await?
            })
        })
        .await
    }
}
