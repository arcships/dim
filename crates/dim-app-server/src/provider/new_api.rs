//! Decode external New API route metadata once, when a provider record is admitted.
use super::failure;
use crate::model_catalog::{ModelEntry, NewApiEndpoint as Endpoint};
use dim_agent_capabilities::host::{HostError, ModelProtocol};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

impl Endpoint {
    pub(super) fn protocol(self) -> Option<ModelProtocol> {
        match self {
            Self::Openai => Some(ModelProtocol::OpenaiCompatible),
            Self::OpenaiResponse => Some(ModelProtocol::OpenaiResponses),
            Self::Anthropic => Some(ModelProtocol::Anthropic),
            Self::Gemini => Some(ModelProtocol::Gemini),
            _ => None,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    #[serde(alias = "endpoint_type")]
    endpoint_type: Option<Endpoint>,
    #[serde(alias = "supported_endpoint_types")]
    supported_endpoint_types: Option<Vec<String>>,
    #[serde(alias = "selectable_endpoint_types")]
    selectable_endpoint_types: Option<Vec<String>>,
    #[serde(alias = "owned_by")]
    owned_by: Option<Value>,
    #[serde(alias = "capability_provider_id")]
    capability_provider_id: Option<Value>,
    #[serde(rename = "type")]
    kind: Option<Value>,
}

pub(super) fn routes<'a>(
    models: impl IntoIterator<Item = &'a ModelEntry>,
    configured: &BTreeMap<String, Endpoint>,
    overrides: &BTreeMap<String, Endpoint>,
) -> Result<HashMap<String, ModelProtocol>, HostError> {
    let mut routes = HashMap::new();
    for model in models {
        let raw: Metadata = model
            .metadata
            .as_ref()
            .map(|m| {
                serde_json::from_value(Value::Object(m.raw.clone()))
                    .map_err(|_| failure("Invalid New API model route metadata"))
            })
            .transpose()?
            .unwrap_or_default();
        let discovered = endpoint(
            &model.model_id,
            raw,
            configured.get(&model.model_id).copied(),
        )?;
        if let Some(protocol) = overrides
            .get(&model.model_id)
            .copied()
            .or(discovered)
            .and_then(Endpoint::protocol)
        {
            routes.insert(model.model_id.clone(), protocol);
        }
    }
    Ok(routes)
}

fn endpoint(
    model_id: &str,
    raw: Metadata,
    configured: Option<Endpoint>,
) -> Result<Option<Endpoint>, HostError> {
    if let Some(explicit) = raw.endpoint_type.or(configured) {
        return Ok(Some(explicit));
    }
    let supported = candidates(raw.supported_endpoint_types)?;
    let selectable = candidates(raw.selectable_endpoint_types)?;
    let candidates = selectable.as_ref().or(supported.as_ref());
    let haystack = format!(
        "{} {} {}",
        model_id,
        raw.owned_by
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default(),
        raw.capability_provider_id
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default()
    )
    .to_ascii_lowercase();
    for (candidate, words) in [
        (Endpoint::Anthropic, &["anthropic", "claude"][..]),
        (Endpoint::Gemini, &["google", "gemini"][..]),
        (Endpoint::OpenaiResponse, &["response", "responses"][..]),
    ] {
        if candidates.is_none_or(|values| values.contains(&candidate))
            && haystack
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| words.contains(&word))
        {
            return Ok(Some(candidate));
        }
    }
    if let Some(hint) = raw.kind.and_then(|kind| serde_json::from_value(kind).ok()) {
        return Ok(Some(hint));
    }
    if let Some(candidates) = candidates {
        let mut chat = candidates
            .iter()
            .copied()
            .filter(|endpoint| endpoint.protocol().is_some());
        let first = chat.next();
        if chat.next().is_none() {
            return Ok(first);
        }
        return Ok(candidates
            .contains(&Endpoint::Openai)
            .then_some(Endpoint::Openai));
    }
    Ok(Some(Endpoint::Openai))
}

fn candidates(values: Option<Vec<String>>) -> Result<Option<Vec<Endpoint>>, HostError> {
    let Some(values) = values.filter(|values| !values.is_empty()) else {
        return Ok(None);
    };
    let mut result = Vec::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(failure("New API endpoint list contains an empty string"));
        }
        if let Ok(endpoint) = serde_json::from_value::<Endpoint>(Value::String(value))
            && !result.contains(&endpoint)
        {
            result.push(endpoint);
        }
    }
    Ok(Some(result))
}

pub(super) fn base_url(base: &str, protocol: ModelProtocol) -> String {
    let (root, tail) = base.rsplit_once('/').unwrap_or((base, ""));
    let tail = tail.to_ascii_lowercase();
    let root = if root.contains("://") && (tail == "v1" || tail.starts_with("v1beta")) {
        root
    } else {
        base
    };
    match protocol {
        ModelProtocol::Gemini => format!("{root}/v1beta"),
        ModelProtocol::Anthropic => root.to_owned(),
        _ => format!("{root}/v1"),
    }
}
