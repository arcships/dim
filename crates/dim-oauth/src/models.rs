//! Projects the relay's `/v1/models?type=dim` list into catalog models. Server
//! metadata wins; the bundled catalog fills what the server omits.
use crate::http::{Body, MAX_SAFE_INTEGER, text};
use dim_agent_capabilities::host::ModelProtocol;
use dim_app_server::{
    managed_provider::{ManagedError, ManagedModel},
    model_catalog::BundledCatalog,
};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{collections::HashSet, sync::LazyLock};

type Result<T> = std::result::Result<T, ManagedError>;
type Object = Map<String, Value>;

const PROVIDER_ID: &str = crate::PROVIDER_ID;
const INVALID: ManagedError = ManagedError::InvalidResponse;
/// Reasoning switches for relay models whose upstream default is on.
const REASONING_OFF: &str = r#"{"deepseek-v4-flash":{"kind":"chat_template_kwargs","keys":["thinking","enable_thinking"]},"deepseek-v4-pro":{"kind":"chat_template_kwargs","keys":["thinking","enable_thinking"]}}"#;

fn object(value: Option<&Value>) -> Object {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}
fn record(value: Option<&Value>) -> Result<&Object> {
    value.and_then(Value::as_object).ok_or(INVALID)
}
fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn label(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !v.is_null())
}
fn safe_positive(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|n| (1..=MAX_SAFE_INTEGER).contains(n))
}
/// The id forms used to match bundled metadata.
fn variants(id: &str) -> Vec<String> {
    static SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[_:\s]+").unwrap());
    let lower = id.trim().to_lowercase();
    let normalized = SEPARATORS
        .replace_all(lower.strip_prefix("models/").unwrap_or(&lower), "-")
        .into_owned();
    let last = normalized.rsplit('/').next().unwrap_or_default().to_owned();
    if last == normalized {
        vec![normalized]
    } else {
        vec![normalized, last]
    }
}

fn reasoning(value: Option<&Value>, options: Option<&Value>) -> Result<Option<Object>> {
    let efforts: Vec<Value> = options
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter(|v| v.get("type") == Some(&json!("effort")))
                .flat_map(|v| strings(v.get("values")))
                .map(Value::String)
                .collect()
        })
        .unwrap_or_default();
    let source = match value {
        Some(Value::Bool(supported)) => {
            let mut result = Object::from_iter([("supported".into(), json!(supported))]);
            if *supported && !efforts.is_empty() {
                result.insert("effortOptions".into(), Value::Array(efforts));
            }
            return Ok(Some(result));
        }
        None if efforts.is_empty() => return Ok(None),
        None => {
            return Ok(Some(Object::from_iter([
                ("supported".into(), json!(true)),
                ("effortOptions".into(), Value::Array(efforts)),
            ])));
        }
        Some(Value::Object(source)) => source,
        Some(_) => return Ok(None),
    };
    for key in [
        "supported",
        "defaultEnabled",
        "default",
        "interleaved",
        "summaries",
    ] {
        if !matches!(present(source.get(key)), None | Some(Value::Bool(_))) {
            return Err(INVALID);
        }
    }
    for key in ["mode", "effort", "level", "verbosity", "visibility"] {
        if !matches!(present(source.get(key)), None | Some(Value::String(_))) {
            return Err(INVALID);
        }
    }
    for key in [
        "effortOptions",
        "effort_options",
        "levelOptions",
        "verbosityOptions",
        "continuation",
        "notes",
    ] {
        if let Some(value) = present(source.get(key))
            && !value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        {
            return Err(INVALID);
        }
    }
    if let Some(budget) = present(source.get("budget")) {
        let budget = record(Some(budget))?;
        for key in ["min", "max", "default"] {
            if !matches!(present(budget.get(key)), None | Some(Value::Number(_))) {
                return Err(INVALID);
            }
        }
        if !matches!(present(budget.get("unit")), None | Some(Value::String(_))) {
            return Err(INVALID);
        }
    }
    if let Some(off) = present(source.get("off")) {
        let off = record(Some(off))?;
        match off.get("kind").and_then(Value::as_str) {
            Some("thinking_type") => {
                text(off.get("value"), INVALID)?;
            }
            Some("thinking_budget") => {
                if !off
                    .get("value")
                    .is_some_and(|v| v.as_u64().is_some_and(|n| n <= MAX_SAFE_INTEGER))
                {
                    return Err(INVALID);
                }
            }
            Some("enable_thinking") => {
                if !off.get("value").is_some_and(Value::is_boolean) {
                    return Err(INVALID);
                }
            }
            Some("chat_template_kwargs") => {
                if !off
                    .get("keys")
                    .and_then(Value::as_array)
                    .is_some_and(|keys| keys.iter().all(Value::is_string))
                {
                    return Err(INVALID);
                }
            }
            _ => return Err(INVALID),
        }
    }
    let mut result = Object::from_iter([(
        "supported".into(),
        present(source.get("supported"))
            .cloned()
            .unwrap_or(json!(true)),
    )]);
    for key in [
        "mode",
        "effort",
        "level",
        "levelOptions",
        "verbosity",
        "verbosityOptions",
        "interleaved",
        "summaries",
        "visibility",
        "continuation",
        "budget",
        "notes",
        "off",
    ] {
        if let Some(value) = present(source.get(key)) {
            result.insert(key.into(), value.clone());
        }
    }
    if let Some(enabled) = present(source.get("defaultEnabled")).or(present(source.get("default")))
    {
        result.insert("defaultEnabled".into(), enabled.clone());
    }
    if let Some(direct) =
        present(source.get("effortOptions")).or(present(source.get("effort_options")))
    {
        result.insert("effortOptions".into(), direct.clone());
    } else if result["supported"] == json!(true) && !efforts.is_empty() {
        result.insert("effortOptions".into(), Value::Array(efforts));
    }
    Ok(Some(result))
}

fn service_tier(value: Option<&Value>, raw: &Object) -> Option<Object> {
    if value == Some(&json!(false)) {
        return Some(Object::from_iter([
            ("supported".into(), json!(false)),
            ("options".into(), json!([])),
        ]));
    }
    let extra = object(raw.get("extra_capabilities"));
    let fast = object(
        object(object(object(raw.get("experimental")).get("modes")).get("fast"))
            .get("provider")
            .and_then(|p| p.get("body")),
    );
    let fast = label(present(fast.get("service_tier")).or(present(fast.get("serviceTier"))));
    let (mut supported, mut preferred) = (None, None);
    let mut options: Vec<String> = vec![];
    let sources = [
        present(raw.get("serviceTier")).or(present(raw.get("service_tier"))),
        present(extra.get("serviceTier")).or(present(extra.get("service_tier"))),
        value,
    ];
    for (index, v) in sources.into_iter().enumerate() {
        let declared = index == 2;
        let object = object(v);
        let support = match v {
            Some(Value::Bool(b)) => Some(*b),
            _ => object.get("supported").and_then(Value::as_bool),
        };
        let pref = match v {
            Some(Value::String(_)) => label(v),
            _ => label(present(object.get("default")).or(present(object.get("default_tier")))),
        };
        if support.is_some() && (supported.is_none() || declared) {
            supported = support;
        }
        if pref.is_some() && (preferred.is_none() || declared) {
            preferred = pref;
        }
        match v {
            Some(Value::String(s)) => options.push(s.clone()),
            _ => options.extend(strings(object.get("options"))),
        }
    }
    if let Some(fast) = fast {
        options.extend(["default".into(), fast]);
    }
    let mut seen = HashSet::new();
    options.retain(|o| seen.insert(o.clone()));
    let preferred = preferred.or_else(|| {
        if options.iter().any(|o| o == "default") {
            Some("default".into())
        } else {
            options.first().cloned()
        }
    });
    if supported.is_none() && preferred.is_none() && options.is_empty() {
        return None;
    }
    let mut result = Object::from_iter([
        ("supported".into(), json!(supported.unwrap_or(true))),
        ("options".into(), json!(options)),
    ]);
    if let Some(preferred) = preferred {
        result.insert("default".into(), json!(preferred));
    }
    Some(result)
}

fn apply_dim_rules(entry: &mut Object, explicit_context: bool) {
    static GPT5: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:gpt|arcship|archship)-5\.[a-z0-9][a-z0-9._-]*$").unwrap()
    });
    static OFF: LazyLock<Object> = LazyLock::new(|| serde_json::from_str(REASONING_OFF).unwrap());
    let ids = variants(entry["modelId"].as_str().unwrap_or_default());
    if !explicit_context && ids.iter().any(|id| GPT5.is_match(id)) {
        entry["capabilities"]["contextWindow"] = json!(258000);
        // The managed catalog declares this value itself, so a later format
        // upgrade keeps it as a declaration.
        if let Some(fields) = entry
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("capabilityOverrideFields"))
            .and_then(Value::as_array_mut)
            && !fields.iter().any(|field| field == "contextWindow")
        {
            fields.push(json!("contextWindow"));
        }
    }
    let Some(reasoning) = entry
        .get_mut("metadata")
        .and_then(|m| m.get_mut("reasoning"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if reasoning.get("supported") == Some(&json!(true))
        && !reasoning.contains_key("off")
        && let Some(off) = ids.iter().find_map(|id| OFF.get(id))
    {
        reasoning.insert("off".into(), off.clone());
    }
}

pub(super) fn project_models(body: &Body, bundled: &BundledCatalog) -> Result<Vec<ManagedModel>> {
    let data = body.get("data").and_then(Value::as_array).ok_or(INVALID)?;
    let mut ids = HashSet::new();
    data.iter()
        .map(|value| {
            let raw = record(Some(value))?;
            let id = text(raw.get("id"), INVALID)?.trim().to_owned();
            if !ids.insert(id.clone()) {
                return Err(INVALID);
            }
            let dim = present(raw.get("dim"))
                .map(|d| record(Some(d)))
                .transpose()?;
            if dim.is_some_and(|d| d.get("id").is_some_and(|v| v != &json!(id))) {
                return Err(INVALID);
            }
            project(&id, raw, dim, bundled)
        })
        .collect()
}

fn project(
    id: &str,
    raw: &Object,
    dim: Option<&Object>,
    bundled: &BundledCatalog,
) -> Result<ManagedModel> {
    let input = dim.unwrap_or(raw);
    let declared = object(input.get("capabilities"));
    let limit = object(input.get("limit"));
    let raw_metadata = object(input.get("raw"));
    let found = bundled
        .fallback(id)
        .map(|entry| serde_json::to_value(entry).expect("model entries serialize"));
    let found_metadata = object(found.as_ref().and_then(|f| f.get("metadata")));
    let mut r = reasoning(
        input.get("reasoning"),
        input
            .get("reasoning_options")
            .or(raw_metadata.get("reasoning_options")),
    )?;
    match &mut r {
        Some(r) if r.get("supported") == Some(&json!(true)) => {
            let mut merged = object(found_metadata.get("reasoning"));
            merged.extend(std::mem::take(r));
            *r = merged;
        }
        None if found_metadata
            .get("reasoning")
            .is_some_and(|v| v.as_bool() != Some(false)) =>
        {
            r = Some(object(found_metadata.get("reasoning")));
        }
        _ => {}
    }
    for value in [
        input.get("contextWindow"),
        limit.get("context"),
        declared.get("contextWindow"),
        input.get("maxOutputTokens"),
        limit.get("output"),
        declared.get("maxOutputTokens"),
    ] {
        if present(value).is_some() && safe_positive(value).is_none() {
            return Err(INVALID);
        }
    }
    let context = safe_positive(input.get("contextWindow"))
        .or(safe_positive(limit.get("context")))
        .or(safe_positive(declared.get("contextWindow")));
    let output = safe_positive(input.get("maxOutputTokens"))
        .or(safe_positive(limit.get("output")))
        .or(safe_positive(declared.get("maxOutputTokens")));
    let found_caps = found.as_ref().map(|f| object(f.get("capabilities")));
    let mut caps = found_caps.clone().unwrap_or_else(|| {
        object(Some(
            &json!({"modalities":["text"],"toolCalling":true,"streaming":true,
            "structuredOutput":true,"vision":false,"audio":false,"reasoning":false}),
        ))
    });
    let found_limit = |key: &str| found_caps.as_ref().and_then(|c| c.get(key)).cloned();
    // A limit the relay neither declares nor the bundled catalog supplies
    // stays absent; the model is then listed as undeclared, never given one.
    for (key, declared) in [("contextWindow", context), ("maxOutputTokens", output)] {
        if let Some(declared) = declared {
            caps.insert(key.into(), Value::from(declared));
        } else if let Some(found) = found_limit(key) {
            caps.insert(key.into(), found);
        } else {
            caps.remove(key);
        }
    }
    for key in [
        "toolCalling",
        "streaming",
        "structuredOutput",
        "vision",
        "audio",
        "reasoning",
    ] {
        if let Some(Value::Bool(value)) = declared.get(key) {
            caps.insert(key.into(), json!(value));
        }
    }
    if declared.contains_key("modalities") {
        caps.insert(
            "modalities".into(),
            json!(strings(declared.get("modalities"))),
        );
    }
    if input.contains_key("modalities") {
        caps.insert("modalities".into(), json!(strings(input.get("modalities"))));
    }
    if let Some(Value::Bool(tool_calling)) = input.get("toolCalling") {
        caps.insert("toolCalling".into(), json!(tool_calling));
    }
    if let Some(Value::Bool(vision)) = input.get("vision") {
        caps.insert("vision".into(), json!(vision));
        if *vision && !input.contains_key("modalities") && !declared.contains_key("modalities") {
            caps.insert("modalities".into(), json!(["text", "image"]));
        }
    }
    if let Some(r) = &r {
        caps.insert(
            "reasoning".into(),
            json!(r.get("supported") == Some(&json!(true))),
        );
    }
    if let Some(state) = declared.get("providerState") {
        caps.insert(
            "providerState".into(),
            Value::Object(record(Some(state))?.clone()),
        );
    }
    let endpoint = match present(input.get("endpoint_type")) {
        None => "openai".to_owned(),
        Some(Value::String(_)) => {
            label(input.get("endpoint_type")).unwrap_or_else(|| "openai".into())
        }
        Some(_) => String::new(),
    };
    let route = match endpoint.as_str() {
        "openai" => Some(ModelProtocol::OpenaiCompatible),
        "anthropic" => Some(ModelProtocol::Anthropic),
        _ => None,
    };
    let found_raw = object(found_metadata.get("raw"));
    let kind = label(input.get("type"))
        .or(label(raw_metadata.get("type")))
        .or(label(found_raw.get("type")));
    let status = input
        .get("status")
        .and_then(Value::as_str)
        .filter(|s| matches!(*s, "active" | "beta" | "deprecated" | "unavailable"))
        .unwrap_or("active");
    // Publish only known non-secret metadata, never the unfiltered relay response.
    let mut model_raw = found_raw;
    model_raw.extend([
        ("id".into(), json!(id)),
        ("providerId".into(), json!(PROVIDER_ID)),
        ("endpointType".into(), json!(endpoint)),
    ]);
    for key in ["imageGeneration", "modalityCapabilities"] {
        if let Some(value) = input.get(key) {
            model_raw.insert(key.into(), value.clone());
        }
    }
    for key in ["type", "imageGeneration", "interleaved", "compat"] {
        if let Some(value) = raw_metadata.get(key) {
            model_raw.insert(key.into(), value.clone());
        }
    }
    for key in [
        "typeHints",
        "type_hints",
        "modalityCapabilities",
        "modality_capabilities",
    ] {
        if raw_metadata.get(key).is_some_and(Value::is_array) {
            model_raw.insert(key.into(), json!(strings(raw_metadata.get(key))));
        }
    }
    if raw_metadata.contains_key("modalities") {
        let io = object(raw_metadata.get("modalities"));
        model_raw.insert(
            "modalities".into(),
            json!({"input": strings(io.get("input")), "output": strings(io.get("output"))}),
        );
    }
    if let Some(kind) = &kind {
        model_raw.insert("type".into(), json!(kind));
    }
    if let Some(Value::Array(actions)) = input.get("modalityCapabilities") {
        model_raw.insert(
            "imageGeneration".into(),
            json!(actions.contains(&json!("image.generate"))),
        );
    }
    if input.get("imageGeneration") == Some(&json!(true))
        && !input.contains_key("modalityCapabilities")
    {
        model_raw.insert("type".into(), json!("image-generation"));
    }
    // Missing or empty output metadata is unspecified; text may coexist with other outputs.
    let output_modalities: Vec<String> = strings(object(model_raw.get("modalities")).get("output"))
        .into_iter()
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .collect();
    let actions = present(model_raw.get("modalityCapabilities"))
        .or(present(model_raw.get("modality_capabilities")))
        .cloned();
    // Explicit image policy wins; otherwise image output identifies an image-generation resource.
    let image_generation = match model_raw.get("imageGeneration") {
        Some(Value::Bool(b)) => *b,
        _ => match &actions {
            Some(Value::Array(_)) => strings(actions.as_ref())
                .iter()
                .any(|a| a == "image.generate"),
            _ => output_modalities.iter().any(|m| m == "image"),
        },
    };
    let non_chat = image_generation
        || model_raw
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "chat")
        || strings(present(model_raw.get("typeHints")).or(present(model_raw.get("type_hints"))))
            .iter()
            .any(|v| v != "chat")
        || strings(actions.as_ref()).iter().any(|v| v != "chat")
        || (!output_modalities.is_empty() && !output_modalities.iter().any(|m| m == "text"));
    // The server's status wins over the kind: a media model the relay marks
    // unavailable serves no use at all, while the chat-kind reason leaves a
    // non-conversational use open.
    let mut unavailable_reason = if route.is_none() {
        Some("unsupported_endpoint")
    } else if status == "unavailable" {
        Some("server_unavailable")
    } else if non_chat {
        Some("unsupported_model_type")
    } else {
        None
    };
    if let Some(field) = input
        .get("reasoning")
        .and_then(|r| r.get("interleavedField"))
    {
        model_raw.insert("interleaved".into(), json!({"field": field}));
    }
    let tier = service_tier(input.get("serviceTier"), &raw_metadata)
        .map(Value::Object)
        .or_else(|| found_metadata.get("serviceTier").cloned());
    let mut pricing = object(found_metadata.get("pricing"));
    let server_pricing = object(raw_metadata.get("pricing"));
    for (source, target) in [("input", "inputTokenPrice"), ("output", "outputTokenPrice")] {
        if let Some(price) = server_pricing
            .get(source)
            .and_then(Value::as_f64)
            .filter(|p| p.is_finite() && *p >= 0.)
        {
            pricing.insert(target.into(), json!(price));
        }
    }
    let found_entry = found.as_ref();
    let display_name = label(input.get("displayName"))
        .or(label(input.get("display_name")))
        .or(label(input.get("name")))
        .or(label(raw.get("displayName")))
        .or(label(raw.get("display_name")))
        .or(label(raw.get("name")))
        .or(found_entry.and_then(|f| label(f.get("displayName"))))
        .unwrap_or_else(|| id.to_owned());
    let guide = label(input.get("modelGuidePrompt"))
        .or(found_entry.and_then(|f| label(f.get("modelGuidePrompt"))));
    let mut metadata = Object::new();
    if let Some(r) = r {
        metadata.insert("reasoning".into(), Value::Object(r));
    }
    if let Some(tier) = tier {
        metadata.insert("serviceTier".into(), tier);
    }
    metadata.insert(
        "metadataSource".into(),
        json!(if dim.is_some() {
            "dim-oauth-model-config"
        } else {
            "provider-default"
        }),
    );
    for key in ["description", "releaseDate", "rateLimit"] {
        if let Some(value) = found_metadata.get(key) {
            metadata.insert(key.into(), value.clone());
        }
    }
    if !pricing.is_empty() {
        metadata.insert("pricing".into(), Value::Object(pricing));
    }
    metadata.extend([
        ("matchedProviderId".into(), json!(PROVIDER_ID)),
        ("matchedModelId".into(), json!(id)),
        (
            "matchStrategy".into(),
            json!(if dim.is_some() {
                "dim-oauth-exact"
            } else {
                "provider-default"
            }),
        ),
        (
            "capabilityOverrideFields".into(),
            json!(
                [
                    context.map(|_| "contextWindow"),
                    output.map(|_| "maxOutputTokens")
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
            ),
        ),
        ("raw".into(), Value::Object(model_raw)),
    ]);
    let mut entry = Object::from_iter([
        ("modelId".into(), json!(id)),
        ("displayName".into(), json!(display_name)),
        ("status".into(), json!(status)),
        ("capabilities".into(), Value::Object(caps)),
        ("metadata".into(), Value::Object(metadata)),
    ]);
    if let Some(guide) = guide {
        entry.insert("modelGuidePrompt".into(), json!(guide));
    }
    apply_dim_rules(&mut entry, dim.is_some() && context.is_some());
    if unavailable_reason.is_none()
        && ["contextWindow", "maxOutputTokens"].iter().any(|key| {
            entry
                .get("capabilities")
                .and_then(|capabilities| capabilities.get(*key))
                .and_then(Value::as_u64)
                .is_none()
        })
    {
        unavailable_reason = Some("limits_undeclared");
    }
    Ok(ManagedModel {
        entry: serde_json::from_value(Value::Object(entry)).map_err(|_| INVALID)?,
        protocol: route.unwrap_or(ModelProtocol::OpenaiCompatible),
        unavailable_reason: unavailable_reason.map(str::to_owned),
        zdr: raw.get("zdr") == Some(&json!(true)),
    })
}
