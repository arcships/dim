use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The process boundary a command requests; wider tiers expose more host access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sandbox {
    Workspace,
    Network,
    Host,
}

impl Sandbox {
    pub fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Network => "network",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IsolationEscalation {
    Deny,
    Ask,
}

/// The node's editable policy file. Omitted lists use resource defaults; empty lists grant none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolationPolicy {
    pub version: u8,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub enabled: Option<bool>,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub network: Option<bool>,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub sandbox: Option<Sandbox>,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub escalation: Option<IsolationEscalation>,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_paths: Option<Vec<PathBuf>>,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub writable_paths: Option<Vec<PathBuf>>,
    #[serde(
        default,
        deserialize_with = "crate::optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub environment: Option<Vec<String>>,
}

impl Default for IsolationPolicy {
    fn default() -> Self {
        Self {
            version: 2,
            enabled: None,
            network: None,
            sandbox: Some(Sandbox::Workspace),
            escalation: Some(IsolationEscalation::Ask),
            read_only_paths: None,
            writable_paths: None,
            environment: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IsolationPolicySource {
    Defaults,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationPolicyEffective {
    pub sandbox: Sandbox,
    pub escalation: IsolationEscalation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationPolicySnapshot {
    pub revision: String,
    pub path: PathBuf,
    pub source: IsolationPolicySource,
    pub policy: IsolationPolicy,
    pub effective: IsolationPolicyEffective,
}
