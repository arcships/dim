//! Product assembly and mock model shared by the App Server cost examples.
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    context_model::ProductContextModel,
    defaults,
    history::HistoryRenderer,
    host::*,
    summary_strategy::{ProductSummaryStrategyConfig, ProductSummaryStrategyPlugin},
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The product summarizer's model: echoes turns like `EchoModel` and answers
/// the summarizer with a short summary that differs each time.
#[derive(Default)]
pub struct Summarizing(AtomicUsize);
impl ModelDriver for Summarizing {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let summarizer = matches!(request.system.as_deref(),
            Some(SystemPrompt::Text(text)) if text.starts_with("You summarize a coding session"));
        if !summarizer {
            return EchoModel.invoke(request, connection, cancel);
        }
        let number = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "summary".into(),
                }),
                Ok(ModelChunk::Text(format!(
                    "Summary {number} of the earlier turns."
                ))),
                Ok(ModelChunk::MessageStop {
                    message_id: "summary".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

pub struct Local;
impl ResolveModelConnection for Local {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async {
            Ok(ResolvedModelConnection {
                connection: ModelConnection::Local,
                failure_policy: None,
                recovery: None,
            })
        })
    }
}

/// The product context window over the echo model. The process model host is
/// provided by [`product_hosts`] or [`product_process`].
pub fn product(builder: &mut AgentBuilder) -> std::result::Result<(), Error> {
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(
            ProductContextModel,
        )))))
        .require_feature(Feature::new("ProductSummaryStrategy").session(
            "ProductSummaryStrategy",
            |names| {
                Ok(Box::new(ProductSummaryStrategyPlugin::new(
                    names,
                    ProductSummaryStrategyConfig::default(),
                )?))
            },
        ))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(builder, Arc::new(MockTools));
    let snapshot: ModelCapabilitySnapshot = serde_json::from_value(serde_json::json!({
        "target": {"providerId": "local", "modelId": "small"},
        "capabilities": {"modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": false, "vision": false, "audio": false, "reasoning": false,
            "contextWindow": 32_000, "maxOutputTokens": 2000}
    }))?;
    defaults::install_with_prompt(
        builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(ModelSelection {
                provider_id: "local".into(),
                model_id: "small".into(),
                capability_snapshot: Some(snapshot),
                config: None,
                metadata: None,
            }),
        }),
    );
    Ok(())
}

/// The product mix's model hosts, which a `MinimalHost` root provides.
pub fn product_hosts(root: &rutis::Ctx) -> dim_agent_runtime::Result<()> {
    let driver: Arc<dyn ModelDriver> = Arc::new(Summarizing::default());
    root.provide(ModelHost(driver))?;
    root.provide(ProviderConnectionResolver(Arc::new(Local)))?;
    Ok(())
}

/// The product mix's model hosts, which an `AgentBuilder` provides before
/// `serve`.
pub fn product_process(builder: &mut AgentBuilder) {
    let driver: Arc<dyn ModelDriver> = Arc::new(Summarizing::default());
    builder
        .process_value(Arc::new(ModelHost(driver)))
        .process_value(Arc::new(ProviderConnectionResolver(Arc::new(Local))));
}
