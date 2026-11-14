//! Provider configuration wire boundary; accepted operations drain on unload.
use super::ProviderService;
use crate::{
    methods::{ManagementMethods, SelectionScope},
    protocol::{Empty, canonical_cwd, optional_non_null},
};
use dim_agent_capabilities::methods::{
    ErrorData, Interruption, MethodCall, MethodDecl, RpcError, params, reply,
};
use rutis::{Ctx, Error};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportLegacyProviders {
    pub source_path: std::path::PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementScope {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub provider_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProvider {
    pub provider_id: String,
    pub expected_version: u32,
    pub patch: super::ProviderPatch,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateModel {
    pub provider_id: String,
    pub expected_version: u32,
    pub model: crate::model_catalog::ModelEntry,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePreferences {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_id: String,
    pub patch: super::PreferencesPatch,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteProvider {
    pub provider_id: String,
    pub expected_version: u32,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetEnabledModels {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_ids: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTarget {
    pub provider_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditModel {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_id: String,
    pub edit: crate::model_catalog::ModelEdit,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResetModel {
    pub provider_id: String,
    pub expected_version: u32,
    pub model_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectModel {
    #[serde(default, deserialize_with = "optional_non_null")]
    pub cwd: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, deserialize_with = "optional_non_null")]
    pub expected_version: Option<u32>,
}
#[derive(Debug, Serialize)]
pub struct CatalogResult {
    pub catalog: Option<crate::provider::ProviderCatalog>,
}
#[derive(Debug, Serialize)]
pub struct SelectionResult {
    pub selection: Option<crate::provider::ProviderSelection>,
}

pub(super) fn provide(
    ctx: &Ctx,
    methods: &ManagementMethods,
    provider: Arc<ProviderService>,
) -> Result<(), Error> {
    methods.register(
        ctx,
        "provider",
        [
            "provider.management",
            "provider.create",
            "provider.update",
            "provider.probe",
            "provider.models.create",
            "provider.models.delete",
            "provider.preferences.update",
            "provider.catalog",
            "provider.refresh",
            "provider.models.set_enabled",
            "provider.models.edit",
            "provider.models.reset",
            "provider.selection",
            "provider.select",
            "provider.list",
            "provider.delete",
            "provider.import_legacy",
        ]
        .into_iter()
        .map(|name| {
            MethodDecl::new(
                name,
                !matches!(
                    name,
                    "provider.catalog"
                        | "provider.selection"
                        | "provider.list"
                        | "provider.management"
                        | "provider.probe"
                ),
                Interruption::Drain,
            )
        })
        .collect(),
        Arc::new(move |call, stopping, _| {
            let MethodCall {
                method,
                params: value,
                ..
            } = call;
            let provider = provider.clone();
            Box::pin(async move {
                let cancel = &stopping;
                match method.as_str() {
                    "provider.management" => {
                        let p: ManagementScope = params(value)?;
                        reply(
                            provider
                                .management(p.provider_id, cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.create" => {
                        let p: super::CreateProvider = params(value)?;
                        reply(provider.create(p, cancel).await.map_err(error)?)
                    }
                    "provider.probe" => {
                        let p: super::ProbeProvider = params(value)?;
                        reply(provider.probe(p, cancel).await.map_err(error)?)
                    }
                    "provider.update" => {
                        let p: UpdateProvider = params(value)?;
                        reply(
                            provider
                                .update(p.provider_id, p.expected_version, p.patch, cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.models.create" => {
                        let p: CreateModel = params(value)?;
                        reply(
                            provider
                                .create_model(p.provider_id, p.expected_version, p.model, cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.models.delete" => {
                        let p: ResetModel = params(value)?;
                        reply(
                            provider
                                .delete_model(p.provider_id, p.expected_version, p.model_id, cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.preferences.update" => {
                        let p: UpdatePreferences = params(value)?;
                        reply(
                            provider
                                .update_preferences(
                                    p.provider_id,
                                    p.expected_version,
                                    p.model_id,
                                    p.patch,
                                    cancel,
                                )
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.catalog" => {
                        let p: ProviderTarget = params(value)?;
                        reply(CatalogResult {
                            catalog: provider
                                .catalog_for(p.provider_id, cancel)
                                .await
                                .map_err(error)?,
                        })
                    }
                    "provider.refresh" => {
                        let p: ProviderTarget = params(value)?;
                        reply(
                            provider
                                .refresh_for(p.provider_id, cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.models.set_enabled" => {
                        let p: SetEnabledModels = params(value)?;
                        reply(
                            provider
                                .set_enabled_models(
                                    p.provider_id,
                                    p.expected_version,
                                    p.model_ids,
                                    cancel,
                                )
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.models.edit" => {
                        let p: EditModel = params(value)?;
                        reply(
                            provider
                                .update_model_config(
                                    p.provider_id,
                                    p.expected_version,
                                    p.model_id,
                                    Some(p.edit),
                                    cancel,
                                )
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.models.reset" => {
                        let p: ResetModel = params(value)?;
                        reply(
                            provider
                                .update_model_config(
                                    p.provider_id,
                                    p.expected_version,
                                    p.model_id,
                                    None,
                                    cancel,
                                )
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.selection" => {
                        let p: SelectionScope = params(value)?;
                        let cwd = match p.cwd {
                            Some(cwd) => Some(canonical_cwd(&cwd).await?),
                            None => None,
                        };
                        reply(SelectionResult {
                            selection: provider
                                .selection(cwd.as_deref(), cancel)
                                .await
                                .map_err(error)?,
                        })
                    }
                    "provider.select" => {
                        let p: SelectModel = params(value)?;
                        if p.model_id.trim().is_empty() {
                            return Err(RpcError::invalid("modelId must not be empty"));
                        }
                        let cwd = match p.cwd {
                            Some(cwd) => Some(canonical_cwd(&cwd).await?),
                            None => None,
                        };
                        reply(
                            provider
                                .select(
                                    cwd.as_deref(),
                                    dim_agent_types::ModelId {
                                        provider_id: p.provider_id,
                                        model_id: p.model_id,
                                    },
                                    p.expected_version,
                                    cancel,
                                )
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.list" => {
                        let _: Empty = params(value)?;
                        reply(provider.list(cancel).await.map_err(error)?)
                    }
                    "provider.import_legacy" => {
                        let p: ImportLegacyProviders = params(value)?;
                        reply(
                            provider
                                .import_legacy(p.source_path, cancel)
                                .await
                                .map_err(error)?,
                        )
                    }
                    "provider.delete" => {
                        let p: DeleteProvider = params(value)?;
                        provider
                            .delete_static(p.provider_id, p.expected_version, cancel)
                            .await
                            .map_err(error)?;
                        reply(Empty {})
                    }
                    _ => unreachable!("registered provider method"),
                }
            })
        }),
    )?;
    Ok(())
}

fn error(error: impl Into<super::ProviderError>) -> RpcError {
    use super::ProviderError;
    let error = error.into();
    let kind = match &error {
        ProviderError::NotFound => "provider_not_found",
        ProviderError::Conflict(_) => "provider_conflict",
        ProviderError::CatalogStale { .. } => "provider_catalog_stale",
        ProviderError::AccountChanged => "provider_account_changed",
        ProviderError::InvalidConfig(_) => "provider_invalid_config",
        ProviderError::Unsupported(_) => "provider_unsupported",
        ProviderError::AuthRequired => "provider_auth_required",
        ProviderError::AuthFailed => "provider_auth_failed",
        ProviderError::TransportFailed => "provider_transport_failed",
        ProviderError::Cancelled => "provider_cancelled",
        ProviderError::Failed => "provider_failed",
    };
    let details = match &error {
        ProviderError::CatalogStale {
            provider_id,
            expected_version,
            current_version,
            issuer,
            subject,
        } => std::collections::BTreeMap::from([
            ("providerId".into(), serde_json::json!(provider_id)),
            (
                "expectedVersion".into(),
                serde_json::json!(expected_version),
            ),
            ("currentVersion".into(), serde_json::json!(current_version)),
            ("issuer".into(), serde_json::json!(issuer)),
            ("subject".into(), serde_json::json!(subject)),
            (
                "requiredAction".into(),
                serde_json::json!(if issuer.is_some() {
                    "provider.refresh"
                } else {
                    "account.login"
                }),
            ),
        ]),
        _ => Default::default(),
    };
    RpcError {
        code: -32000,
        message: error.to_string(),
        data: Some(ErrorData {
            app_error: kind.into(),
            details,
            daemon_busy: None,
        }),
    }
}
