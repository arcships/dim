//! The auxiliary model call in the model-config profile: the configured
//! catalog and its declared configuration, the configured connection, and the
//! shared model host. The run and the binding are the ones every owner of
//! model selection uses ([AuxiliaryModel §4.3](../../../docs/design/appserver/auxiliary-model.md#43-named-binding)).
use dim_agent_capabilities::auxiliary_model::{self, AuxiliaryTarget};
use dim_agent_capabilities::host::{
    AuxiliaryError, AuxiliaryErrorCode, AuxiliaryReply, AuxiliaryRequest, CompleteAuxiliary,
};
use dim_agent_types::ModelId;
use std::{future::Future, pin::Pin, time::Instant};
use tokio_util::sync::CancellationToken;

impl CompleteAuxiliary for crate::plugin::ConfiguredConnection {
    fn complete(
        &self,
        model: ModelId,
        request: AuxiliaryRequest,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<AuxiliaryReply, AuxiliaryError>> + Send + '_>> {
        Box::pin(async move {
            auxiliary_model::check(&request)?;
            let started = Instant::now();
            let result = match self.models.get(&model.provider_id, &model.model_id) {
                Ok(entry) => {
                    let target = AuxiliaryTarget {
                        model: model.clone(),
                        config: entry.defaults.clone(),
                        capability_snapshot: None,
                    };
                    auxiliary_model::complete(&self.host, self, target, request, &cancel).await
                }
                Err(_) => Err(unavailable()),
            };
            auxiliary_model::record(&model, started, result.as_ref());
            result
        })
    }
}

/// The code a model the file does not declare gets, the same one Provider
/// answers with.
fn unavailable() -> AuxiliaryError {
    AuxiliaryError {
        code: AuxiliaryErrorCode::ModelUnavailable,
        message: "Model is unavailable".into(),
    }
}
