use super::error;
use crate::config::ProviderKind;
use aimux_core::options::CallOptions;
use aimux_core::shared::provider_namespace;
use dim_agent_capabilities::context_model::thinking_tokens;
use dim_agent_capabilities::host::HostError;
use dim_agent_types::{AnthropicThinkingType, ModelConfig, ModelRequest};
use serde_json::{Map, Value, json};

/// The floor a thinking budget has on Anthropic and Bedrock, below which the
/// provider rejects the request.
const MIN_THINKING_BUDGET: u32 = 1024;

/// The allowance the split works on, or `None` when the request carries none:
/// such a request keeps the path it had before #1466.
fn allowance_tokens(config: &ModelConfig) -> Option<u64> {
    config
        .max_tokens
        .filter(|max| max.is_finite() && *max >= 1.)
        .map(|max| max as u64)
}

/// The thinking room of the request's allowance: the tokens a reply may spend
/// on thinking once [`answer_tokens`](dim_agent_capabilities::context_model::answer_tokens) is kept for its answer (#1466).
fn thinking_room(config: &ModelConfig) -> Option<u64> {
    allowance_tokens(config).map(thinking_tokens)
}

/// The declared maximum of the model's thinking budget. The adopted snapshot is
/// the only carrier, so a request without one declares none.
fn declared_budget_max(request: &ModelRequest) -> Option<u64> {
    request
        .capability_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.reasoning.as_ref())
        .and_then(|reasoning| reasoning.budget.as_ref())
        .and_then(|budget| budget.max)
        .filter(|max| *max >= 0.0)
        .map(|max| max as u64)
}

/// The declared floor of the model's thinking budget: its declaration's `min`,
/// and at least 1, so a budget never turns thinking off.
fn declared_budget_min(request: &ModelRequest) -> u64 {
    request
        .capability_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.reasoning.as_ref())
        .and_then(|reasoning| reasoning.budget.as_ref())
        .and_then(|budget| budget.min)
        .filter(|min| *min >= 1.0)
        .map(|min| min as u64)
        .unwrap_or(1)
}

/// Whether the configuration states Gemini's own thinking budget, which the
/// split sends as given.
fn native_gemini_budget(config: &ModelConfig) -> bool {
    config
        .provider_options
        .as_ref()
        .and_then(|options| options.gemini.as_ref())
        .and_then(|options| options.thinking_config.as_ref())
        .and_then(|options| options.thinking_budget)
        .is_some()
}

/// Whether the model thinks with a numeric budget rather than a level.
fn declares_budget_mode(request: &ModelRequest) -> bool {
    request
        .capability_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.reasoning.as_ref())
        .is_some_and(|reasoning| reasoning.mode.as_deref() == Some("budget"))
}

/// Whether the model's declaration says thinking is on unless turned off.
fn thinking_on_by_default(request: &ModelRequest) -> bool {
    request
        .capability_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.reasoning.as_ref())
        .is_some_and(|reasoning| reasoning.default_enabled != Some(false))
}

/// The budget Anthropic and Bedrock send, and the `max_tokens` their request
/// carries (#1466).
///
/// The two adapters differ: Anthropic's adds the budget to `max_tokens`, so
/// dim sends the answer room there and the wire cap becomes the allowance;
/// Bedrock's takes the thinking block as given, so dim sends the allowance and
/// reserves the answer by capping the budget.
///
/// `requested` is the configured budget. `None` is an enable without a budget,
/// which asks for the whole room. Both are a budget's own validation first, so
/// a configured value keeps the error it had, and then capped by the room, so
/// the answer never falls below [`answer_tokens`](dim_agent_capabilities::context_model::answer_tokens). `Ok(None)` means the request
/// states no budget at all: it carried none and none was derived.
fn answered_budget(
    request: &ModelRequest,
    config: &ModelConfig,
    kind: &ProviderKind,
    path: &str,
    requested: Option<f64>,
) -> Result<Option<(u32, Option<u32>)>, HostError> {
    // Without an allowance the request keeps the path it had before #1466: a
    // configured budget is validated and sent as it is, with no room to cap.
    let Some(allowance) = allowance_tokens(config) else {
        return match requested {
            Some(budget) => Ok(Some((
                budget_integer(request, path, budget, 1024.)? as u32,
                None,
            ))),
            None => Ok(None),
        };
    };
    let room = thinking_tokens(allowance);
    let asked = match requested {
        Some(budget) => (budget_integer(request, path, budget, 1024.)? as u64).min(room),
        None => room,
    };
    if room < u64::from(MIN_THINKING_BUDGET) {
        return Err(error(
            request,
            path,
            &format!(
                "an allowance of {allowance} tokens leaves {room} tokens to think with, below the {MIN_THINKING_BUDGET} this protocol accepts"
            ),
        ));
    }
    let max_tokens = match kind {
        ProviderKind::Anthropic => Some((allowance - asked) as u32),
        _ => None,
    };
    Ok(Some((asked as u32, max_tokens)))
}

/// The Gemini thinking budget (#1466): the thinking room, lowered to the
/// model's declared maximum and held at or above its declared floor.
///
/// A configured positive budget is validated and then capped by the room. The
/// room is a share of the allowance, so on a small allowance it can fall below
/// the floor: a derived room below the floor sends no budget, so the model keeps
/// its own default, and a configured budget the room lowers below the floor is
/// refused. `None` means the request states no budget, so nothing is added
/// here; a budget the split derives from a model that already thinks is decided
/// by the caller, and a budget the provider declares natively is not touched.
fn gemini_budget(
    request: &ModelRequest,
    config: &ModelConfig,
    path: &str,
    requested: Option<f64>,
) -> Result<Option<i32>, HostError> {
    let Some(room) = thinking_room(config) else {
        // Without an allowance nothing caps the budget, and `-1` may be a
        // dynamic-thinking request, so the value is never narrowed through an
        // unsigned type.
        return Ok(match requested {
            Some(budget) => Some(budget_integer(request, path, budget, -1.)?),
            None => None,
        });
    };
    let floor = declared_budget_min(request);
    let allowance = allowance_tokens(config).unwrap_or(0);
    let mut asked = match requested {
        Some(budget) => {
            let asked = budget_integer(request, path, budget, -1.)? as u64;
            let capped = asked.min(room);
            if capped != asked && capped < floor {
                return Err(error(
                    request,
                    path,
                    &format!(
                        "an allowance of {allowance} tokens leaves {room} tokens to think with, below the {floor} this model accepts"
                    ),
                ));
            }
            // A value the room did not lower is the configuration's own and
            // keeps the path it had: only a derived room is held at the floor.
            if capped == asked && asked < floor {
                return Ok(Some(asked as i32));
            }
            capped
        }
        None => room,
    };
    // A derived room below the floor sends no budget, so the model keeps its
    // own default instead of having thinking turned off.
    if asked < floor {
        return Ok(None);
    }
    if let Some(max) = declared_budget_max(request) {
        asked = asked.min(max);
    }
    Ok(Some(asked as i32))
}

/// Whether a request that states no budget still thinks with one: the model
/// declares budget mode and thinking on, and states no effort. An effort is a
/// `thinkingLevel`, which a budget cannot sit beside, so such a request keeps
/// the level alone. See [`gemini_budget`] for what the answer decides.
fn gemini_thinks_without_budget(request: &ModelRequest, effort: Option<&str>) -> bool {
    declares_budget_mode(request) && thinking_on_by_default(request) && effort.is_none()
}

pub(super) fn apply(
    request: &ModelRequest,
    config: &ModelConfig,
    kind: &ProviderKind,
    options: &mut CallOptions,
) -> Result<(), HostError> {
    let generic = config.reasoning.as_ref();
    let providers = config.provider_options.as_ref();
    let mut fields = Map::new();
    let mut thinking = Map::new();
    if let Some(reasoning) = generic {
        if reasoning.effort.as_deref() == Some("none")
            && (reasoning.enabled.is_some()
                || reasoning.budget_tokens.is_some_and(|budget| budget != 0.0))
        {
            return Err(error(
                request,
                "config.reasoning",
                "conflicting reasoning enable and disable settings",
            ));
        }
        for (field, unsupported) in [
            (
                "budgetTokens",
                reasoning.budget_tokens.is_some()
                    && matches!(
                        kind,
                        ProviderKind::OpenaiResponses
                            | ProviderKind::Xai
                            | ProviderKind::XaiResponses
                    ),
            ),
            (
                "summary",
                reasoning.summary.is_some()
                    && !matches!(
                        kind,
                        ProviderKind::OpenaiResponses | ProviderKind::XaiResponses
                    ),
            ),
            (
                "includeThoughts",
                reasoning.include_thoughts.is_some() && *kind != ProviderKind::Gemini,
            ),
            (
                "interleavedThinking",
                reasoning.interleaved_thinking.is_some(),
            ),
        ] {
            if unsupported {
                return Err(error(
                    request,
                    &format!("config.reasoning.{field}"),
                    "not supported by this protocol",
                ));
            }
        }
        if *kind == ProviderKind::Bedrock
            && reasoning
                .effort
                .as_deref()
                .is_some_and(|effort| effort != "provider-default")
        {
            return Err(error(
                request,
                "config.reasoning.effort",
                "not supported by Bedrock",
            ));
        }
        if let Some(effort) = reasoning
            .effort
            .as_deref()
            .filter(|effort| *effort != "provider-default")
        {
            if effort == "none" && *kind == ProviderKind::Anthropic {
                thinking.insert("type".into(), json!("disabled"));
            } else if effort == "none" && *kind == ProviderKind::Gemini {
                thinking.insert("thinkingBudget".into(), json!(0));
            } else {
                let field = match kind {
                    ProviderKind::OpenaiResponses
                    | ProviderKind::Xai
                    | ProviderKind::XaiResponses => "reasoningEffort",
                    ProviderKind::Anthropic => "effort",
                    ProviderKind::Gemini => "thinkingLevel",
                    ProviderKind::Bedrock => unreachable!(),
                    _ => unreachable!(),
                };
                if *kind == ProviderKind::Gemini {
                    thinking.insert(field.into(), json!(effort));
                } else {
                    fields.insert(field.into(), json!(effort));
                }
            }
        }
        let budget_overridden = match kind {
            ProviderKind::Anthropic => providers
                .and_then(|p| p.anthropic.as_ref())
                .and_then(|p| p.thinking.as_ref())
                .is_some_and(|p| {
                    p.budget_tokens.is_some()
                        || matches!(
                            p.thinking_type,
                            Some(AnthropicThinkingType::Disabled | AnthropicThinkingType::Adaptive)
                        )
                }),
            ProviderKind::Gemini => providers
                .and_then(|p| p.gemini.as_ref())
                .and_then(|p| p.thinking_config.as_ref())
                .is_some_and(|p| p.thinking_budget.is_some()),
            _ => false,
        };
        // A configured budget is validated as it always was, then capped by the
        // thinking room (#1466): it states an upper bound, and the answer room
        // stays reserved above it. A derived budget follows only a
        // configuration that already asked for thinking, so nothing starts
        // thinking because of the split.
        if !budget_overridden {
            let asked = reasoning.enabled.is_some() || reasoning.budget_tokens.is_some();
            match kind {
                ProviderKind::Anthropic | ProviderKind::Bedrock if asked => {
                    let path = if reasoning.budget_tokens.is_some() {
                        "config.reasoning.budgetTokens"
                    } else {
                        "config.reasoning.enabled"
                    };
                    if let Some((budget, max_tokens)) =
                        answered_budget(request, config, kind, path, reasoning.budget_tokens)?
                    {
                        if let Some(max_tokens) = max_tokens {
                            options.max_output_tokens = Some(max_tokens);
                        }
                        thinking.insert("budgetTokens".into(), json!(budget));
                    }
                }
                ProviderKind::Gemini
                    if reasoning.budget_tokens.is_some_and(|budget| budget > 0.0) =>
                {
                    // A configured budget above zero is capped here; zero and
                    // `-1` are Gemini's own off and dynamic settings and go
                    // through the block below, as they always have. A derived
                    // budget is decided after this block, where it also applies
                    // to a configuration that states no reasoning at all, and a
                    // native one is left as it is.
                    if let Some(budget) = gemini_budget(
                        request,
                        config,
                        "config.reasoning.budgetTokens",
                        reasoning.budget_tokens,
                    )? {
                        thinking.insert("thinkingBudget".into(), json!(budget));
                    }
                }
                _ => {}
            }
        }
        // A budget the split does not cap keeps the validation it had: zero or
        // a negative value is not a room, it is a setting to refuse.
        if let Some(budget) = reasoning
            .budget_tokens
            .filter(|_| !budget_overridden)
            .filter(|budget| *budget <= 0.0)
        {
            let minimum = if *kind == ProviderKind::Gemini {
                -1.0
            } else {
                MIN_THINKING_BUDGET as f64
            };
            let budget = budget_integer(request, "config.reasoning.budgetTokens", budget, minimum)?;
            thinking.insert(
                if *kind == ProviderKind::Gemini {
                    "thinkingBudget"
                } else {
                    "budgetTokens"
                }
                .into(),
                json!(budget),
            );
        }
        if let Some(summary) = reasoning.summary {
            fields.insert("reasoningSummary".into(), json!(summary));
        }
        if let Some(include) = reasoning.include_thoughts {
            thinking.insert("includeThoughts".into(), json!(include));
        }
        if *kind == ProviderKind::Anthropic
            && (reasoning.enabled.is_some() || reasoning.budget_tokens.is_some())
        {
            thinking.insert("type".into(), json!("enabled"));
        }
    }
    // A budget-mode Gemini model that thinks sends the thinking room even when
    // the configuration states no budget at all (#1466), so thinking cannot
    // spend the answer's share. A native budget is sent as given, so it skips
    // the derivation, and every other protocol sends nothing here.
    if *kind == ProviderKind::Gemini
        && generic.is_none_or(|reasoning| reasoning.budget_tokens.is_none())
        && !native_gemini_budget(config)
        && gemini_thinks_without_budget(request, generic.and_then(|r| r.effort.as_deref()))
        && let Some(budget) = gemini_budget(request, config, "config.reasoning", None)?
    {
        thinking.insert("thinkingBudget".into(), json!(budget));
    }
    match kind {
        ProviderKind::Xai | ProviderKind::XaiResponses => {
            if config.top_k.is_some() {
                return Err(error(request, "config.topK", "not supported by xAI"));
            }
            if let Some(xai) = providers.and_then(|p| p.xai.as_ref()) {
                if xai.interleaved_thinking.is_some() {
                    return Err(error(
                        request,
                        "config.providerOptions.xai.interleavedThinking",
                        "not supported by xAI",
                    ));
                }
                if let Some(effort) = xai.reasoning_effort.as_deref() {
                    fields.remove("reasoningEffort");
                    if effort != "provider-default" {
                        fields.insert("reasoningEffort".into(), json!(effort));
                    }
                }
            }
            if providers.is_some_and(|p| {
                p.openai.as_ref().is_some_and(|p| p.service_tier.is_some())
                    || p.openai_compatible
                        .as_ref()
                        .is_some_and(|p| p.service_tier.is_some())
            }) {
                return Err(error(
                    request,
                    "config.providerOptions",
                    "service tier is not supported by xAI",
                ));
            }
            if generic.is_some_and(|r| r.enabled.is_some())
                && !fields
                    .get("reasoningEffort")
                    .is_some_and(|v| v.as_str().is_some_and(|e| !e.is_empty()))
            {
                return Err(error(
                    request,
                    "config.reasoning.enabled",
                    "requires a concrete reasoning effort",
                ));
            }
            if *kind == ProviderKind::XaiResponses {
                fields.insert("store".into(), json!(false));
            }
        }
        ProviderKind::OpenaiResponses => {
            if let Some(openai) = providers.and_then(|p| p.openai.as_ref()) {
                if openai.interleaved_thinking.is_some() {
                    return Err(error(
                        request,
                        "config.providerOptions.openai.interleavedThinking",
                        "not supported by Responses",
                    ));
                }
                if let Some(effort) = openai.reasoning_effort.as_deref() {
                    fields.remove("reasoningEffort");
                    if effort != "provider-default" {
                        fields.insert("reasoningEffort".into(), json!(effort));
                    }
                }
                if let Some(summary) = openai.summary {
                    fields.insert("reasoningSummary".into(), json!(summary));
                }
                if let Some(tier) = &openai.service_tier {
                    fields.insert("serviceTier".into(), json!(tier));
                    // The Responses model gates `serviceTier` on the model
                    // id; the catalogue and the account entitlement decide.
                    fields.insert("forceServiceTier".into(), json!(true));
                }
            }
            if generic.is_some_and(|r| r.enabled.is_some())
                && !providers
                    .and_then(|p| p.openai.as_ref())
                    .is_some_and(|p| p.reasoning_effort.as_deref() == Some("none"))
                && !fields
                    .get("reasoningEffort")
                    .is_some_and(|v| v.as_str().is_some_and(|e| e != "none"))
            {
                return Err(error(
                    request,
                    "config.reasoning.enabled",
                    "requires a concrete reasoning effort",
                ));
            }
            // The model decides from its name whether it reasons; an explicit
            // setting makes the alias one, so the setting reaches the wire
            // instead of being dropped. This replaces the 0.5 patch that
            // preserved explicit reasoning for deployment aliases.
            if fields.contains_key("reasoningEffort") || fields.contains_key("reasoningSummary") {
                fields.insert("forceReasoning".into(), json!(true));
            }
            fields.insert("store".into(), json!(false));
            fields.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
        ProviderKind::Anthropic => {
            if let Some(anthropic) = providers.and_then(|p| p.anthropic.as_ref()) {
                if anthropic.interleaved_thinking.is_some() || anthropic.send_reasoning.is_some() {
                    return Err(error(
                        request,
                        "config.providerOptions.anthropic",
                        "sendReasoning and interleavedThinking are not supported by the native adapter",
                    ));
                }
                if let Some(effort) = anthropic.effort.as_deref() {
                    fields.remove("effort");
                    if effort != "provider-default" {
                        fields.insert("effort".into(), json!(effort));
                    }
                }
                if let Some(native) = &anthropic.thinking {
                    if native.thinking_type.is_none() {
                        return Err(error(
                            request,
                            "config.providerOptions.anthropic.thinking.type",
                            "required when thinking is set",
                        ));
                    }
                    if let Some(kind) = native.thinking_type {
                        thinking.insert("type".into(), json!(kind));
                    }
                    if let Some(budget) = native.budget_tokens {
                        thinking.insert(
                            "budgetTokens".into(),
                            json!(budget_integer(
                                request,
                                "config.providerOptions.anthropic.thinking.budgetTokens",
                                budget,
                                1024.0
                            )?),
                        );
                    }
                    if matches!(
                        native.thinking_type,
                        Some(AnthropicThinkingType::Disabled | AnthropicThinkingType::Adaptive)
                    ) {
                        thinking.remove("budgetTokens");
                    }
                    if matches!(
                        native.thinking_type,
                        Some(AnthropicThinkingType::Disabled | AnthropicThinkingType::Adaptive)
                    ) && native.budget_tokens.is_some()
                    {
                        return Err(error(
                            request,
                            "config.providerOptions.anthropic.thinking",
                            "disabled or adaptive thinking cannot have an explicit budget",
                        ));
                    }
                }
            }
            if !thinking.is_empty() {
                fields.insert("thinking".into(), Value::Object(thinking));
            }
        }
        ProviderKind::Gemini => {
            if let Some(native) = providers
                .and_then(|p| p.gemini.as_ref())
                .and_then(|p| p.thinking_config.as_ref())
            {
                if let Some(budget) = native.thinking_budget {
                    thinking.remove("thinkingLevel");
                    thinking.insert(
                        "thinkingBudget".into(),
                        json!(budget_integer(
                            request,
                            "config.providerOptions.gemini.thinkingConfig.thinkingBudget",
                            budget,
                            -1.0
                        )?),
                    );
                }
                if let Some(include) = native.include_thoughts {
                    thinking.insert("includeThoughts".into(), json!(include));
                }
            }
            if generic.is_some_and(|reasoning| reasoning.enabled.is_some())
                && thinking.get("thinkingBudget").and_then(Value::as_i64) == Some(0)
                && providers
                    .and_then(|p| p.gemini.as_ref())
                    .and_then(|p| p.thinking_config.as_ref())
                    .is_none_or(|p| p.thinking_budget.is_none())
            {
                return Err(error(
                    request,
                    "config.reasoning",
                    "conflicting reasoning enable and disable settings",
                ));
            }
            if thinking.contains_key("thinkingBudget") && thinking.contains_key("thinkingLevel") {
                return Err(error(
                    request,
                    "config.reasoning",
                    "Gemini cannot combine a thinking level and budget",
                ));
            }
            if generic.is_some_and(|r| r.enabled.is_some())
                && !thinking.contains_key("thinkingBudget")
                && !thinking.contains_key("thinkingLevel")
            {
                return Err(error(
                    request,
                    "config.reasoning.enabled",
                    "requires a thinking budget or level",
                ));
            }
            if !thinking.is_empty() {
                fields.insert("thinkingConfig".into(), Value::Object(thinking));
            }
        }
        ProviderKind::Bedrock => {
            if config.top_k.is_some() {
                return Err(error(request, "config.topK", "not supported by Bedrock"));
            }
            if providers.is_some() {
                return Err(error(
                    request,
                    "config.providerOptions",
                    "provider-specific options do not match Bedrock",
                ));
            }
            if generic.is_some_and(|reasoning| reasoning.enabled.is_some())
                && !thinking.contains_key("budgetTokens")
            {
                return Err(error(
                    request,
                    "config.reasoning.enabled",
                    "requires a reasoning budget for Bedrock",
                ));
            }
            if let Some(budget) = thinking.remove("budgetTokens") {
                // The Converse API takes the thinking block itself: the
                // portable reasoning path would also add the budget to
                // maxTokens, which is not what this request states.
                fields.insert(
                    "additionalModelRequestFields".into(),
                    json!({"thinking": {"type":"enabled", "budget_tokens":budget}}),
                );
            }
        }
        _ => unreachable!(),
    }
    let namespace = match kind {
        ProviderKind::Xai | ProviderKind::XaiResponses => "xai",
        ProviderKind::OpenaiResponses => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Gemini => "google",
        ProviderKind::Bedrock => "bedrock",
        _ => unreachable!(),
    };
    if !fields.is_empty() {
        options.provider_options = Some(
            provider_namespace(namespace, Value::Object(fields))
                .expect("native reasoning fields are an object"),
        );
    }
    Ok(())
}

fn budget_integer(
    request: &ModelRequest,
    path: &str,
    value: f64,
    minimum: f64,
) -> Result<i32, HostError> {
    if !value.is_finite()
        || value.fract() != 0.0
        || !(minimum..=f64::from(i32::MAX)).contains(&value)
    {
        return Err(error(request, path, "invalid integer reasoning budget"));
    }
    Ok(value as i32)
}
