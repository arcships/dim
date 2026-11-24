use crate::config::{ModelConfigFile, ProviderKind};
use dim_agent_capabilities::context_model::{output_allowance, window_facts};
use dim_agent_capabilities::host::{ContextModel, HostError, HostFuture, ResolveContextModel};
use dim_agent_runtime::ModelId;
use dim_agent_types::{ModelCapabilities, ModelCapabilitySnapshot, ModelConfig, ModelRequest};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct Model {
    pub base_url: String,
    pub headers: HashMap<String, String>,
    pub api_key_env: Option<String>,
    pub defaults: ModelConfig,
    pub kind: ProviderKind,
    pub preset: Option<String>,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub output_allowance: u64,
}

/// Validated development catalog; credentials are resolved for each request.
pub struct AimuxModels {
    default_model: ModelId,
    providers: HashMap<String, HashMap<String, Model>>,
}
impl std::fmt::Debug for AimuxModels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AimuxModels")
            .field("default_model", &self.default_model)
            .field("provider_count", &self.providers.len())
            .finish_non_exhaustive()
    }
}
impl AimuxModels {
    pub fn from_config(config: ModelConfigFile) -> Result<Self, HostError> {
        let mut providers = HashMap::new();
        for provider in config.providers {
            if provider.kind == ProviderKind::Bedrock {
                return Err(HostError::failed(
                    "Bedrock is not supported by the development model configuration",
                ));
            }
            if provider.id.is_empty() || providers.contains_key(&provider.id) {
                return Err(HostError::failed(
                    "Provider IDs must be nonempty and unique",
                ));
            }
            let url = url::Url::parse(&provider.base_url)
                .map_err(|_| HostError::failed(format!("Invalid baseUrl for {}", provider.id)))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(HostError::failed(format!(
                    "Invalid baseUrl for {}",
                    provider.id
                )));
            }
            for (name, value) in &provider.headers {
                if http::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                    || http::header::HeaderValue::from_str(value).is_err()
                {
                    return Err(HostError::failed(format!(
                        "Invalid header for {}",
                        provider.id
                    )));
                }
            }
            let preset = provider.compatible.and_then(|value| value.preset);
            if let Some(preset) = &preset {
                if !matches!(provider.kind, ProviderKind::OpenaiCompatible) {
                    return Err(HostError::failed(
                        "compatible.preset requires openai-compatible",
                    ));
                }
                if !crate::driver::compatible_preset(preset) {
                    return Err(HostError::failed(format!(
                        "Unknown compatible preset: {preset}"
                    )));
                }
            }
            let mut models = HashMap::new();
            for entry in provider.models {
                if entry.id.is_empty() || models.contains_key(&entry.id) {
                    return Err(HostError::failed(format!(
                        "Model IDs for {} must be nonempty and unique",
                        provider.id
                    )));
                }
                if entry.config.max_tokens.is_none() {
                    return Err(HostError::failed(format!(
                        "{}.{}.config.maxTokens is required",
                        provider.id, entry.id
                    )));
                }
                let Some(context_window) = entry.context_window.filter(|value| *value > 0) else {
                    return Err(HostError::failed(format!(
                        "{}.{}.contextWindow must be a positive integer",
                        provider.id, entry.id
                    )));
                };
                let Some(max_output_tokens) = entry.max_output_tokens.filter(|value| *value > 0)
                else {
                    return Err(HostError::failed(format!(
                        "{}.{}.maxOutputTokens must be a positive integer",
                        provider.id, entry.id
                    )));
                };
                if entry
                    .config
                    .temperature
                    .is_some_and(|value| !value.is_finite() || value < 0.)
                    || entry
                        .config
                        .top_p
                        .is_some_and(|value| !(0. ..=1.).contains(&value))
                    || entry.config.top_k.is_some_and(|value| {
                        !value.is_finite()
                            || value < 1.
                            || value.fract() != 0.
                            || value > 9_007_199_254_740_991.
                    })
                {
                    return Err(HostError::failed(format!(
                        "Invalid generation settings for {}/{}",
                        provider.id, entry.id
                    )));
                }
                let mut request = ModelRequest::new(&provider.id, &entry.id, Vec::new());
                let allowance = output_allowance(context_window, max_output_tokens);
                let mut defaults = entry.config;
                defaults.max_tokens = Some(
                    defaults
                        .max_tokens
                        .map_or(allowance as f64, |max| max.min(allowance as f64)),
                );
                request.config = Some(defaults.clone());
                crate::request::convert(&request, &provider.kind, preset.as_deref())?;
                models.insert(
                    entry.id,
                    Model {
                        base_url: provider.base_url.clone(),
                        headers: provider.headers.clone(),
                        api_key_env: provider.api_key_env.clone(),
                        defaults,
                        kind: provider.kind,
                        preset: preset.clone(),
                        context_window,
                        max_output_tokens,
                        output_allowance: allowance,
                    },
                );
            }
            providers.insert(provider.id, models);
        }
        let models = Self {
            default_model: config.default_model,
            providers,
        };
        models.get(
            &models.default_model.provider_id,
            &models.default_model.model_id,
        )?;
        Ok(models)
    }

    pub fn default_model(&self) -> &ModelId {
        &self.default_model
    }

    /// The environment variable names this catalog reads provider credentials
    /// from, sorted and deduplicated across every model.
    pub fn api_key_envs(&self) -> std::collections::BTreeSet<String> {
        self.providers
            .values()
            .flat_map(|models| models.values())
            .filter_map(|model| model.api_key_env.clone())
            .collect()
    }

    /// The models this configuration serves, by provider and then model id:
    /// what the `selectableModels` read answers. The file declares the two
    /// limits and no other capability, so every entry carries the text
    /// defaults and those limits.
    pub(crate) fn selectable(&self) -> Vec<ModelCapabilitySnapshot> {
        let mut entries: Vec<(&str, &str, &Model)> = self
            .providers
            .iter()
            .flat_map(|(provider, models)| {
                models
                    .iter()
                    .map(move |(model, entry)| (provider.as_str(), model.as_str(), entry))
            })
            .collect();
        entries.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        entries
            .into_iter()
            .map(|(provider_id, model_id, model)| ModelCapabilitySnapshot {
                target: ModelId {
                    provider_id: provider_id.to_owned(),
                    model_id: model_id.to_owned(),
                },
                capabilities: ModelCapabilities::with_limits(
                    model.context_window,
                    model.max_output_tokens,
                ),
                reasoning: None,
                service_tier: None,
            })
            .collect()
    }

    pub(crate) fn get(&self, provider: &str, model: &str) -> Result<&Model, HostError> {
        self.providers
            .get(provider)
            .and_then(|models| models.get(model))
            .ok_or_else(|| {
                HostError::failed(format!("Model is not configured: {provider}/{model}"))
            })
    }
}

/// The model-file profile's window facts: the entry the request names, by
/// provider and model id (ContextCompaction §5.1).
impl ResolveContextModel for AimuxModels {
    fn resolve(
        &self,
        request: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move {
            let model = self.get(&request.provider_id, &request.model_id)?;
            Ok(window_facts(
                model.context_window,
                model.output_allowance as f64,
                &request,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    #[test]
    fn api_key_envs_are_the_sorted_unique_names_the_catalog_reads() {
        let config: ModelConfigFile = serde_json::from_value(json!({
            "defaultModel": {"providerId": "local", "modelId": "one"},
            "providers": [
                {
                    "id": "local",
                    "kind": "openai-compatible",
                    "apiKeyEnv": "LOCAL_KEY",
                    "baseUrl": "https://example.test/v1",
                    "models": [
                        {"id": "one", "contextWindow": 128000, "maxOutputTokens": 4096,
                         "config": {"maxTokens": 4096}},
                        {"id": "two", "contextWindow": 128000, "maxOutputTokens": 4096,
                         "config": {"maxTokens": 4096}}
                    ]
                },
                {
                    "id": "other",
                    "kind": "openai-compatible",
                    "apiKeyEnv": "OTHER_KEY",
                    "baseUrl": "https://example.test/v1",
                    "models": [
                        {"id": "two", "contextWindow": 128000, "maxOutputTokens": 4096,
                         "config": {"maxTokens": 4096}}
                    ]
                }
            ]
        }))
        .unwrap();
        let models = AimuxModels::from_config(config).unwrap();
        assert_eq!(
            models.api_key_envs(),
            BTreeSet::from(["LOCAL_KEY".to_owned(), "OTHER_KEY".to_owned()])
        );
    }

    /// The development file's `compatible.preset` is the vendor package the
    /// compatible protocol builds, so it reaches the image rule too: an xAI
    /// model takes only PNG and JPEG, and a GIF becomes its reference.
    #[test]
    fn the_model_config_xai_preset_reaches_the_image_rule() {
        let config: ModelConfigFile = serde_json::from_value(json!({
            "defaultModel": {"providerId": "local", "modelId": "one"},
            "providers": [{
                "id": "local",
                "kind": "openai-compatible",
                "baseUrl": "https://api.x.ai/v1",
                "compatible": {"preset": "xai"},
                "models": [{"id": "one", "contextWindow": 128000, "maxOutputTokens": 4096,
                            "config": {"maxTokens": 4096}}]
            }]
        }))
        .unwrap();
        let models = AimuxModels::from_config(config).unwrap();
        let model = models.get("local", "one").unwrap();
        assert_eq!(model.preset.as_deref(), Some("xai"));
        let request: ModelRequest = serde_json::from_value(json!({
            "providerId": "local", "modelId": "one",
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "mediaType": "image/gif",
                 "data": "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"}}
            ]}]
        }))
        .unwrap();
        let prepared =
            crate::request::convert(&request, &model.kind, model.preset.as_deref()).unwrap();
        let wire = serde_json::to_string(&prepared.options).unwrap();
        assert!(
            !wire.contains("image/gif"),
            "the configured xAI destination takes no GIF as pixels: {wire}"
        );
        assert!(
            wire.contains("img:"),
            "the GIF becomes its reference: {wire}"
        );
    }
}
