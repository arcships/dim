use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionPreset {
    #[default]
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileAccess {
    ReadOnly,
    WorkspaceReadWrite,
    Unrestricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolApproval {
    Ask,
    Bypass,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionPolicy {
    #[serde(default)]
    pub preset: PermissionPreset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_access: Option<FileAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_approval: Option<ToolApproval>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
}

impl PermissionPolicy {
    pub fn file_access(&self) -> FileAccess {
        self.file_access.unwrap_or(match self.preset {
            PermissionPreset::ReadOnly => FileAccess::ReadOnly,
            PermissionPreset::WorkspaceWrite => FileAccess::WorkspaceReadWrite,
            PermissionPreset::FullAccess => FileAccess::Unrestricted,
        })
    }

    pub fn tool_approval(&self) -> ToolApproval {
        self.tool_approval.unwrap_or(match self.preset {
            PermissionPreset::FullAccess => ToolApproval::Bypass,
            _ => ToolApproval::Ask,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionChangedData {
    pub policy: PermissionPolicy,
}

/// Characters that end a simple command: shell separators, redirection and
/// substitution. A command rule never contains one.
pub const COMMAND_SEPARATORS: &[char] = &[';', '&', '|', '`', '(', ')', '<', '>', '$', '\n', '\r'];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandMatch {
    Exact,
    Prefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandRuleDecision {
    Allow,
    Deny,
}

/// A user's rule for commands of process-starting tool calls; `command` is
/// compared word by word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRule {
    pub command: String,
    #[serde(rename = "match")]
    pub matching: CommandMatch,
    pub decision: CommandRuleDecision,
}
