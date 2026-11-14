use dim_agent_types::MemberApprovalDescriptor;

/// The line that names the member's action: the tool the approval covers.
pub fn tool_line(descriptor: &MemberApprovalDescriptor) -> String {
    format!("Tool: {}", descriptor.call.name)
}

pub fn lines(descriptor: &MemberApprovalDescriptor) -> Vec<String> {
    let member = &descriptor.member;
    let mut lines = vec![
        format!(
            "Member: {}  Session: {}",
            member.agent_id, member.session_id
        ),
        format!("Member branch: {}", member.branch_id),
        tool_line(descriptor),
        format!("Member reason: {}", descriptor.reason),
    ];
    lines.extend(
        serde_json::to_string_pretty(&descriptor.call.input)
            .expect("tool input serializes")
            .lines()
            .map(str::to_owned),
    );
    lines
        .into_iter()
        .map(|line| crate::sanitize::safe(&line))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum DetailError {
    Unsupported,
    Failed(String),
}

impl DetailError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

impl From<crate::client::RpcError> for DetailError {
    fn from(error: crate::client::RpcError) -> Self {
        if error.code == -32601 {
            Self::Unsupported
        } else {
            Self::Failed(error.to_string())
        }
    }
}

impl std::fmt::Display for DetailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str(
                "This client/server combination cannot review the original member action",
            ),
            Self::Failed(message) => f.write_str(message),
        }
    }
}
