//! One model call outside any Session for a consumer that cannot hold
//! credentials. The caller owns the choice of model; no event is written.
use super::{ModelPreferences, ProductResolver, ProviderService};
use dim_agent_capabilities::auxiliary_model::{self, AuxiliaryTarget};
use dim_agent_capabilities::host::*;
use dim_agent_types::ModelId;
use std::{future::Future, pin::Pin, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct AuxiliaryModel {
    pub(super) owner: Arc<ProviderService>,
    pub(super) host: Arc<ModelHost>,
}

impl CompleteAuxiliary for AuxiliaryModel {
    fn complete(
        &self,
        model: ModelId,
        request: AuxiliaryRequest,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<AuxiliaryReply, AuxiliaryError>> + Send + '_>> {
        Box::pin(async move {
            auxiliary_model::check(&request)?;
            let call = cancel.child_token();
            let _dropped = call.clone().drop_guard();
            let (owner, host) = (self.owner.clone(), self.host.clone());
            let target = model.clone();
            let started = Instant::now();
            let result = self
                .owner
                .owned(&call, move |cancel| {
                    Box::pin(async move { run(owner, host, target, request, cancel).await })
                })
                .await;
            auxiliary_model::record(&model, started, result.as_ref());
            result
        })
    }
}

/// Provider's selectability answer for a model it does not serve.
fn unavailable() -> AuxiliaryError {
    AuxiliaryError {
        code: AuxiliaryErrorCode::ModelUnavailable,
        message: "Model is unavailable".into(),
    }
}

async fn run(
    owner: Arc<ProviderService>,
    host: Arc<ModelHost>,
    model: ModelId,
    request: AuxiliaryRequest,
    cancel: CancellationToken,
) -> Result<AuxiliaryReply, AuxiliaryError> {
    let selectable = owner
        .catalog_for(model.provider_id.clone(), &cancel)
        .await?
        .is_some_and(|catalog| catalog.selectable(&model.model_id).is_some());
    if !selectable {
        return Err(unavailable());
    }
    let selection = owner
        .configuration(
            Some(model.clone()),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await?;
    let snapshot = selection
        .capability_snapshot
        .ok_or_else(|| HostError::failed("Model capabilities are missing"))?;
    let limit = snapshot.capabilities.max_output_tokens;
    let target = AuxiliaryTarget {
        model,
        config: selection.config.unwrap_or_default(),
        capability_snapshot: Some(snapshot),
        max_output_tokens: limit,
    };
    auxiliary_model::complete(&host, &ProductResolver(owner), target, request, &cancel).await
}
