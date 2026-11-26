use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelId {
    pub provider_id: String,
    pub model_id: String,
}

/// Session configuration defaults and public description of one Agent;
/// fixed for the Agent's lifetime. Capabilities come from the process builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    /// The mount list of the Agent's Sessions; without one they declare
    /// every tool the process provides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional_non_null"
    )]
    pub model: Option<ModelId>,
}
