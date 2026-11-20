use crate::{
    host::{
        ContextContribution, ContextModel, ContextModelHost, HostError, HostErrorKind,
        ModelPortError,
    },
    port_faces::{failure, request, unknown_method},
};
use dim_agent_types::{Message, ModelRequest, ModelSelection, SystemPrompt, ToolDefinition};
use rutis::host::CtxHost;
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const NAME: &str = "contextModelHost";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextModelDescription {
    pub context_window: f64,
    pub output_tokens: f64,
    pub threshold_tokens: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixed {
    pub system: Option<SystemPrompt>,
    pub tools: Option<Vec<ToolDefinition>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionsRequest {
    pub model: ModelSelection,
    pub fixed: Option<Fixed>,
    pub messages: Vec<Message>,
}

#[derive(Serialize)]
pub struct Contributions {
    pub algorithm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<ContextContribution>,
    pub messages: Vec<ContextContribution>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstimateRequest {
    pub model: ModelSelection,
    pub algorithm: String,
    pub contribution: ContextContribution,
}

impl ContextModelHost {
    pub(crate) fn is_bound(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) async fn resolve(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> Result<Arc<dyn ContextModel>, HostError> {
        self.0
            .as_ref()
            .ok_or_else(|| HostError::failed("Context compaction is not available"))?
            .resolve(request, cancel)
            .await
    }

    async fn model(
        &self,
        facts: ModelSelection,
        cancel: CancellationToken,
    ) -> Result<Arc<dyn ContextModel>, HostError> {
        let request = ModelRequest {
            capability_snapshot: facts.capability_snapshot,
            config: facts.config,
            metadata: facts.metadata,
            ..ModelRequest::new(facts.provider_id, facts.model_id, vec![])
        };
        self.resolve(Arc::new(request), cancel).await
    }

    pub async fn describe(
        &self,
        facts: ModelSelection,
        cancel: CancellationToken,
    ) -> Result<ContextModelDescription, HostError> {
        let model = self.model(facts, cancel).await?;
        Ok(ContextModelDescription {
            context_window: model.context_window(),
            output_tokens: model.output_tokens(),
            threshold_tokens: model.threshold_tokens(),
            algorithm: model
                .contribution_counter()
                .map(|counter| counter.algorithm().to_owned()),
        })
    }

    pub async fn count(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> Result<f64, HostError> {
        Ok(self
            .resolve(request.clone(), cancel)
            .await?
            .count_tokens(&request))
    }

    pub async fn contributions(
        &self,
        request: ContributionsRequest,
        cancel: CancellationToken,
    ) -> Result<Contributions, HostError> {
        let model = self.model(request.model, cancel).await?;
        let counter = model.contribution_counter().ok_or_else(unsupported)?;
        Ok(Contributions {
            algorithm: counter.algorithm().to_owned(),
            fixed: request
                .fixed
                .map(|fixed| counter.fixed(fixed.system.as_ref(), fixed.tools.as_deref())),
            messages: request
                .messages
                .iter()
                .map(|message| counter.message(message))
                .collect(),
        })
    }

    pub async fn estimate(
        &self,
        request: EstimateRequest,
        cancel: CancellationToken,
    ) -> Result<f64, HostError> {
        let model = self.model(request.model, cancel).await?;
        let counter = model.contribution_counter().ok_or_else(unsupported)?;
        if request.algorithm != counter.algorithm() {
            return Err(HostError::classified(
                HostErrorKind::InvalidRequest,
                "Context counting algorithm does not match the contribution",
            ));
        }
        Ok(counter.estimate(request.contribution))
    }
}

fn unsupported() -> HostError {
    HostError::failed("The context model does not support persistent usage statistics")
}

impl HostDispatch for ContextModelHost {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"describe":"async","count":"async","contributions":"async","estimate":"async"}),
        )
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        enum Operation {
            Describe(ModelSelection),
            Count(Arc<ModelRequest>),
            Contributions(ContributionsRequest),
            Estimate(EstimateRequest),
        }
        let operation = match method {
            "describe" => Operation::Describe(request(NAME, args)?),
            "count" => Operation::Count(request(NAME, args)?),
            "contributions" => Operation::Contributions(request(NAME, args)?),
            "estimate" => Operation::Estimate(request(NAME, args)?),
            _ => return Err(unknown_method(NAME, method)),
        };
        let owner = self.clone();
        let cancel = rutis_bridge::session::caller_row()
            .map(|row| row.ctx().cancellation_token())
            .unwrap_or_default()
            .child_token();
        Ok(Value::cancellable_future(async move {
            let _finished = cancel.clone().drop_guard();
            let failed = |error| failure(NAME, ModelPortError::from(error));
            use rutis_bridge::session::arg;
            match operation {
                Operation::Describe(facts) => {
                    arg(&owner.describe(facts, cancel).await.map_err(failed)?)
                }
                Operation::Count(request) => {
                    arg(&owner.count(request, cancel).await.map_err(failed)?)
                }
                Operation::Contributions(input) => {
                    arg(&owner.contributions(input, cancel).await.map_err(failed)?)
                }
                Operation::Estimate(input) => {
                    arg(&owner.estimate(input, cancel).await.map_err(failed)?)
                }
            }
        }))
    }
}
