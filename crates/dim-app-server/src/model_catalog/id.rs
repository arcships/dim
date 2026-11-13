//! How a model id names a bundled model, and what numbers it takes from it.

use super::*;

impl BundledCatalog {
    /// The bundled row an id names: the projection's own list by exact id,
    /// then a fixed list of family and aggregator sources, then any provider
    /// by exact id and by a normalized id.
    pub(super) fn lookup(
        &self,
        provider: &ProviderProjection,
        id: &str,
    ) -> Option<(String, ModelEntry, String, f64)> {
        if provider.source != Some(ModelSource::Custom)
            && let Some(m) = provider.models.iter().find(|m| m.model_id == id)
        {
            // The bundled entry is the declaration for a builtin provider's
            // model, so a stored copy that lacks a limit is supplied from it.
            let found = self
                .bundled_models(&provider.provider_id)
                .and_then(|models| models.iter().find(|entry| entry.model_id == id))
                .cloned()
                .unwrap_or_else(|| m.clone());
            return Some((
                provider.provider_id.clone(),
                found,
                "provider-exact".into(),
                1.,
            ));
        }
        let mut ids = vec!["custom-provider"];
        let normalized = variants(id);
        for (prefix, sources) in [
            ("deepseek-", &["deepseek"][..]),
            ("glm-", &["zai", "zhipuai"]),
            ("qwen", &["bailian-coding-plan", "bailian"]),
            ("kimi-", &["moonshot", "moonshot-ai"]),
            ("claude-", &["anthropic"]),
            ("gemini-", &["google"]),
            ("gpt-", &["openai"]),
            ("minimax-", &["minimax", "minimax-cn"]),
            ("seed-", &["doubao-seed"]),
        ] {
            if normalized.iter().any(|variant| variant.starts_with(prefix)) {
                ids.extend_from_slice(sources);
            }
        }
        ids.extend([
            "openrouter",
            "zenmux",
            "vercel-ai-gateway",
            "jiekou",
            "ppinfra",
            "aihubmix",
        ]);
        for pid in ids {
            if let Some(models) = self.bundled_models(pid)
                && let Some((m, kind, score)) = match_models(models, id)
            {
                return Some((pid.into(), m.clone(), format!("{pid}-{kind}"), score));
            }
        }
        for exact in [true, false] {
            let candidates: Vec<_> = self
                .providers
                .iter()
                .filter(|p| !matches!(p.provider_id.as_str(), "custom-provider" | "aihubmix"))
                .flat_map(|p| p.models.iter().map(move |m| (p, m)))
                .filter(|(_, m)| {
                    if exact {
                        m.model_id == id
                    } else {
                        !variants(&m.model_id).is_disjoint(&normalized)
                    }
                })
                .filter_map(|(p, m)| media_signature(m).map(|signature| (p, m, signature)))
                .collect();
            if let Some((p, m, signature)) = candidates.first()
                && candidates.iter().all(|(_, _, other)| other == signature)
            {
                let kind = if exact { "exact" } else { "normalized" };
                return Some((
                    p.provider_id.clone(),
                    (*m).clone(),
                    format!("{}-{kind}", p.provider_id),
                    if exact { 1. } else { 0.98 },
                ));
            }
        }
        if provider.source == Some(ModelSource::Custom) {
            provider.models.iter().find(|m| m.model_id == id).map(|m| {
                (
                    provider.provider_id.clone(),
                    m.clone(),
                    "custom-config-exact".into(),
                    1.,
                )
            })
        } else {
            None
        }
    }
}

fn media_signature(model: &ModelEntry) -> Option<Value> {
    let raw = model.metadata.as_ref().map(|m| &m.raw);
    let get = |name: &str| raw.and_then(|r| r.get(name));
    let sorted = |values: Vec<String>| {
        let mut values: Vec<_> = values
            .into_iter()
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .collect();
        values.sort();
        values.dedup();
        values
    };
    let list = |value: Option<&Value>| {
        value
            .and_then(Value::as_array)
            .map(|v| strings(v))
            .unwrap_or_default()
    };
    let kind = model.metadata.as_ref().and_then(model_kind);
    let hints = sorted(list(get("typeHints").or_else(|| get("type_hints"))));
    let actions = sorted(list(
        get("modalityCapabilities").or_else(|| get("modality_capabilities")),
    ));
    let input = sorted(list(get("modalities").and_then(|v| v.get("input"))));
    let output = sorted(list(get("modalities").and_then(|v| v.get("output"))));
    let modalities = sorted(model.capabilities.modalities.clone());
    let is_media = model.capabilities.audio
        || model.capabilities.vision
        || kind.as_ref().is_some_and(|kind| kind != "chat")
        || !actions.is_empty()
        || hints.iter().any(|kind| kind != "chat")
        || input
            .iter()
            .chain(&output)
            .chain(&modalities)
            .any(|v| v != "text");
    is_media.then(|| {
        serde_json::json!({
            "type": kind, "typeHints": hints, "modalityCapabilities": actions,
            "input": input, "output": output, "modalities": modalities,
            "audio": model.capabilities.audio, "vision": model.capabilities.vision,
        })
    })
}

fn match_models<'a>(
    models: &'a [ModelEntry],
    id: &str,
) -> Option<(&'a ModelEntry, &'static str, f64)> {
    if let Some(m) = models.iter().find(|m| m.model_id == id) {
        return Some((m, "exact", 1.));
    }
    let target = variants(id);
    let normalized: Vec<_> = models
        .iter()
        .filter(|m| variants(&m.model_id).iter().any(|v| target.contains(v)))
        .collect();
    if normalized.len() == 1 {
        return Some((normalized[0], "normalized", 0.98));
    }
    let nums = numbers(id);
    let mut scored: Vec<_> = models
        .iter()
        .filter(|m| numbers(&m.model_id) == nums)
        .map(|m| (m, similarity(id, &m.model_id)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let best = scored.first()?;
    if best.1 >= 0.82 && best.1 - scored.get(1).map(|x| x.1).unwrap_or(0.) >= 0.08 {
        Some((best.0, "fuzzy", best.1))
    } else {
        None
    }
}

pub(super) fn variants(s: &str) -> HashSet<String> {
    let c = canonical(s);
    let mut r = HashSet::from([c.clone()]);
    if let Some((_, tail)) = c.rsplit_once('/') {
        r.insert(tail.into());
    }
    r
}

pub(super) fn canonical(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    let mut result = String::new();
    let mut separator = false;
    for ch in lower.strip_prefix("models/").unwrap_or(&lower).chars() {
        if ch == '_' || ch == ':' || ch.is_whitespace() {
            if !separator {
                result.push('-');
            }
            separator = true;
        } else {
            result.push(ch);
            separator = false;
        }
    }
    result
}

fn numbers(s: &str) -> Vec<String> {
    s.split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (canonical(a), canonical(b));
    let max = a.len().max(b.len());
    if max == 0 {
        return 1.;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.bytes().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.bytes().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(prev + usize::from(ca != cb));
            prev = old;
        }
    }
    1. - row[b.len()] as f64 / max as f64
}
