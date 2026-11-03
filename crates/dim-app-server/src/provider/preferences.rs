use dim_agent_capabilities::host::{ModelProtocol, adopted_preferences, reasoning_config};
use dim_agent_types::*;

/// The base configuration an adoption starts from: the model's declared
/// configuration with the catalog's own defaults applied.
///
/// `docs/design/appserver/provider-management.md` §3.3 says omitted
/// preferences "still start at catalog defaults", and
/// `docs/design/model-configuration.md` says a cleared field carries "the
/// value an explicit adoption of the same model with no preference for that
/// field would carry". The declared reasoning effort and service tier are
/// therefore part of that base, applied through the same mapping an explicit
/// preference uses, so the adopted configuration, every read and every
/// request agree on the value.
///
/// A declared default applies only when the model declares the control: the
/// effort is one of the picker's `effortOptions` (or `none` beside a
/// reasoning-off declaration), the tier is supported and listed, and the
/// protocol can express the value. Otherwise the base comes back unchanged.
/// Unlike an explicit preference, a default never makes an adoption fail.
pub(super) fn catalog_defaults(
    base: &ModelConfig,
    snapshot: &ModelCapabilitySnapshot,
    protocol: ModelProtocol,
) -> ModelConfig {
    let mut config = base.clone();
    let adopted = adopted_preferences(&config, protocol);
    if adopted
        .as_ref()
        .is_none_or(|adopted| adopted.reasoning_effort.is_none())
        && let Some(effort) = declared_effort(snapshot)
        && let Ok(applied) = reasoning_config(&config, snapshot, protocol, &effort)
    {
        config = applied;
    }
    if adopted
        .as_ref()
        .is_none_or(|adopted| adopted.service_tier.is_none())
        && let Some(tier) = declared_service_tier(snapshot)
    {
        let preferences = ModelPreferences {
            reasoning_effort: None,
            service_tier: Some(tier),
        };
        if let Ok(applied) = configure(&preferences, snapshot, protocol, &config) {
            config = applied;
        }
    }
    config
}

/// The model's declared default effort, when the picker would offer it.
fn declared_effort(snapshot: &ModelCapabilitySnapshot) -> Option<String> {
    let reasoning = snapshot.reasoning.as_ref()?;
    if !reasoning.supported || !snapshot.capabilities.reasoning {
        return None;
    }
    let effort = reasoning.effort.as_ref()?;
    let mut options = reasoning.effort_options.clone().unwrap_or_default();
    if reasoning.off.is_some() && !options.iter().any(|option| option == "none") {
        options.push("none".into());
    }
    options
        .iter()
        .any(|option| option == effort)
        .then(|| effort.clone())
}

/// The model's declared default service tier, when its declaration lists it.
fn declared_service_tier(snapshot: &ModelCapabilitySnapshot) -> Option<String> {
    let tier = snapshot.service_tier.as_ref()?;
    let default = tier.default.as_ref()?;
    (tier.supported && !default.is_empty() && tier.options.contains(default))
        .then(|| default.clone())
}

/// Applies preferences to a copy of `base`, validating them against the
/// model's declared capabilities.
pub(super) fn configure(
    preferences: &ModelPreferences,
    snapshot: &ModelCapabilitySnapshot,
    protocol: ModelProtocol,
    base: &ModelConfig,
) -> Result<ModelConfig, &'static str> {
    if protocol == ModelProtocol::Bedrock && *preferences != ModelPreferences::default() {
        return Err("Model preferences are not supported by Bedrock");
    }
    let mut config = match &preferences.reasoning_effort {
        Some(effort) => reasoning_config(base, snapshot, protocol, effort)?,
        None => base.clone(),
    };
    if let Some(tier) = &preferences.service_tier {
        let supported = snapshot
            .service_tier
            .as_ref()
            .is_some_and(|s| s.supported && !tier.is_empty() && s.options.contains(tier));
        if !supported {
            return Err("Service tier is not supported by this model");
        }
        let options = config.provider_options.get_or_insert_default();
        match protocol {
            ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => {
                options.openai.get_or_insert_default().service_tier = Some(tier.clone());
            }
            ModelProtocol::OpenaiCompatible => {
                options
                    .openai_compatible
                    .get_or_insert_default()
                    .service_tier = Some(tier.clone());
            }
            _ => return Err("Service tier is not supported by this protocol"),
        }
    }
    Ok(config)
}

fn restore<T: Clone>(current: &mut Option<T>, declared: Option<&T>) {
    *current = declared.cloned();
}

/// The protocol's own option struct, created from the declared configuration
/// when it carries one and otherwise only reached when the adopted one exists.
fn options_for<'a, T: Default>(
    config: &'a mut ModelConfig,
    declared: Option<&T>,
    slot: impl FnOnce(&mut ProviderOptions) -> &mut Option<T>,
) -> Option<&'a mut T> {
    match declared {
        Some(_) => {
            Some(slot(config.provider_options.get_or_insert_default()).get_or_insert_default())
        }
        None => slot(config.provider_options.as_mut()?).as_mut(),
    }
}

/// Puts the reasoning controls [`configure`] replaces back to the model's
/// declared configuration, so a cleared effort carries what an explicit
/// adoption of the same model would.
pub(super) fn restore_reasoning(
    config: &mut ModelConfig,
    declared: &ModelConfig,
    protocol: ModelProtocol,
) {
    let declared_reasoning = declared.reasoning.as_ref();
    let reasoning = config.reasoning.get_or_insert_default();
    restore(
        &mut reasoning.effort,
        declared_reasoning.and_then(|value| value.effort.as_ref()),
    );
    restore(
        &mut reasoning.enabled,
        declared_reasoning.and_then(|value| value.enabled.as_ref()),
    );
    restore(
        &mut reasoning.budget_tokens,
        declared_reasoning.and_then(|value| value.budget_tokens.as_ref()),
    );
    if *reasoning == ProviderReasoningConfig::default() {
        config.reasoning = None;
    }
    let declared_options = declared.provider_options.as_ref();
    match protocol {
        ModelProtocol::Anthropic => {
            let declared = declared_options
                .and_then(|options| options.anthropic.as_ref())
                .filter(|value| value.effort.is_some() || value.thinking.is_some());
            if let Some(options) = options_for(config, declared, |options| &mut options.anthropic) {
                restore(
                    &mut options.effort,
                    declared.and_then(|value| value.effort.as_ref()),
                );
                restore(
                    &mut options.thinking,
                    declared.and_then(|value| value.thinking.as_ref()),
                );
            }
        }
        ModelProtocol::OpenaiCompatible => {
            let declared = declared_options
                .and_then(|options| options.openai_compatible.as_ref())
                .filter(|value| {
                    value.reasoning_effort.is_some()
                        || value.thinking.is_some()
                        || value.enable_thinking.is_some()
                });
            if let Some(options) =
                options_for(config, declared, |options| &mut options.openai_compatible)
            {
                restore(
                    &mut options.reasoning_effort,
                    declared.and_then(|value| value.reasoning_effort.as_ref()),
                );
                restore(
                    &mut options.thinking,
                    declared.and_then(|value| value.thinking.as_ref()),
                );
                restore(
                    &mut options.enable_thinking,
                    declared.and_then(|value| value.enable_thinking.as_ref()),
                );
            }
        }
        ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => {
            let declared = declared_options
                .and_then(|options| options.openai.as_ref())
                .filter(|value| value.reasoning_effort.is_some());
            if let Some(options) = options_for(config, declared, |options| &mut options.openai) {
                restore(
                    &mut options.reasoning_effort,
                    declared.and_then(|value| value.reasoning_effort.as_ref()),
                );
            }
        }
        ModelProtocol::Xai | ModelProtocol::XaiResponses => {
            let declared = declared_options
                .and_then(|options| options.xai.as_ref())
                .filter(|value| value.reasoning_effort.is_some());
            if let Some(options) = options_for(config, declared, |options| &mut options.xai) {
                restore(
                    &mut options.reasoning_effort,
                    declared.and_then(|value| value.reasoning_effort.as_ref()),
                );
            }
        }
        ModelProtocol::Gemini => {
            let declared = declared_options
                .and_then(|options| options.gemini.as_ref())
                .and_then(|options| options.thinking_config.as_ref())
                .and_then(|config| config.thinking_budget);
            match declared {
                Some(budget) => {
                    config
                        .provider_options
                        .get_or_insert_default()
                        .gemini
                        .get_or_insert_default()
                        .thinking_config
                        .get_or_insert_default()
                        .thinking_budget = Some(budget);
                }
                None => {
                    if let Some(options) = config
                        .provider_options
                        .as_mut()
                        .and_then(|options| options.gemini.as_mut())
                        .and_then(|options| options.thinking_config.as_mut())
                    {
                        options.thinking_budget = None;
                    }
                }
            }
        }
        ModelProtocol::Bedrock => {}
    }
}

/// Puts the service-tier control [`configure`] replaces back to the model's
/// declared configuration.
pub(super) fn restore_service_tier(
    config: &mut ModelConfig,
    declared: &ModelConfig,
    protocol: ModelProtocol,
) {
    let declared_options = declared.provider_options.as_ref();
    match protocol {
        ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => {
            let declared = declared_options
                .and_then(|options| options.openai.as_ref())
                .filter(|value| value.service_tier.is_some());
            if let Some(options) = options_for(config, declared, |options| &mut options.openai) {
                restore(
                    &mut options.service_tier,
                    declared.and_then(|value| value.service_tier.as_ref()),
                );
            }
        }
        ModelProtocol::OpenaiCompatible => {
            let declared = declared_options
                .and_then(|options| options.openai_compatible.as_ref())
                .filter(|value| value.service_tier.is_some());
            if let Some(options) =
                options_for(config, declared, |options| &mut options.openai_compatible)
            {
                restore(
                    &mut options.service_tier,
                    declared.and_then(|value| value.service_tier.as_ref()),
                );
            }
        }
        _ => {}
    }
}
