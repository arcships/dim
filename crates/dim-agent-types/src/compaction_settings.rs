//! The `compaction` settings document and the window shaping it drives
//! (#2235 PR 1b): one persisted value shape — `auto`, a token count or a
//! percentage — with per-model overrides, a soft cap for token values wider
//! than the model's window, and the shaping floor every explicit value
//! clamps to. The server resolves this document on every preparation; the
//! client computes the same effective window when it writes a value, so the
//! pure geometry lives here, beside the types both sides share.

use crate::JS_MAX_SAFE_INTEGER;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

/// One compaction window as the `compaction` source carries it: `"auto"`, a
/// token count (integer, optional `k`/`m` suffix as a string) or a
/// percentage string of the model's context window (#2235 §3.2). Token
/// counts of zero and malformed strings are invalid; percentages must be
/// positive and at most 100.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CompactionWindowSpec {
    /// The model-derived automatic threshold, the same as an absent entry.
    #[default]
    Auto,
    Tokens(u64),
    Percent(f64),
}

impl CompactionWindowSpec {
    /// Parses the value a user writes: `auto`, a percentage or a token
    /// count with the optional `k`/`m` suffixes.
    pub fn parse(text: &str) -> Result<CompactionWindowSpec, CompactionWindowSpecError> {
        parse_compaction_window(text)
    }

    /// The value as users write it in the document: `auto`, a percentage
    /// such as `40%`, or the bare token count.
    pub fn display(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Tokens(tokens) => tokens.to_string(),
            Self::Percent(percent) => format_percent(*percent),
        }
    }
}

fn format_percent(percent: f64) -> String {
    if percent == percent.trunc() {
        format!("{}%", percent as u64)
    } else {
        format!("{percent}%")
    }
}

/// A malformed compaction window value: neither `auto`, a percentage nor a
/// token count within the safe-integer range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionWindowSpecError;

impl fmt::Display for CompactionWindowSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "compaction window must be \"auto\", a positive percentage of at most 100% or a positive token count",
        )
    }
}

impl std::error::Error for CompactionWindowSpecError {}

// The value grammar is the one the write boundary's schema declares
// (#2235 PR 1b): a percentage is `100` with an all-zero fraction, one or
// two digits without a leading zero, or `0.` followed by a nonzero
// fraction; a token count is digits without a leading zero, at most 15
// before an optional suffix, 12 before `k` and 9 before `m`.
fn parse_percent_digits(digits: &str) -> Option<f64> {
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if !fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let shaped = match whole.len() {
        1 => true,
        2 => !whole.starts_with('0'),
        _ => false,
    } || whole == "100" && fraction.bytes().all(|byte| byte == b'0')
        || whole == "0" && !fraction.is_empty() && fraction.bytes().any(|byte| byte != b'0');
    if !shaped || whole.bytes().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<f64>()
        .ok()
        .filter(|percent| *percent > 0.0 && *percent <= 100.0)
}

fn parse_compaction_window(text: &str) -> Result<CompactionWindowSpec, CompactionWindowSpecError> {
    if text == "auto" {
        return Ok(CompactionWindowSpec::Auto);
    }
    if let Some(digits) = text.strip_suffix('%') {
        return parse_percent_digits(digits)
            .map(CompactionWindowSpec::Percent)
            .ok_or(CompactionWindowSpecError);
    }
    let (digits, multiplier, limit) = match text.as_bytes().last() {
        Some(b'k' | b'K') => (&text[..text.len() - 1], 1_000, 12),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1_000_000, 9),
        _ => (text, 1, 15),
    };
    let shaped = !digits.is_empty()
        && digits.len() <= limit
        && !digits.starts_with('0')
        && digits.bytes().all(|byte| byte.is_ascii_digit());
    if !shaped {
        return Err(CompactionWindowSpecError);
    }
    let base = digits.parse::<u64>().ok().filter(|value| *value > 0);
    base.and_then(|value| value.checked_mul(multiplier))
        .filter(|tokens| *tokens <= JS_MAX_SAFE_INTEGER)
        .map(CompactionWindowSpec::Tokens)
        .ok_or(CompactionWindowSpecError)
}

impl Serialize for CompactionWindowSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Percent(percent) => serializer.serialize_str(&format_percent(*percent)),
            Self::Tokens(tokens) => serializer.serialize_u64(*tokens),
        }
    }
}

impl<'de> Deserialize<'de> for CompactionWindowSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Tokens(u64),
            Text(String),
        }
        match Value::deserialize(deserializer).map_err(serde::de::Error::custom)? {
            Value::Text(text) => parse_compaction_window(&text).map_err(serde::de::Error::custom),
            Value::Tokens(tokens) if tokens > 0 && tokens <= JS_MAX_SAFE_INTEGER => {
                Ok(CompactionWindowSpec::Tokens(tokens))
            }
            Value::Tokens(_) => Err(serde::de::Error::custom(CompactionWindowSpecError)),
        }
    }
}

/// The `compaction` settings document (#2235 §3.2): the global `default`
/// window and per-model overrides keyed by exact `<providerId>/<modelId>` or
/// bare `<modelId>`. Every member absent from the stored document is the
/// schema's default, which the source's declaration fills before any reader
/// sees it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionSettings {
    #[serde(default)]
    pub default: CompactionWindowSpec,
    #[serde(default)]
    pub models: BTreeMap<String, CompactionWindowSpec>,
}

impl CompactionSettings {
    /// The entry that governs `provider_id`/`model_id` (#2235 §3.2): the
    /// exact `<provider>/<model>` key first, then the bare model key, then
    /// the global default. An explicit `"auto"` entry is a hit like any
    /// other — it overrides a non-auto outer layer.
    pub fn spec_for(&self, provider_id: &str, model_id: &str) -> CompactionWindowSpec {
        self.models
            .get(&format!("{provider_id}/{model_id}"))
            .or_else(|| self.models.get(model_id))
            .copied()
            .unwrap_or(self.default)
    }
}

/// The least compaction window a configuration may shape with (#2235 §3.5):
/// 32k tokens, capped at a tenth of the model's capacity.
pub fn compact_window_floor(context_window: u64) -> u64 {
    32_000.min(context_window / 10)
}

/// The tokens a compaction window of `percent` of the capacity is.
pub fn compact_window_percent_tokens(percent: f64, context_window: u64) -> u64 {
    (percent * context_window as f64 / 100.0).floor() as u64
}

/// What a compaction window entry becomes on a model whose context window is
/// `context_window` (#2235 §3.2, §3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapedWindow {
    /// No explicit window: the model-derived automatic threshold. `auto` is
    /// itself never raised; a token entry wider than the capacity is a soft
    /// cap and falls back here instead of failing.
    ModelDerived,
    /// The window in force. `raised_from` is the configured tokens when the
    /// shaping floor lifted them, so a client can report the raise once.
    Shaped {
        tokens: u64,
        raised_from: Option<u64>,
    },
}

/// Resolves `spec` against the model's context window: percentages convert
/// to tokens, a token value wider than the window is a soft cap that returns
/// the model-derived default, and every explicit window at or inside the
/// window clamps up to [`compact_window_floor`].
pub fn shape_compaction_window(spec: CompactionWindowSpec, context_window: u64) -> ShapedWindow {
    let configured = match spec {
        CompactionWindowSpec::Auto => return ShapedWindow::ModelDerived,
        CompactionWindowSpec::Tokens(tokens) => {
            if tokens > context_window {
                return ShapedWindow::ModelDerived;
            }
            tokens
        }
        CompactionWindowSpec::Percent(percent) => {
            compact_window_percent_tokens(percent, context_window)
        }
    };
    let floor = compact_window_floor(context_window);
    if configured < floor {
        ShapedWindow::Shaped {
            tokens: floor,
            raised_from: Some(configured),
        }
    } else {
        ShapedWindow::Shaped {
            tokens: configured,
            raised_from: None,
        }
    }
}
