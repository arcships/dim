//! Stateless vendor routing. Provider owns configuration; Aimux owns execution.
use super::new_api;
use crate::model_catalog::{ModelEntry, NewApiEndpoint};
use dim_agent_capabilities::host::{HostError, HttpModelConnection, ModelProtocol};
use std::collections::{BTreeMap, HashMap};

const DRIVERS: &[(&str, Option<ModelProtocol>)] = &[
    ("openai-compatible", Some(ModelProtocol::OpenaiCompatible)),
    ("custom", Some(ModelProtocol::OpenaiCompatible)),
    ("xai-grok-build", Some(ModelProtocol::OpenaiCompatible)),
    ("deepseek", Some(ModelProtocol::OpenaiCompatible)),
    ("moonshotai", Some(ModelProtocol::OpenaiCompatible)),
    ("aihubmix", Some(ModelProtocol::OpenaiCompatible)),
    ("zenmux", Some(ModelProtocol::OpenaiCompatible)),
    ("openai-responses", Some(ModelProtocol::OpenaiResponses)),
    ("openai-codex", Some(ModelProtocol::OpenaiResponses)),
    ("aihubmix-responses", Some(ModelProtocol::OpenaiResponses)),
    ("azure-openai", Some(ModelProtocol::OpenaiResponses)),
    ("xai", Some(ModelProtocol::Xai)),
    ("xai-responses", Some(ModelProtocol::XaiResponses)),
    ("anthropic", Some(ModelProtocol::Anthropic)),
    ("gemini", Some(ModelProtocol::Gemini)),
    ("amazon-bedrock", Some(ModelProtocol::Bedrock)),
    ("new-api-router", None),
];

pub(super) fn supported(driver: &str) -> bool {
    DRIVERS.iter().any(|(name, _)| *name == driver)
}

pub(super) fn custom_drivers() -> impl Iterator<Item = &'static str> {
    DRIVERS
        .iter()
        .map(|(driver, _)| *driver)
        .filter(|driver| !matches!(*driver, "openai-codex" | "xai-grok-build"))
}

pub(super) fn remote_model_list(driver: &str) -> bool {
    supported(driver)
        && !matches!(
            driver,
            "amazon-bedrock" | "azure-openai" | "openai-codex" | "xai-grok-build"
        )
}

pub(super) fn routes<'a>(
    driver: &str,
    models: impl IntoIterator<Item = &'a ModelEntry>,
    configured: &BTreeMap<String, NewApiEndpoint>,
    overrides: &BTreeMap<String, NewApiEndpoint>,
) -> Result<HashMap<String, ModelProtocol>, HostError> {
    if driver == "new-api-router" {
        return new_api::routes(models, configured, overrides);
    }
    Ok(models
        .into_iter()
        .filter_map(|model| {
            protocol(driver, &model.model_id).map(|protocol| (model.model_id.clone(), protocol))
        })
        .collect())
}

fn protocol(driver: &str, model_id: &str) -> Option<ModelProtocol> {
    if driver == "aihubmix" && model_id.starts_with("anthropic/") {
        return Some(ModelProtocol::OpenaiResponses);
    }
    if driver == "zenmux" && model_id.starts_with("anthropic/") {
        return Some(ModelProtocol::Anthropic);
    }
    DRIVERS
        .iter()
        .find(|(name, _)| *name == driver)
        .and_then(|(_, protocol)| *protocol)
}

pub(super) fn connection(
    driver: &str,
    protocol: ModelProtocol,
    base_url: &str,
    mut headers: HashMap<String, String>,
) -> HttpModelConnection {
    let base = base_url.trim_end_matches('/');
    let base_url = if driver == "new-api-router" {
        new_api::base_url(base, protocol)
    } else if driver == "azure-openai" {
        format!("{base}/v1")
    } else if driver == "zenmux" && protocol == ModelProtocol::Anthropic {
        let root = base
            .strip_suffix("/anthropic/v1")
            .or_else(|| base.strip_suffix("/v1"))
            .unwrap_or(base);
        format!("{root}/anthropic/v1")
    } else {
        base.to_owned()
    };
    if matches!(driver, "aihubmix" | "aihubmix-responses")
        && !headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("app-code"))
    {
        headers.insert("APP-Code".into(), "WHVL9885".into());
    }
    let preset = if protocol == ModelProtocol::OpenaiCompatible
        && matches!(driver, "deepseek" | "moonshotai" | "aihubmix" | "zenmux")
    {
        Some(driver.to_owned())
    } else {
        None
    };
    HttpModelConnection {
        protocol,
        base_url,
        network: Default::default(),
        headers,
        bearer: None,
        api_key: None,
        preset,
    }
}
