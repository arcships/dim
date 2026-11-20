use super::{SqliteStorage, storage_failure};
use rutis_bridge::session::Error as BridgeError;
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(
    tag = "method",
    content = "request",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub(super) enum ProviderOperation {
    #[serde(rename_all = "camelCase")]
    CommitEdit {
        provider_id: String,
        edit: Box<ProviderEdit>,
    },
    ManagementPage(ManagementRead),
    QueryModels {
        page: crate::provider::ModelQuery,
        accounts: crate::provider::ModelQueryAccounts,
    },
    FindCandidates(Candidates),
    ImportLegacy {
        batch: crate::provider::LegacySnapshot,
    },
    ListProviders {},
    #[serde(rename_all = "camelCase")]
    ReadRecord {
        provider_id: String,
        projection: RecordProjection,
    },
    #[serde(rename_all = "camelCase")]
    AdoptProvider {
        id: String,
        revision: String,
        base_facts: Box<crate::provider::persistence::AdoptionFacts>,
    },
    #[serde(rename_all = "camelCase")]
    ActivateSource {
        revision: String,
        base_facts: Vec<u8>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum RecordProjection {
    Version,
    #[serde(untagged)]
    Models(crate::provider::ProviderRecordProjection),
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields, rename_all_fields = "camelCase")]
pub(super) enum ManagementRead {
    Providers {
        page: crate::provider::ManagementPageRequest,
        accounts: BTreeMap<String, String>,
    },
    Models {
        provider_id: String,
        model_id: Option<String>,
        page: Option<crate::provider::ManagementModelPage>,
    },
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum Candidates {
    Providers {
        model_id: Option<String>,
    },
    Models {
        providers: Vec<String>,
    },
    Ordered {
        provider_id: String,
        after: Option<crate::provider::OrderedModel>,
        limit: usize,
    },
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum ProviderEdit {
    RefreshManaged {
        expected_version: Option<u32>,
        catalog: crate::managed_provider::ManagedCatalog,
        current: crate::managed_provider::AccountSnapshot,
    },
    EnableModel {
        expected_version: u32,
        model_id: String,
        enabled: bool,
        state: crate::provider::ModelAccount,
    },
    EnableModels {
        expected_version: u32,
        model_ids: Vec<String>,
        state: crate::provider::ModelAccount,
    },
    EditModel {
        expected_version: u32,
        model_id: String,
        edit: Option<crate::model_catalog::ModelEdit>,
        state: crate::provider::ModelAccount,
    },
    Preferences {
        expected_version: u32,
        model_id: String,
        patch: crate::provider::PreferencesPatch,
        account: Option<crate::managed_provider::AccountSnapshot>,
    },
    PatchHeader {
        expected_version: u32,
        patch: crate::provider::ProviderPatch,
        reply: crate::provider::UpdateReplyKind,
    },
    CreateModel {
        expected_version: u32,
        model: Box<crate::model_catalog::ModelEntry>,
    },
    DeleteModel {
        expected_version: u32,
        model_id: String,
    },
    Create {
        record: crate::provider::StaticProviderRecord,
    },
    Delete {
        expected_version: u32,
    },
    ReplaceConnection {
        expected: serde_json::Value,
        record: crate::provider::StaticProviderRecord,
    },
    RefreshStatic {
        expected: serde_json::Value,
        record: crate::provider::StaticProviderRecord,
    },
}

impl ProviderOperation {
    pub(super) async fn execute(
        self,
        db: &Arc<SqliteStorage>,
    ) -> Result<serde_json::Value, BridgeError> {
        let loaded_bundle = || {
            crate::model_catalog::BundledCatalog::load()
                .map(Arc::new)
                .map_err(|e| BridgeError::Value(e.to_string()))
        };
        Ok(match self {
            Self::CommitEdit { provider_id, edit } => {
                let reply = match *edit {
                    ProviderEdit::EnableModel {
                        expected_version,
                        model_id,
                        enabled,
                        state,
                    } => {
                        let bundled = loaded_bundle()?;
                        db.enable_model(
                            provider_id,
                            expected_version,
                            model_id,
                            enabled,
                            state,
                            bundled,
                        )
                        .await
                        .map(|v| serde_json::json!(v))
                    }
                    ProviderEdit::EnableModels {
                        expected_version,
                        model_ids,
                        state,
                    } => {
                        let bundled = loaded_bundle()?;
                        db.enable_models(provider_id, expected_version, model_ids, state, bundled)
                            .await
                            .map(|v| serde_json::json!(v))
                    }
                    ProviderEdit::EditModel {
                        expected_version,
                        model_id,
                        edit,
                        state,
                    } => {
                        crate::provider::validate_model_edit(edit.as_ref())
                            .map_err(provider_failure)?;
                        let bundled = loaded_bundle()?;
                        db.edit_model(
                            provider_id,
                            expected_version,
                            model_id,
                            edit,
                            state,
                            bundled,
                        )
                        .await
                        .map(|r| serde_json::json!(r))
                    }
                    ProviderEdit::Preferences {
                        expected_version,
                        model_id,
                        patch,
                        account,
                    } => {
                        let bundled = loaded_bundle()?;
                        match account {
                            Some(current) => db
                                .managed_preferences(
                                    provider_id,
                                    expected_version,
                                    model_id,
                                    patch,
                                    current,
                                    bundled,
                                )
                                .await
                                .map(|r| serde_json::json!(r)),
                            None => db
                                .static_preferences(
                                    provider_id,
                                    expected_version,
                                    model_id,
                                    patch,
                                    bundled,
                                )
                                .await
                                .map(|r| serde_json::json!(r)),
                        }
                    }
                    ProviderEdit::RefreshManaged {
                        expected_version,
                        catalog,
                        current,
                    } => {
                        if !crate::provider::same_account(&current, &catalog.account) {
                            return Err(provider_failure(
                                crate::provider::ProviderError::AccountChanged,
                            ));
                        }
                        db.refresh_managed(provider_id, expected_version, catalog)
                            .await
                            .map(|r| serde_json::json!(r))
                    }
                    ProviderEdit::PatchHeader {
                        expected_version,
                        patch,
                        reply,
                    } => {
                        let bundled = loaded_bundle()?;
                        let cancel = CancellationToken::new();
                        let _guard = cancel.clone().drop_guard();
                        db.patch_header(
                            provider_id,
                            expected_version,
                            patch,
                            reply,
                            bundled,
                            cancel,
                        )
                        .await
                        .map(|r| serde_json::json!(r))
                    }
                    ProviderEdit::CreateModel {
                        expected_version,
                        model,
                    } => db
                        .create_model_record(
                            provider_id,
                            expected_version,
                            *model,
                            loaded_bundle()?,
                        )
                        .await
                        .map(|r| serde_json::json!(r)),
                    ProviderEdit::DeleteModel {
                        expected_version,
                        model_id,
                    } => db
                        .delete_model_record(
                            provider_id,
                            expected_version,
                            model_id,
                            loaded_bundle()?,
                        )
                        .await
                        .map(|r| serde_json::json!(r)),
                    ProviderEdit::Create { mut record } => {
                        crate::provider::validate_static_record(&provider_id, &mut record)
                            .map_err(|e| BridgeError::Value(e.to_string()))?;
                        db.create_record(provider_id, record)
                            .await
                            .map(|r| serde_json::json!(r))
                    }
                    ProviderEdit::ReplaceConnection {
                        expected,
                        mut record,
                    }
                    | ProviderEdit::RefreshStatic {
                        expected,
                        mut record,
                    } => {
                        crate::provider::validate_static_record(&provider_id, &mut record)
                            .map_err(|e| BridgeError::Value(e.to_string()))?;
                        db.replace_record(provider_id, expected, record)
                            .await
                            .map(|r| serde_json::json!(r))
                    }
                    ProviderEdit::Delete { expected_version } => {
                        let bundled = loaded_bundle()?;
                        db.delete_record(provider_id, expected_version, bundled)
                            .await
                            .map(|()| serde_json::json!({}))
                    }
                };
                reply.map_err(provider_failure)?
            }
            Self::ManagementPage(ManagementRead::Models {
                provider_id,
                model_id,
                page,
            }) => {
                crate::provider::ManagementModelPage::validate_window(
                    model_id.as_deref(),
                    page.as_ref(),
                )
                .map_err(provider_failure)?;
                let bundled = loaded_bundle()?;
                let cancel = CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                serde_json::json!(
                    db.management_window(provider_id, model_id, page, bundled, cancel)
                        .await
                        .map_err(storage_failure)?
                )
            }
            Self::ManagementPage(ManagementRead::Providers { page, accounts }) => {
                let limit = page.page_limit().map_err(provider_failure)?;
                let bundled = loaded_bundle()?;
                let cancel = CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                let (records, next) = db
                    .management_page(page, limit, accounts, bundled, cancel)
                    .await
                    .map_err(storage_failure)?;
                let records: Vec<_> = records
                    .into_iter()
                    .map(|(id, row)| {
                        (
                            id,
                            row.map(|r| r.map_err(crate::provider::management::error)),
                        )
                    })
                    .collect();
                serde_json::json!({"records":records,"next":next})
            }
            Self::FindCandidates(request) => {
                let bundled = loaded_bundle()?;
                match request {
                    Candidates::Providers { model_id } => serde_json::json!(
                        db.candidate_providers(model_id, bundled)
                            .await
                            .map_err(storage_failure)?
                    ),
                    Candidates::Models { providers } => serde_json::json!(
                        db.candidate_models(providers, bundled)
                            .await
                            .map_err(storage_failure)?
                    ),
                    Candidates::Ordered {
                        provider_id,
                        after,
                        limit,
                    } => serde_json::json!(
                        db.ordered_models(provider_id, after, limit, bundled)
                            .await
                            .map_err(storage_failure)?
                    ),
                }
            }
            Self::QueryModels { page, accounts } => {
                let limit = page
                    .page_limit()
                    .map_err(|e| BridgeError::Value(e.to_string()))?;
                let bundled = loaded_bundle()?;
                let cancel = CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                serde_json::json!(
                    db.query_models(page, limit, accounts, bundled, cancel)
                        .await
                        .map_err(storage_failure)?
                )
            }
            Self::ImportLegacy { batch } => {
                let cancel = CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                let bundled = loaded_bundle()?;
                serde_json::json!(
                    db.import_legacy(batch, bundled, cancel)
                        .await
                        .map_err(storage_failure)?
                )
            }
            Self::ListProviders {} => {
                let bundled = loaded_bundle()?;
                serde_json::json!(db.list_providers(bundled).await.map_err(storage_failure)?)
            }
            Self::ReadRecord {
                provider_id,
                projection: RecordProjection::Version,
            } => serde_json::json!(
                db.record_version(provider_id)
                    .await
                    .map_err(storage_failure)?
            ),
            Self::ReadRecord {
                provider_id,
                projection: RecordProjection::Models(projection),
            } => serde_json::json!(
                db.read_record(provider_id, projection)
                    .await
                    .map_err(storage_failure)?
            ),
            Self::AdoptProvider {
                id,
                revision,
                base_facts,
            } => {
                let bundled = loaded_bundle()?;
                let cancel = CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                db.adopt_provider(id, revision, Some(*base_facts), bundled, cancel)
                    .await
                    .map_err(storage_failure)?;
                serde_json::json!({})
            }
            Self::ActivateSource {
                revision,
                base_facts,
            } => {
                db.activate_source(revision, base_facts)
                    .await
                    .map_err(storage_failure)?;
                serde_json::json!({})
            }
        })
    }
}

fn provider_failure(error: crate::provider::ProviderError) -> BridgeError {
    let error = crate::provider::management::error(error);
    let data = error
        .data
        .expect("Provider errors carry an application code");
    rutis_bridge::exports::failure(
        "ProviderError",
        &data.app_error,
        error.message,
        Some(serde_json::json!(data.details)),
    )
}
