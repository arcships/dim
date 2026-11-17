//! The wire shapes of the `eventHub` service and the `eventhub` namespace,
//! and the field bounds Appendix A.1–A.3 states.
use crate::{error::EventHubError, text::one_line};
use dim_agent_types::InputMode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The longest body a marker carries; a longer one becomes a Session file
/// (design §3.2).
pub const BODY_LIMIT: usize = 5120;
/// How much of an overflowed body the marker keeps (design §3.2).
pub const PREVIEW_BYTES: usize = 1024;
/// The largest event body (design A.2).
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// The largest `guide` a declaration carries (design §2.1).
pub const MAX_GUIDE_BYTES: usize = 4096;
/// The attribute names the runtime writes itself; a producer may not use them
/// (design §7).
pub const RESERVED_ATTRIBUTES: [&str; 6] = ["source", "type", "id", "file", "bytes", "truncated"];

/// `eventHub.declare` (design A.1).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclareRequest {
    pub source: String,
    pub guide: String,
    pub types: Vec<TypeDeclaration>,
}

/// One type a source declares (design §2.1).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeDeclaration {
    pub r#type: String,
    pub description: String,
    pub mode: InputMode,
}

/// `eventHub.publish` (design A.2).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishRequest {
    pub source: String,
    pub r#type: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub body: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub to: Vec<ToTarget>,
}

/// One explicit target of a publish (design A.2).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToTarget {
    pub session_id: String,
    #[serde(default)]
    pub branch_id: Option<String>,
    #[serde(default)]
    pub mode: Option<InputMode>,
}

/// `eventhub.subscribe` (design A.3).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeRequest {
    pub types: Vec<String>,
    #[serde(default, rename = "match")]
    pub matches: Option<BTreeMap<String, String>>,
    #[serde(default, rename = "target")]
    pub target: SubscribeTarget,
    #[serde(default)]
    pub mode: Option<InputMode>,
}

/// The target of a subscription request; `branchId` defaults to `main`
/// (design A.3).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscribeTarget {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub branch_id: Option<String>,
}

/// `eventhub.subscriptions` (design A.3).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRequest {
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `eventhub.unsubscribe` (design A.3).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnsubscribeRequest {
    pub subscription_id: String,
}

/// Where a subscription was created (design §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Tool,
    Client,
}

/// The branch a subscription or delivery names (design §2.2, §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub session_id: String,
    pub branch_id: String,
}

impl Target {
    pub fn new(session_id: impl Into<String>, branch_id: Option<&str>) -> Self {
        Self {
            session_id: session_id.into(),
            branch_id: branch_id.map_or_else(|| "main".into(), str::to_owned),
        }
    }
}

/// A source name: `[a-z][a-z0-9-]{0,31}` (design §2.1).
pub fn valid_source(source: &str) -> bool {
    let mut bytes = source.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_lowercase())
        && source.len() <= 32
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A type's name part: `[a-z][a-z0-9_-]{0,31}` (design §2.1).
pub fn valid_type_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_lowercase())
        && name.len() <= 32
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// An attribute name: `[a-z][a-z0-9_-]{0,31}` (design A.2).
pub fn valid_attribute(name: &str) -> bool {
    valid_type_name(name)
}

/// `<source>.<name>` (design §2.1).
pub fn valid_type(declared: &str) -> bool {
    declared.split_once('.').is_some_and(|(source, name)| {
        !name.contains('.') && valid_source(source) && valid_type_name(name)
    })
}

/// The source a type or a `<source>.*` pattern names (design A.3).
pub fn source_of(pattern: &str) -> Option<&str> {
    if let Some(source) = pattern.strip_suffix(".*") {
        return valid_source(source).then_some(source);
    }
    valid_type(pattern)
        .then(|| pattern.split_once('.').map(|(source, _)| source))
        .flatten()
}

/// Whether the subscription pattern selects `event_type` of `source`
/// (design §2.2: an exact type or `<source>.*`).
pub fn pattern_matches(pattern: &str, source: &str, event_type: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(pattern_source) => pattern_source == source,
        None => pattern == event_type,
    }
}

/// Checks a declaration's bounds; the source is checked by the caller, since
/// `duplicate_source` comes before it.
pub fn validate_declaration(request: &DeclareRequest) -> Result<(), EventHubError> {
    if !valid_source(&request.source) {
        return Err(EventHubError::new(
            "invalid_declaration",
            format!("source {} is not a name", request.source),
        ));
    }
    if request.guide.len() > MAX_GUIDE_BYTES {
        return Err(EventHubError::new(
            "invalid_declaration",
            format!("guide is longer than {MAX_GUIDE_BYTES} bytes"),
        ));
    }
    if request.types.is_empty() || request.types.len() > 64 {
        return Err(EventHubError::new(
            "invalid_declaration",
            "a declaration has 1 to 64 types",
        ));
    }
    let mut named = BTreeSet::new();
    for declared in &request.types {
        if !valid_type(&declared.r#type) {
            return Err(EventHubError::new(
                "invalid_declaration",
                format!("type {} is not a name", declared.r#type),
            ));
        }
        let prefix = declared.r#type.split_once('.').map(|(source, _)| source);
        if prefix != Some(request.source.as_str()) {
            return Err(EventHubError::new(
                "invalid_declaration",
                format!(
                    "type {} is not of source {}",
                    declared.r#type, request.source
                ),
            ));
        }
        if declared.description.len() > 512 {
            return Err(EventHubError::new(
                "invalid_declaration",
                format!(
                    "the description of {} is longer than 512 bytes",
                    declared.r#type
                ),
            ));
        }
        // The catalog keeps one entry per type, so a name twice would silently
        // keep the last (design A.1).
        if !named.insert(declared.r#type.as_str()) {
            return Err(EventHubError::new(
                "invalid_declaration",
                format!("type {} is declared twice", declared.r#type),
            ));
        }
    }
    Ok(())
}

/// Checks a publish's fields other than the body's size, which the caller
/// answers `too_large` (design A.2).
pub fn validate_publish(request: &PublishRequest) -> Result<(), EventHubError> {
    if request.body.len() > MAX_BODY_BYTES {
        return Err(EventHubError::new(
            "too_large",
            format!("body is longer than {MAX_BODY_BYTES} bytes"),
        ));
    }
    if !valid_type(&request.r#type) {
        return Err(EventHubError::invalid_event(format!(
            "type {} is not a name",
            request.r#type
        )));
    }
    if request.r#type.split_once('.').map(|(source, _)| source) != Some(request.source.as_str()) {
        return Err(EventHubError::invalid_event(format!(
            "type {} is not of source {}",
            request.r#type, request.source
        )));
    }
    if let Some(id) = &request.id
        && id.len() > 128
    {
        return Err(EventHubError::invalid_event("id is longer than 128 bytes"));
    }
    if let Some(title) = &request.title
        && (title.len() > 256 || !one_line(title))
    {
        return Err(EventHubError::invalid_event(
            "title must be one line of at most 256 bytes",
        ));
    }
    if request.attributes.len() > 16 {
        return Err(EventHubError::invalid_event(
            "attributes holds at most 16 entries",
        ));
    }
    for (name, value) in &request.attributes {
        if !valid_attribute(name) {
            return Err(EventHubError::invalid_event(format!(
                "attribute name {name} is not a name"
            )));
        }
        if RESERVED_ATTRIBUTES.contains(&name.as_str()) {
            return Err(EventHubError::invalid_event(format!(
                "attribute name {name} is reserved"
            )));
        }
        if value.len() > 256 || !one_line(value) {
            return Err(EventHubError::invalid_event(format!(
                "attribute {name} must be one line of at most 256 bytes"
            )));
        }
    }
    if request.to.len() > 16 {
        return Err(EventHubError::invalid_event("to holds at most 16 targets"));
    }
    for target in &request.to {
        if target.session_id.is_empty() {
            return Err(EventHubError::invalid_event("a target needs a sessionId"));
        }
        if target.branch_id.as_deref() == Some("") {
            return Err(EventHubError::invalid_event("a target branchId is empty"));
        }
    }
    Ok(())
}

/// A checked `subscribe` request's record fields (design A.3).
pub struct CheckedSubscribe {
    pub types: Vec<String>,
    pub matches: Option<BTreeMap<String, String>>,
    pub target: Target,
}

/// Checks a subscribe request and returns the record fields it names
/// (design A.3). Types are checked against the catalog by the caller
/// (`unknown_type`).
pub fn validate_subscribe(request: &SubscribeRequest) -> Result<CheckedSubscribe, EventHubError> {
    if request.types.is_empty() || request.types.len() > 16 {
        return Err(EventHubError::invalid_params("types holds 1 to 16 entries"));
    }
    for pattern in &request.types {
        if source_of(pattern).is_none() {
            return Err(EventHubError::invalid_params(format!(
                "type {pattern} is not a type or <source>.*"
            )));
        }
    }
    let matches = match &request.matches {
        Some(matches) => {
            if matches.len() > 16 {
                return Err(EventHubError::invalid_params(
                    "match holds at most 16 entries",
                ));
            }
            for name in matches.keys() {
                // The value may be empty: an attribute may be, and a match
                // on it selects events whose value is empty (design A.2).
                if !valid_attribute(name) {
                    return Err(EventHubError::invalid_params(format!(
                        "match entry {name} is not a name"
                    )));
                }
            }
            Some(matches.clone())
        }
        None => None,
    };
    if request.target.session_id.is_empty() {
        return Err(EventHubError::invalid_params("target needs a sessionId"));
    }
    if request.target.branch_id.as_deref() == Some("") {
        return Err(EventHubError::invalid_params("target branchId is empty"));
    }
    let target = Target::new(
        request.target.session_id.clone(),
        request.target.branch_id.as_deref(),
    );
    Ok(CheckedSubscribe {
        types: request.types.clone(),
        matches,
        target,
    })
}
