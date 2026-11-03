use crate::history::HistoryEntry;
use dim_agent_runtime::PromptSourceTarget;
use dim_agent_types::*;
use futures_util::Stream;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) use dim_agent_runtime::panic_message;

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;
pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelChunk, HostError>> + Send>>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum HostError {
    #[error("{message}")]
    Failed {
        message: String,
        budget: Option<ProviderOutputBudgetFacts>,
        kind: HostErrorKind,
        model: Option<Box<ModelFailureFacts>>,
        presentation: Option<Box<ProviderPresentation>>,
    },
    #[error("{0}")]
    Cancelled(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFailureOrigin {
    Transport,
    HttpResponse,
    StreamEvent,
}
#[derive(Debug, Clone)]
pub struct ModelFailureFacts {
    pub origin: ModelFailureOrigin,
    /// Trimmed, ASCII lowercase, and nonempty when present; normalized at ingress.
    pub error_type: Option<String>,
    pub message: Option<String>,
    pub request_id: Option<String>,
    pub transient: bool,
    pub retry_after: Option<std::time::Duration>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostErrorKind {
    Other,
    Http {
        status: Option<u16>,
        /// Trimmed, ASCII lowercase, and nonempty when present; normalized at ingress.
        code: Option<String>,
    },
    InvalidRequest,
    Unsupported,
    Transport,
    OwnerClosed,
}
impl HostError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            budget: None,
            kind: HostErrorKind::Other,
            model: None,
            presentation: None,
        }
    }
    pub fn classified(kind: HostErrorKind, message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            budget: None,
            kind,
            model: None,
            presentation: None,
        }
    }
}

/// Request settings deliberately omit messages: the loop owns the fixed prefix.
#[derive(Debug, Clone)]
pub struct ModelSettings {
    pub provider_id: String,
    pub model_id: String,
    pub capability_snapshot: Option<ModelCapabilitySnapshot>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub system: Option<SystemPrompt>,
    pub config: Option<ModelConfig>,
    pub metadata: Option<RequestMetadata>,
}
impl ModelSettings {
    pub fn new(provider_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
            capability_snapshot: None,
            tools: None,
            system: None,
            config: None,
            metadata: None,
        }
    }
    pub fn request(self, messages: Vec<Arc<Message>>) -> ModelRequest {
        ModelRequest {
            provider_id: self.provider_id,
            model_id: self.model_id,
            capability_snapshot: self.capability_snapshot,
            messages,
            tools: self.tools.map(Arc::new),
            system: self.system.map(Arc::new),
            config: self.config,
            metadata: self.metadata,
        }
    }
}

pub trait PrepareModel: Send + Sync + 'static {
    /// Prepares settings against the request's pinned available tools.
    /// Returned tools must match these definitions; `None` declares no tools.
    fn prepare(
        &self,
        turn_id: &str,
        request_id: &str,
        history: &[Arc<Message>],
        available_tools: &[ToolDefinition],
    ) -> Result<ModelSettings, HostError>;
}
pub struct PrepareModelHost(pub Arc<dyn PrepareModel>);

#[derive(Debug, Clone)]
pub struct ContextSource {
    pub turn_id: String,
    pub request_id: String,
    pub through: u64,
}
pub trait PrepareContext: Send + Sync + 'static {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        source: ContextSource,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>>;
}
pub struct PrepareContextHost(pub Arc<dyn PrepareContext>);

pub trait LoadPrompt: Send + Sync + 'static {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>>;
}
pub struct PromptHost(pub Arc<dyn LoadPrompt>);

pub struct EmptyPromptHost;
impl LoadPrompt for EmptyPromptHost {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                Err(HostError::Cancelled("Prompt loading cancelled".into()))
            } else {
                Err(HostError::failed(format!(
                    "unknown prompt source: {}",
                    target.source
                )))
            }
        })
    }
}

/// Host adapters validate external wire chunks before producing this type.
#[derive(Debug, Clone)]
pub enum ModelChunk {
    MessageStart {
        message_id: String,
    },
    /// A streamed fragment of the current thinking block, for display.
    Thinking(String),
    /// The current thinking block is complete, with its whole content.
    ThinkingEnd {
        thinking: String,
    },
    /// A streamed fragment of the current text block.
    Text(String),
    /// The current text block is complete.
    TextEnd,
    /// A tool call has started; it precedes the call's arguments and `ToolUse`.
    ToolStart {
        id: String,
        name: String,
    },
    /// A fragment of a started call's argument text, for display.
    ToolArguments {
        id: String,
        name: String,
        delta: String,
    },
    ToolUse(ToolCall),
    /// Complete native blocks, including continuation metadata, before MessageStop.
    Content(Vec<MessagePart>),
    MessageStop {
        message_id: String,
        reason: ModelStopReason,
    },
    Usage(Usage),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStopReason {
    EndTurn,
    ToolUse,
    StopSequence,
    MaxTokens,
}
/// Connection facts are resolved for one final request and never serialized.
/// Local is the explicit development host profile, with no network credentials.
#[derive(Clone)]
pub enum ModelConnection {
    Local,
    Http(HttpModelConnection),
    Bedrock(BedrockModelConnection),
}
pub use dim_agent_types::ModelProtocol;
/// The reasoning-effort mapping of the model catalog, applied to a copied
/// configuration: the configuration's own reasoning fields are reset, then the
/// model's declaration for this protocol is set (ContextCompaction §13.2).
/// Effort `none` uses the model's reasoning-off declaration or a native `none`;
/// any other effort must be one of the model's `effort_options`. The error is
/// the caller-visible reason the effort is not supported.
pub fn reasoning_config(
    base: &ModelConfig,
    snapshot: &ModelCapabilitySnapshot,
    protocol: ModelProtocol,
    effort: &str,
) -> Result<ModelConfig, &'static str> {
    if protocol == ModelProtocol::Bedrock {
        return Err("Model preferences are not supported by Bedrock");
    }
    let mut config = base.clone();
    let reasoning = snapshot
        .reasoning
        .as_ref()
        .filter(|reasoning| reasoning.supported && snapshot.capabilities.reasoning)
        .ok_or("Model does not support reasoning preferences")?;
    let native = reasoning
        .effort_options
        .as_ref()
        .is_some_and(|options| options.iter().any(|option| option == effort));
    if let Some(previous) = &mut config.reasoning {
        previous.effort = None;
        previous.enabled = None;
        previous.budget_tokens = None;
    }
    if let Some(options) = &mut config.provider_options {
        match protocol {
            ModelProtocol::Anthropic => {
                if let Some(options) = &mut options.anthropic {
                    options.effort = None;
                    options.thinking = None;
                }
            }
            ModelProtocol::OpenaiCompatible => {
                if let Some(options) = &mut options.openai_compatible {
                    options.reasoning_effort = None;
                    options.thinking = None;
                    options.enable_thinking = None;
                }
            }
            ModelProtocol::Gemini => {
                if let Some(thinking) = options
                    .gemini
                    .as_mut()
                    .and_then(|options| options.thinking_config.as_mut())
                {
                    thinking.thinking_budget = None;
                }
            }
            ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => {
                if let Some(options) = &mut options.openai {
                    options.reasoning_effort = None;
                }
            }
            // Bedrock is rejected above, before the configuration is copied.
            ModelProtocol::Bedrock => {}
            ModelProtocol::Xai | ModelProtocol::XaiResponses => {
                if let Some(options) = &mut options.xai {
                    options.reasoning_effort = None;
                }
            }
        }
    }
    if effort == "none" {
        match (&reasoning.off, protocol) {
            (Some(ReasoningOff::ThinkingType { value }), ModelProtocol::Anthropic)
                if value == "disabled" =>
            {
                config
                    .provider_options
                    .get_or_insert_default()
                    .anthropic
                    .get_or_insert_default()
                    .thinking = Some(AnthropicThinking {
                    thinking_type: Some(AnthropicThinkingType::Disabled),
                    ..Default::default()
                });
            }
            (Some(ReasoningOff::ThinkingType { value }), ModelProtocol::OpenaiCompatible)
                if value == "disabled" =>
            {
                config
                    .provider_options
                    .get_or_insert_default()
                    .openai_compatible
                    .get_or_insert_default()
                    .thinking = Some(CompatibleThinking {
                    thinking_type: Some(ThinkingType::Disabled),
                });
            }
            (Some(ReasoningOff::ThinkingBudget { value: 0 }), ModelProtocol::Gemini) => {
                config
                    .provider_options
                    .get_or_insert_default()
                    .gemini
                    .get_or_insert_default()
                    .thinking_config
                    .get_or_insert_default()
                    .thinking_budget = Some(0.);
            }
            (
                Some(ReasoningOff::EnableThinking { value: false }),
                ModelProtocol::OpenaiCompatible,
            ) => {
                config
                    .provider_options
                    .get_or_insert_default()
                    .openai_compatible
                    .get_or_insert_default()
                    .enable_thinking = Some(false);
            }
            (None, _) if native => {
                config.reasoning.get_or_insert_default().effort = Some(effort.to_owned());
            }
            _ => {
                return Err("Model reasoning-off declaration is not supported by this protocol");
            }
        }
    } else if native && !effort.is_empty() {
        config.reasoning.get_or_insert_default().effort = Some(effort.to_owned());
    } else {
        return Err("Reasoning effort is not supported by this model");
    }
    if config
        .reasoning
        .as_ref()
        .is_some_and(|reasoning| *reasoning == ProviderReasoningConfig::default())
    {
        config.reasoning = None;
    }
    Ok(config)
}
/// The inverse of [`reasoning_config`], and of a model's own declared
/// configuration: the preferences a configuration sends, in the spelling a
/// client requests them. It names the value the configuration carries for this
/// protocol — a native effort, a protocol option, or the reasoning-off
/// declaration — and nothing when it carries none.
pub fn adopted_preferences(
    config: &ModelConfig,
    protocol: ModelProtocol,
) -> Option<ModelPreferences> {
    let options = config.provider_options.as_ref();
    let effort = config
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.effort.clone())
        .filter(|effort| !effort.is_empty() && effort != "provider-default")
        .or_else(|| match protocol {
            ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => options
                .and_then(|options| options.openai.as_ref())
                .and_then(|options| options.reasoning_effort.clone()),
            ModelProtocol::OpenaiCompatible => options
                .and_then(|options| options.openai_compatible.as_ref())
                .and_then(|options| options.reasoning_effort.clone()),
            ModelProtocol::Anthropic => options
                .and_then(|options| options.anthropic.as_ref())
                .and_then(|options| options.effort.clone()),
            ModelProtocol::Xai | ModelProtocol::XaiResponses => options
                .and_then(|options| options.xai.as_ref())
                .and_then(|options| options.reasoning_effort.clone()),
            ModelProtocol::Gemini | ModelProtocol::Bedrock => None,
        })
        .or_else(|| reasoning_off(options, protocol).then(|| "none".to_owned()));
    let service_tier = match protocol {
        ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => options
            .and_then(|options| options.openai.as_ref())
            .and_then(|options| options.service_tier.clone()),
        ModelProtocol::OpenaiCompatible => options
            .and_then(|options| options.openai_compatible.as_ref())
            .and_then(|options| options.service_tier.clone()),
        _ => None,
    };
    (effort.is_some() || service_tier.is_some()).then_some(ModelPreferences {
        reasoning_effort: effort,
        service_tier,
    })
}
/// Whether the configuration carries the protocol's own reasoning-off
/// declaration.
fn reasoning_off(options: Option<&ProviderOptions>, protocol: ModelProtocol) -> bool {
    match protocol {
        ModelProtocol::Anthropic => options
            .and_then(|options| options.anthropic.as_ref())
            .and_then(|options| options.thinking.as_ref())
            .is_some_and(|thinking| {
                thinking.thinking_type == Some(AnthropicThinkingType::Disabled)
            }),
        ModelProtocol::OpenaiCompatible => options
            .and_then(|options| options.openai_compatible.as_ref())
            .is_some_and(|options| {
                options.enable_thinking == Some(false)
                    || options
                        .thinking
                        .as_ref()
                        .and_then(|thinking| thinking.thinking_type)
                        == Some(ThinkingType::Disabled)
            }),
        ModelProtocol::Gemini => options
            .and_then(|options| options.gemini.as_ref())
            .and_then(|options| options.thinking_config.as_ref())
            .is_some_and(|config| config.thinking_budget.is_some_and(|budget| budget == 0.)),
        _ => false,
    }
}
/// Request-local transport facts. Providers validate these at their input
/// boundary; the model host owns clients, not persisted provider records.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelNetwork {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub proxy: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub timeout_ms: Option<u64>,
}
#[derive(Clone)]
pub struct HttpModelConnection {
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub network: ModelNetwork,
    pub headers: std::collections::HashMap<String, String>,
    pub bearer: Option<String>,
    pub api_key: Option<String>,
    pub preset: Option<String>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    pub session_token: Option<String>,
}
#[derive(Clone)]
pub enum BedrockAuth {
    Bearer(String),
    SigV4 {
        credentials: AwsCredentials,
        region: String,
    },
}
#[derive(Clone)]
pub struct BedrockModelConnection {
    pub base_url: String,
    pub network: ModelNetwork,
    pub headers: HashMap<String, String>,
    pub auth: BedrockAuth,
}
fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
/// Request-local credentials stay with the caller, never with the transport.
pub struct ResolvedModelConnection {
    pub connection: ModelConnection,
    pub failure_policy: Option<Arc<dyn ModelFailurePolicy>>,
    pub recovery: Option<Arc<dyn RecoverModelConnection>>,
}
/// Recovery replaces credentials, never the request's product policy.
pub struct RecoveredModelConnection {
    pub connection: ModelConnection,
    pub recovery: Arc<dyn RecoverModelConnection>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CredentialRecoveryPhase {
    #[default]
    Initial,
    Recovered,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRejection {
    Unrelated,
    Recoverable,
    Terminal,
}
pub trait RecoverModelConnection: Send + Sync + 'static {
    /// Pure authority-specific interpretation; a terminal rejection requires
    /// settlement even when output or policy prevents replay.
    fn rejection(&self, error: &HostError, phase: CredentialRecoveryPhase) -> CredentialRejection;
    fn recover(&self, cancel: CancellationToken) -> HostFuture<'_, RecoveredModelConnection>;
    fn finish_rejected(&self, cancel: CancellationToken) -> HostFuture<'_, ()>;
}
/// Pure request policy, independent of credential recovery support.
pub trait ModelFailurePolicy: Send + Sync + 'static {
    fn forbids_retry(&self, _error: &HostError) -> bool {
        false
    }
    fn presentation(&self, _error: &HostError) -> Option<ProviderPresentation> {
        None
    }
}
pub trait ResolveModelConnection: Send + Sync + 'static {
    fn resolve(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection>;
}
pub struct ProviderConnectionResolver(pub Arc<dyn ResolveModelConnection>);

/// Resolves a configuration once. Executing an adopted request must not call
/// this port again to replace its capabilities with the latest catalog.
pub trait ResolveModelSelection: Send + Sync + 'static {
    fn resolve(
        &self,
        explicit: Option<ModelId>,
        cwd: Option<std::path::PathBuf>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelSelection>;
}
pub struct ModelSelectionHost(pub Arc<dyn ResolveModelSelection>);

/// Enumerates the models the owner would serve: the same selectability rules as
/// execution, one read, no live request. The caller passes its cancellation
/// token; cancelling it cancels the read.
pub trait SelectableModels: Send + Sync + 'static {
    fn selectable(&self, cancel: CancellationToken)
    -> HostFuture<'_, Vec<ModelCapabilitySnapshot>>;
}
pub struct SelectableModelsHost(pub Arc<dyn SelectableModels>);

/// One non-streaming request outside any Session: no tools, no Timeline.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuxiliaryRequest {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_output_tokens: Option<u32>,
}
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryReply {
    /// The text blocks only; thinking output is dropped.
    pub text: String,
    pub stop_reason: ModelStopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliaryErrorCode {
    InvalidRequest,
    ModelUnavailable,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AuxiliaryError {
    pub code: AuxiliaryErrorCode,
    pub message: String,
}
/// The caller chooses the model; the owner of credentials never falls back
/// to another one and never returns the connection.
pub trait CompleteAuxiliary: Send + Sync + 'static {
    fn complete(
        &self,
        model: ModelId,
        request: AuxiliaryRequest,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<AuxiliaryReply, AuxiliaryError>> + Send + '_>>;
}
pub struct AuxiliaryModelHost(pub Arc<dyn CompleteAuxiliary>);

pub trait ModelDriver: Send + Sync + 'static {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream>;
}
pub struct ModelHost(pub Arc<dyn ModelDriver>);

#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub turn_id: String,
    pub request_id: String,
    pub branch_id: String,
}
pub trait ToolDriver: Send + Sync + 'static {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }
    /// Resolve a filesystem write's target without effects. The returned call
    /// is the one Approval admits and the handler executes.
    fn resolve_write(&self, call: ToolCall, _: CancellationToken) -> HostFuture<'_, ToolCall> {
        Box::pin(async move { Ok(call) })
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult>;
}
pub struct ToolHost(pub Arc<dyn ToolDriver>);

/// What ContextPolicy hands a strategy: the entries from the previous cut
/// on, the previous result, the request whose context is compacted and the
/// cuts the retention rule allows.
#[derive(Clone)]
pub struct CompactionInput {
    /// The projected entries from the previous `retain_from` on.
    pub history: Vec<HistoryEntry>,
    pub checkpoint: Option<CompactionCheckpoint>,
    pub request: Arc<ModelRequest>,
    pub model: Arc<dyn ContextModel>,
    /// `retain_from` of the earliest cut whose kept tail fits the retention
    /// budget (for a manual step, at least the latest turn's start).
    pub retain_limit: Option<String>,
    /// Every legal cut, in order; a strategy returns one of their
    /// `retain_from`.
    pub cuts: Vec<CompactionCut>,
    pub trigger: CompactionTrigger,
    pub estimated_input_tokens: f64,
    pub baseline_source: BaselineSource,
    pub baseline_prompt_tokens: Option<f64>,
    pub delta_tokens: Option<f64>,
    pub provider_budget: Option<ProviderOutputBudgetFacts>,
    /// The earlier pass of a compaction ContextPolicy re-planned (§5.3), as
    /// it reported it: a pass given it continues the same request budget and
    /// its own counts, and the policy adds its model calls and usage to the
    /// earlier pass's.
    pub prior: Option<StrategyDiagnostics>,
}
/// A legal cut: `history[..entry]` is replaced, `history[entry..]` is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionCut {
    pub retain_from: String,
    pub entry: usize,
    /// At or before `retain_limit`.
    pub within_limit: bool,
}
impl CompactionInput {
    /// The latest cut within the retention limit or, when none is within it
    /// (a single long turn), the latest cut that keeps something, so the
    /// branch compacts instead of skipping until overflow. The cut that
    /// replaces everything is within the limit whenever no kept tail fits the
    /// bound (ContextCompaction §5.3).
    pub fn cut(&self) -> Option<&CompactionCut> {
        self.cuts
            .iter()
            .rev()
            .find(|cut| cut.within_limit)
            .or_else(|| {
                self.cuts
                    .iter()
                    .rev()
                    .find(|cut| self.may_fall_back_to(cut))
            })
    }
    /// Whether a cut outside the retention limit is allowed when none is
    /// within it.
    pub fn may_fall_back_to(&self, cut: &CompactionCut) -> bool {
        self.provider_budget.is_some() || cut.entry < self.history.len()
    }
}
#[derive(Debug, Clone)]
pub struct CompactionCheckpoint {
    pub retain_from: String,
    pub replacement: Arc<[Message]>,
    pub context_baseline: Arc<[ContextBaselineEntry]>,
    /// The parts of `replacement`: the strategy's messages, then the digest;
    /// the rest are the pinned inputs (ContextCompaction §4.3).
    pub replacement_parts: Option<ReplacementParts>,
}
/// Window facts of the model whose context is prepared.
pub trait ContextModel: Send + Sync + 'static {
    fn context_window(&self) -> f64;
    fn output_tokens(&self) -> f64;
    fn threshold_tokens(&self) -> f64;
    fn count_tokens(&self, request: &ModelRequest) -> f64;
}
/// A strategy's diagnostics: its model calls, their summed usage and its own
/// record.
#[derive(Debug, Clone)]
pub struct StrategyDiagnostics {
    pub model_call_count: u64,
    pub model_usage: Option<Usage>,
    pub record: CompactionStrategyRecord,
}
impl StrategyDiagnostics {
    /// Two passes of one compaction: the model calls and the usage summed,
    /// and the later pass's own record, which a strategy continues from
    /// `prior` (ContextCompaction §5.3).
    pub fn sum(&self, next: &Self) -> Self {
        let mut model_usage = self.model_usage.clone();
        if let Some(next) = next.model_usage.clone() {
            add_usage(&mut model_usage, next);
        }
        Self {
            model_call_count: self.model_call_count + next.model_call_count,
            model_usage,
            record: next.record.clone(),
        }
    }
}

/// Adds one reply's usage into a compaction's running sum.
pub(crate) fn add_usage(total: &mut Option<Usage>, usage: Usage) {
    match total {
        None => *total = Some(usage),
        Some(sum) => {
            sum.prompt_tokens += usage.prompt_tokens;
            sum.completion_tokens += usage.completion_tokens;
            sum.total_tokens += usage.total_tokens;
            let cache = |a: Option<f64>, b: Option<f64>| match (a, b) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0.) + b.unwrap_or(0.)),
            };
            sum.cache_read_tokens = cache(sum.cache_read_tokens, usage.cache_read_tokens);
            sum.cache_write_tokens = cache(sum.cache_write_tokens, usage.cache_write_tokens);
        }
    }
}
#[derive(Debug, Clone)]
pub enum StrategyOutcome {
    Completed {
        retain_from: String,
        replacement: Vec<Message>,
        diagnostics: StrategyDiagnostics,
    },
    Skipped {
        reason: String,
        diagnostics: StrategyDiagnostics,
    },
    /// The strategy failed after it ran; the diagnostics carry its calls,
    /// usage and duration (ContextCompaction §4.2).
    Failed {
        error: String,
        diagnostics: StrategyDiagnostics,
    },
}
/// Produces the replacement of the history before a cut. Replay never calls
/// it: a committed result is replayed by its record alone.
pub trait CompactionStrategy: Send + Sync + 'static {
    /// Written into `context/compaction-start` and the result.
    fn name(&self) -> &str;
    /// The planned token count of the messages `compact` returns: the summary's
    /// target plus its wrapper (ContextCompaction §4.2). The retention rule
    /// reserves it before the strategy runs; a reply that overshoots it is
    /// kept and reported (ContextCompaction §5.2).
    fn replacement_size(&self, model: &dyn ContextModel) -> f64;
    fn compact(
        &self,
        input: CompactionInput,
        cancel: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome>;
}
/// An explicit optional binding, like `ContextModelHost`; the policy exists
/// when both are `Some`.
pub struct CompactionStrategyHost(pub Option<Arc<dyn CompactionStrategy>>);
pub trait ResolveContextModel: Send + Sync + 'static {
    fn resolve(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>>;
}
/// An explicit optional policy binding keeps dependencies visible in every profile.
pub struct ContextModelHost(pub Option<Arc<dyn ResolveContextModel>>);

/// A Session file SessionStore stored. `path` is absolute on the Session's
/// resource node and names the same bytes for the file's whole life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionFile {
    pub file_id: String,
    pub path: std::path::PathBuf,
    pub bytes: u64,
}
/// The `sessionFiles` error codes: `not_found`, `invalid`, `too_large`,
/// `node_unavailable`, `cancelled` and `storage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFileErrorCode {
    NotFound,
    Invalid,
    TooLarge,
    NodeUnavailable,
    Cancelled,
    Storage,
}
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct SessionFileError {
    pub code: SessionFileErrorCode,
    pub message: String,
}
/// The process service `sessionFiles`, provided by SessionStore: it stores a
/// Session file and answers which artifacts a Session owns (ADR 013 D1).
pub trait SessionFilesService: Send + Sync + 'static {
    /// Stores `content` once, read-only, as the file `name` owned by
    /// `session_id`. Cancellation takes effect until the file is committed;
    /// afterwards the reply stands.
    fn put(
        &self,
        session_id: String,
        name: String,
        content: Vec<u8>,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<StoredSessionFile, SessionFileError>> + Send + '_>>;
    /// Records `path` as output owned by `session_id`; a repeated path is a
    /// no-op. The caller has checked the path against its own output root.
    fn register(
        &self,
        session_id: String,
        path: std::path::PathBuf,
    ) -> Pin<Box<dyn Future<Output = Result<(), SessionFileError>> + Send + '_>>;
    /// Whether `session_id` owns the artifact at `path`, a resolved absolute
    /// path: an exact registered output path, or an attachment path inside
    /// the directory of an owned file id (ADR 013 D3). A query failure is an
    /// error, which denies access.
    fn owns(
        &self,
        session_id: String,
        path: std::path::PathBuf,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SessionFileError>> + Send + '_>>;
    /// Every path `session_id` owns: a Session file's directory and each
    /// registered output path.
    fn owned_paths(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<std::path::PathBuf>, SessionFileError>> + Send + '_>>;
}
/// The process service `sessionFiles`, provided by SessionStore, so that its
/// consumers name no App Server type.
pub struct SessionFilesHost(pub Arc<dyn SessionFilesService>);

pub struct IdentityContext;
impl PrepareContext for IdentityContext {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        _: ContextSource,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                Err(HostError::Cancelled("Context preparation cancelled".into()))
            } else {
                Ok(request)
            }
        })
    }
}
