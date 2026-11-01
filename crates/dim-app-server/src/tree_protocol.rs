//! Version 2 product views of the Session broadcast; positions are branch sequences.
use dim_agent_capabilities::session_broadcast::SessionNotice;
use dim_agent_runtime::{BranchStatus, ModelProgress, TimelineInfo};
use dim_agent_types::{AgentEvent, ApprovalCallData, ApprovalDecision, BranchSource, EventKind};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    pub branch_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_point: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_event: Option<u64>,
}
impl From<TimelineInfo> for BranchInfo {
    fn from(info: TimelineInfo) -> Self {
        Self {
            branch_id: info.id,
            parent_branch_id: info.parent,
            branch_point: info.branch_point,
            create_event: info.create_anchor,
        }
    }
}

#[derive(Debug)]
pub struct ProductEvent(Arc<AgentEvent>);
impl ProductEvent {
    /// `version` is the connection's protocol version (2 or 3).
    pub fn new(event: Arc<AgentEvent>, version: u32) -> Option<Self> {
        let visible = match &event.kind {
            EventKind::ToolUseStart(_) => version >= 3,
            EventKind::ToolResult(data) => version >= 3 || data.call_event_id.is_some(),
            EventKind::InputWithdraw(_) | EventKind::InputDeleted(_) => version >= 3,
            EventKind::ContextCompact(_)
            | EventKind::ContextCompactionStart(_)
            | EventKind::ContextCompaction(_) => version >= 3,
            EventKind::BranchEnd(_) | EventKind::BranchEnded(_) => version >= 3,
            EventKind::InputMessage(_)
            | EventKind::InputApplied(_)
            | EventKind::InputConsumed(_)
            | EventKind::ExecStarted(_)
            | EventKind::ExecEnded(_)
            | EventKind::PermissionChanged(_)
            | EventKind::StopInterrupt(_)
            | EventKind::TurnStart(_)
            | EventKind::TurnEnd(_)
            | EventKind::ContextConfigured(_)
            | EventKind::ContextScope(_)
            | EventKind::ModelUsage(_)
            | EventKind::ModelStart(_)
            | EventKind::Thinking(_)
            | EventKind::Text(_)
            | EventKind::ToolUse(_)
            | EventKind::ModelOutputEnd(_)
            | EventKind::ToolCall(_)
            | EventKind::ToolStart(_)
            | EventKind::ToolCompleted(_)
            | EventKind::BranchCreate(_)
            | EventKind::BranchStart(_)
            | EventKind::BranchCreateFailed(_)
            | EventKind::CollaborationRequest(_)
            | EventKind::CollaborationCancel(_)
            | EventKind::CollaborationClose(_)
            | EventKind::CollaborationMessage(_)
            | EventKind::CollaborationDelivered(_)
            | EventKind::CollaborationResult(_) => true,
            EventKind::ModelContent(_)
            | EventKind::ApprovalRequest(_)
            | EventKind::ApprovalRespond(_)
            | EventKind::ApprovalCancelled(_)
            | EventKind::PromptChanged(_)
            | EventKind::PromptConfigureRequest(_)
            | EventKind::PromptConfigureResult(_)
            | EventKind::ContextPrepare(_)
            | EventKind::ContextBudget(_)
            | EventKind::ModelCall(_)
            | EventKind::BranchResume(_)
            | EventKind::SessionForked(_)
            // v2/v3 keep their allowlists; extension events reach only a
            // dynamic protocol after host visibility filtering (ADR 001).
            | EventKind::Extension(_) => false,
        };
        visible.then_some(Self(event))
    }
}
impl Serialize for ProductEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let EventKind::ContextConfigured(configured) = &self.0.kind {
            let mut view = serde_json::json!({
                "id": self.0.id, "sequence": self.0.sequence, "create_at": self.0.create_at,
                "type": "context", "name": "configured",
                "data": {"timeline": configured.timeline, "revision": configured.revision,
                    "previous": configured.previous, "prompt": configured.prompt,
                    "provider_id": configured.settings.provider_id,
                    "model_id": configured.settings.model_id}
            });
            if let Some(preferences) = crate::provider::adopted_selection_preferences(
                configured.settings.config.as_ref(),
                configured.settings.metadata.as_ref(),
            ) {
                view["data"]["model_preferences"] =
                    serde_json::to_value(preferences).map_err(serde::ser::Error::custom)?;
            }
            view.serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchState {
    pub branch_id: String,
    pub running: bool,
    pub stopping: bool,
}
impl BranchState {
    pub fn new(branch_id: String, status: &BranchStatus) -> Self {
        Self {
            branch_id,
            running: status.running,
            stopping: status.stopping,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct CreateResult {
    pub branch: BranchInfo,
}
#[derive(Debug, Serialize)]
pub struct BarrierResult {
    pub sequence: u64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResult {
    pub events: Vec<ProductEvent>,
    pub through: u64,
    pub next_after: u64,
    pub done: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNotice {
    pub session_id: String,
    pub branch_id: String,
    pub event: ProductEvent,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateNotice {
    pub session_id: String,
    #[serde(flatten)]
    pub state: BranchState,
}
/// v3 `session.notice`: an owner's Session notice with its fields unchanged.
/// No sequence and no replay (design: appserver/session-notices.md).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardedNotice {
    pub session_id: String,
    #[serde(flatten)]
    pub notice: Arc<SessionNotice>,
}
/// The branch's model output not yet written to the Timeline (ADR agent-029).
/// Display state: no sequence, never persisted, each notice replaces the last.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProgressNotice {
    pub session_id: String,
    pub branch_id: String,
    pub turn_id: Option<String>,
    pub request_id: Option<String>,
    /// Stable identity of the open text block, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_part_id: Option<String>,
    /// Stable identity of the open thinking block, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_part_id: Option<String>,
    pub text: String,
    /// Unicode scalar values of this request's `step/text` deltas before `text`.
    pub text_offset: u64,
    pub thinking: String,
    /// Unicode scalar values of this request's `step/thinking` deltas before `thinking`.
    pub thinking_offset: u64,
    pub tools: Vec<ToolProgress>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProgress {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}
impl ModelProgressNotice {
    pub fn new(session_id: String, branch_id: String, progress: ModelProgress) -> Self {
        Self {
            session_id,
            branch_id,
            turn_id: progress.turn_id,
            request_id: progress.request_id,
            text_part_id: progress.text_part_id,
            thinking_part_id: progress.thinking_part_id,
            text: progress.text,
            text_offset: progress.text_offset,
            thinking: progress.thinking,
            thinking_offset: progress.thinking_offset,
            tools: progress
                .tools
                .into_iter()
                .map(|tool| ToolProgress {
                    call_id: tool.call_id,
                    name: tool.name,
                    arguments: tool.arguments,
                })
                .collect(),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: String,
    pub source: BranchSource,
    pub call: ApprovalCallData,
    pub reason: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCancelled {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: String,
    pub reason: String,
}
/// A committed `approval/respond`. Approval accepts the first response it
/// consumes, in sequence order, so the first notice per `requestId` carries the
/// accepted decision; a later one was answered `Unknown`. A following
/// `approval.cancelled` means the call did not start.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolved {
    pub session_id: String,
    pub branch_id: String,
    pub request_id: String,
    pub decision: ApprovalDecision,
}

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_types::*;

    fn configured(config: Option<ModelConfig>, metadata: Option<RequestMetadata>) -> ProductEvent {
        ProductEvent(Arc::new(AgentEvent {
            id: "event".into(),
            sequence: 1,
            create_at: 0,
            tags: None,
            kind: EventKind::ContextConfigured(ContextConfiguredData {
                timeline: "main".into(),
                revision: "revision".into(),
                previous: None,
                prompt: None,
                settings: Box::new(ModelConfiguration {
                    provider_id: "provider".into(),
                    model_id: "model".into(),
                    capability_snapshot: None,
                    tools: None,
                    tool_sources: Vec::new(),
                    config,
                    metadata,
                }),
                system: None,
                system_source: None,
            }),
        }))
    }

    fn effort() -> ModelConfig {
        ModelConfig {
            reasoning: Some(ProviderReasoningConfig {
                effort: Some("high".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn protocol(value: serde_json::Value) -> RequestMetadata {
        RequestMetadata {
            extensions: serde_json::Map::from_iter([("providerProtocol".into(), value)]),
            ..Default::default()
        }
    }

    #[test]
    fn a_stored_event_without_a_protocol_projects_no_model_preferences() {
        for event in [
            configured(Some(effort()), None),
            configured(
                Some(effort()),
                Some(protocol(serde_json::json!("no-such-protocol"))),
            ),
            configured(None, Some(protocol(serde_json::json!("openai-chat")))),
        ] {
            let view = serde_json::to_value(&event).unwrap();
            assert!(view["data"].get("model_preferences").is_none(), "{view}");
        }
        let view = serde_json::to_value(configured(
            Some(effort()),
            Some(protocol(serde_json::json!("openai-chat"))),
        ))
        .unwrap();
        assert_eq!(
            view["data"]["model_preferences"],
            serde_json::json!({"reasoningEffort": "high"}),
            "{view}"
        );
    }
}
