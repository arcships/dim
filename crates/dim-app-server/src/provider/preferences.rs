use dim_agent_capabilities::host::{ModelProtocol, reasoning_config};
use dim_agent_types::*;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A declared option group that carries no preference control must not
    /// leave an empty group behind, or a no-op clear would look adopted.
    #[test]
    fn a_declared_group_without_controls_leaves_the_config_unchanged() {
        let declared = ModelConfig {
            provider_options: Some(ProviderOptions {
                anthropic: Some(Default::default()),
                openai_compatible: Some(Default::default()),
                openai: Some(Default::default()),
                xai: Some(Default::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        for protocol in [
            ModelProtocol::Anthropic,
            ModelProtocol::OpenaiCompatible,
            ModelProtocol::OpenaiChat,
            ModelProtocol::OpenaiResponses,
            ModelProtocol::Xai,
            ModelProtocol::XaiResponses,
        ] {
            let mut config = ModelConfig::default();
            restore_reasoning(&mut config, &declared, protocol);
            restore_service_tier(&mut config, &declared, protocol);
            assert_eq!(config, ModelConfig::default(), "{protocol:?}");
        }
    }

    #[test]
    fn a_declared_group_with_an_unrelated_field_does_not_create_one() {
        let declared = ModelConfig {
            provider_options: Some(ProviderOptions {
                anthropic: Some(AnthropicProviderOptions {
                    interleaved_thinking: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut config = ModelConfig::default();
        restore_reasoning(&mut config, &declared, ModelProtocol::Anthropic);
        assert_eq!(config, ModelConfig::default());
    }

    /// A clear reports the values the model declares, as a `--model-config`
    /// model carrying an effort, a thinking declaration and a tier does.
    #[test]
    fn a_clear_restores_the_declared_values() {
        let declared = ModelConfig {
            reasoning: Some(ProviderReasoningConfig {
                effort: Some("low".into()),
                budget_tokens: Some(2048.),
                ..Default::default()
            }),
            provider_options: Some(ProviderOptions {
                openai_compatible: Some(OpenAiCompatibleProviderOptions {
                    reasoning_effort: Some("low".into()),
                    service_tier: Some("priority".into()),
                    thinking: Some(CompatibleThinking {
                        thinking_type: Some(ThinkingType::Disabled),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut config = ModelConfig {
            reasoning: Some(ProviderReasoningConfig {
                effort: Some("high".into()),
                summary: Some(SummaryStyle::Auto),
                ..Default::default()
            }),
            provider_options: Some(ProviderOptions {
                openai_compatible: Some(OpenAiCompatibleProviderOptions {
                    reasoning_effort: Some("high".into()),
                    service_tier: Some("default".into()),
                    enable_thinking: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        restore_reasoning(&mut config, &declared, ModelProtocol::OpenaiCompatible);
        restore_service_tier(&mut config, &declared, ModelProtocol::OpenaiCompatible);
        let options = config
            .provider_options
            .as_ref()
            .unwrap()
            .openai_compatible
            .as_ref()
            .unwrap();
        assert_eq!(options.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(
            options.thinking,
            Some(CompatibleThinking {
                thinking_type: Some(ThinkingType::Disabled)
            })
        );
        assert_eq!(options.enable_thinking, None);
        assert_eq!(options.service_tier.as_deref(), Some("priority"));
        let reasoning = config.reasoning.as_ref().unwrap();
        assert_eq!(reasoning.effort.as_deref(), Some("low"));
        assert_eq!(reasoning.budget_tokens, Some(2048.));
        assert_eq!(reasoning.summary, Some(SummaryStyle::Auto));
    }
}
