//! Window facts of a profile that compacts, from the model's declared limits
//! with a local token estimate (ContextCompaction §5.1).
//! The trigger reserve is a provisional heuristic (#128).
use crate::host::*;
use dim_agent_types::*;
use std::{io, sync::Arc};
use tokio_util::sync::CancellationToken;

/// The trigger leaves `RESERVE_RATIO` of the input budget, at least
/// `RESERVE_MIN_TOKENS` and at most `RESERVE_MAX_RATIO` of it, for the
/// estimation error of the next request.
const RESERVE_RATIO: f64 = 0.10;
const RESERVE_MIN_TOKENS: f64 = 16_000.;
const RESERVE_MAX_RATIO: f64 = 0.20;
/// No reply needs more than this, however much the model declares.
const OUTPUT_CEILING: u64 = 131_072;
/// The share of the output allowance a reply keeps for its answer beside
/// thinking, with `ANSWER_MIN_TOKENS` as the floor and half the allowance as
/// the cap. Provisional, like the trigger reserve above (#1466).
const ANSWER_SHARE: f64 = 0.25;
const ANSWER_MIN_TOKENS: f64 = 4_096.;
/// Serialized UTF-8 bytes per estimated token.
const BYTES_PER_TOKEN: f64 = 4.;
/// Estimated tokens of one image part.
const IMAGE_TOKENS: f64 = 1_600.;

/// The adopted output allowance of a model: its declared maximum output,
/// bounded by [`OUTPUT_CEILING`] and by half the window, so the input budget
/// `B = context_window − allowance` is at least half the window even when a
/// source declares an output at or above it (ContextCompaction §5.1). The
/// declared maximum stays the capability fact it is.
pub fn output_allowance(context_window: u64, declared_max_output: u64) -> u64 {
    declared_max_output
        .min(OUTPUT_CEILING)
        .min(context_window / 2)
        .max(1)
}

/// The part of the output allowance a reply keeps for its answer when it may
/// think (#1466): a quarter of the allowance, at least 4096 tokens, never more
/// than half of it, and at least 1. Thinking can therefore never take the whole
/// allowance, and `allowance - answer_tokens` is the thinking room.
pub fn answer_tokens(allowance: u64) -> u64 {
    let allowance_f = allowance as f64;
    (1_u64)
        .max((ANSWER_SHARE * allowance_f).round() as u64)
        .max(ANSWER_MIN_TOKENS.min(allowance_f / 2.) as u64)
        .min(allowance)
}

/// The part of the output allowance a reply may spend on thinking, which is
/// every token `answer_tokens` does not reserve.
pub fn thinking_tokens(allowance: u64) -> u64 {
    allowance.saturating_sub(answer_tokens(allowance))
}

/// The configuration a stored adoption yields at a request boundary: the
/// stored output allowance, repaired to [`output_allowance`] when it is
/// absent or above the rule. The stored fact is not rewritten, so a Session
/// adopted before the rule existed stays readable and usable.
pub fn adopted_config(
    snapshot: Option<&ModelCapabilitySnapshot>,
    config: Option<&ModelConfig>,
) -> Option<ModelConfig> {
    let Some(capabilities) = snapshot.map(|snapshot| &snapshot.capabilities) else {
        return config.cloned();
    };
    let allowance =
        output_allowance(capabilities.context_window, capabilities.max_output_tokens) as f64;
    let mut config = config.cloned().unwrap_or_default();
    config.max_tokens = Some(
        config
            .max_tokens
            .filter(|stored| *stored > 0.)
            .map_or(allowance, |stored| stored.min(allowance)),
    );
    Some(config)
}

/// Resolves window facts from the request alone; no service is needed.
pub struct ProductContextModel;
impl ResolveContextModel for ProductContextModel {
    fn resolve(
        &self,
        request: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move {
            let snapshot = request.capability_snapshot.as_ref().ok_or_else(|| {
                HostError::classified(
                    HostErrorKind::InvalidRequest,
                    "Model capabilities are not adopted; select the model again",
                )
            })?;
            snapshot
                .validate_for(&request.provider_id, &request.model_id)
                .map_err(|message| HostError::classified(HostErrorKind::InvalidRequest, message))?;
            let allowance = request
                .config
                .as_ref()
                .and_then(|config| config.max_tokens)
                .ok_or_else(|| {
                    HostError::classified(
                        HostErrorKind::InvalidRequest,
                        "Model output allowance is not adopted; select the model again",
                    )
                })?;
            Ok(window_facts(
                snapshot.capabilities.context_window,
                allowance,
                &request,
            ))
        })
    }
}

/// Window facts for a model whose adopted output allowance is `output_tokens`:
/// its context window and the allowance every request reserves.
pub fn window_facts(
    context_window: u64,
    output_tokens: f64,
    request: &ModelRequest,
) -> Arc<dyn ContextModel> {
    Arc::new(WindowFacts {
        context_window: context_window as f64,
        output_tokens,
        auto_compact_window: request
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.startup.as_ref())
            .and_then(|startup| startup.auto_compact_window),
    })
}

struct WindowFacts {
    context_window: f64,
    output_tokens: f64,
    auto_compact_window: Option<u64>,
}
impl ContextModel for WindowFacts {
    fn context_window(&self) -> f64 {
        self.context_window
    }
    fn output_tokens(&self) -> f64 {
        self.output_tokens
    }
    fn threshold_tokens(&self) -> f64 {
        let budget = (self.context_window - self.output_tokens).max(0.);
        let threshold = budget
            - (RESERVE_RATIO * budget)
                .max(RESERVE_MIN_TOKENS)
                .min(RESERVE_MAX_RATIO * budget);
        self.auto_compact_window.map_or(threshold, |window| {
            threshold.min((window as f64 - self.output_tokens).max(0.))
        })
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        let mut contribution = self.fixed(
            request.system.as_deref(),
            request.tools.as_deref().map(Vec::as_slice),
        );
        for message in &request.messages {
            let next = self.message(message);
            contribution.serialized_bytes += next.serialized_bytes;
            contribution.images += next.images;
        }
        self.estimate(contribution)
    }
    fn contribution_counter(&self) -> Option<&dyn ContextCounter> {
        Some(self)
    }
}

impl ContextCounter for WindowFacts {
    fn algorithm(&self) -> &str {
        "dim/window-facts/v1"
    }

    fn fixed(
        &self,
        system: Option<&SystemPrompt>,
        tools: Option<&[ToolDefinition]>,
    ) -> ContextContribution {
        ContextContribution {
            serialized_bytes: size(&system) + size(&tools),
            images: 0,
        }
    }

    fn message(&self, message: &Message) -> ContextContribution {
        let mut contribution = ContextContribution::default();
        match &message.content {
            MessageContent::Text(_) => contribution.serialized_bytes += size(message),
            MessageContent::Parts(parts) => {
                for part in parts {
                    match part {
                        MessagePart::Image { .. } => contribution.images += 1,
                        MessagePart::ToolResult {
                            content: ToolContent::Blocks(blocks),
                            ..
                        } => {
                            for block in blocks {
                                match block {
                                    ContentBlock::Image { .. } => contribution.images += 1,
                                    block => contribution.serialized_bytes += size(block),
                                }
                            }
                        }
                        part => contribution.serialized_bytes += size(part),
                    }
                }
            }
        }
        contribution
    }

    fn estimate(&self, contribution: ContextContribution) -> f64 {
        contribution.serialized_bytes as f64 / BYTES_PER_TOKEN
            + contribution.images as f64 * IMAGE_TOKENS
    }
}

struct SerializedSize(u64);

impl io::Write for SerializedSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .expect("serialized context size fits u64");
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn size(value: &impl serde::Serialize) -> u64 {
    let mut size = SerializedSize(0);
    serde_json::to_writer(&mut size, value).expect("model request parts serialize");
    size.0
}
