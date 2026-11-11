//! Version 2 product views of the Session broadcast; positions are branch sequences.
use dim_agent_capabilities::session_broadcast::SessionNotice;
use dim_agent_runtime::{
    BranchDirectoryEntry, BranchDirectoryState, BranchStatus, ModelProgress, TimelineInfo,
};
use dim_agent_types::{
    AgentEvent, ApprovalCallData, ApprovalDecision, BranchSource, EventKind, ModelFailureKind,
};
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

/// One branch directory record; `session.branch.get` also reads its display fields.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchDirectoryView {
    #[serde(flatten)]
    pub branch: BranchInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<dim_agent_types::BranchSource>,
    pub state: &'static str,
    pub purpose: Option<String>,
    pub cut_messages: Option<u64>,
    pub end: Option<dim_agent_types::BranchEndData>,
    pub head: u64,
    /// The line's `branch/end` sequence; absent while open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_event: Option<u64>,
}

impl From<BranchDirectoryEntry> for BranchDirectoryView {
    fn from(entry: BranchDirectoryEntry) -> Self {
        Self {
            branch: entry.info.clone().into(),
            source: entry.info.source,
            state: match entry.state {
                BranchDirectoryState::Open => "open",
                BranchDirectoryState::Ending => "ending",
                BranchDirectoryState::Ended => "ended",
            },
            purpose: None,
            cut_messages: None,
            end: None,
            head: entry.head,
            end_event: entry.end_event,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchesResult {
    pub branches: Vec<BranchDirectoryView>,
}

#[derive(Debug)]
pub struct ProductEvent {
    event: Arc<AgentEvent>,
}
impl ProductEvent {
    /// `version` is the connection's protocol version (2, 3 or 4).
    pub fn new(event: Arc<AgentEvent>, version: u32) -> Option<Self> {
        if event.parent_call_id.is_some() && version < 4 {
            return None;
        }
        let visible = match &event.kind {
            EventKind::ToolUseStart(_) => version >= 3,
            EventKind::ToolResult(data) => version >= 3 || data.call_event_id.is_some(),
            EventKind::InputWithdraw(_) | EventKind::InputDeleted(_) => version >= 3,
            EventKind::ContextCompact(_)
            | EventKind::ContextCompactionStart(_)
            | EventKind::ContextCompaction(_) => version >= 3,
            EventKind::BranchEnd(_) | EventKind::BranchEnded(_) => version >= 3,
            // A fork's opening is where an owner that folds the branch's own
            // history stops reading the copied facts (Goal A11).
            EventKind::SessionForked(_) => version >= 3,
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
            // v2/v3 keep their allowlists; extension events reach only a
            // dynamic protocol after host visibility filtering (ADR 001).
            | EventKind::Extension(_) => false,
        };
        visible.then_some(Self { event })
    }
}
impl Serialize for ProductEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let EventKind::ContextConfigured(configured) = &self.event.kind {
            let mut view = serde_json::json!({
                "id": self.event.id, "sequence": self.event.sequence, "create_at": self.event.create_at,
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
            if let Some(snapshot) = &configured.settings.capability_snapshot {
                view["data"]["capability_snapshot"] =
                    serde_json::to_value(snapshot).map_err(serde::ser::Error::custom)?;
            }
            view.serialize(serializer)
        } else {
            self.event.serialize(serializer)
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
    /// Work the submit checks did not do, for the caller to show (Hooks
    /// A.6.2). Only this reply carries them: a branch's first input is its
    /// prompt, so no check's warning enters it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<dim_agent_types::SubmitWarning>,
}
#[derive(Debug, Serialize)]
pub struct BarrierResult {
    pub sequence: u64,
}
/// `session.history`: a page of the branch's own facts. Each event is the
/// encoded form the reply carries, so the byte budget in
/// `connection::tree` counts the bytes the transport writes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResult {
    pub events: Vec<Box<serde_json::value::RawValue>>,
    pub through: u64,
    pub next_after: u64,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_start: Option<u64>,
}
/// `session.keys`: the live keyed blocks of one branch segment at `through`,
/// ascending by the carrying input's sequence and by block order inside one
/// input (keyed-blocks §2.2).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeysResult {
    pub through: u64,
    pub entries: Vec<KeyEntry>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyEntry {
    pub key: String,
    pub sequence: u64,
    pub input_id: String,
    pub text: String,
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
    /// Unix milliseconds at which the open thinking block started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_started_at: Option<u64>,
    pub text: String,
    /// Unicode scalar values of this request's `step/text` deltas before `text`.
    pub text_offset: u64,
    pub thinking: String,
    /// Unicode scalar values of this request's `step/thinking` deltas before `thinking`.
    pub thinking_offset: u64,
    pub tools: Vec<ToolProgress>,
    /// The retry this request is waiting to make, while it sleeps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryProgress>,
}
/// A retry a request is waiting for, shown while it sleeps.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryProgress {
    /// The attempt about to run, at least 2.
    pub attempt: u8,
    /// The total cap on attempts.
    pub max_attempts: u8,
    /// Unix milliseconds at which the attempt starts.
    pub retry_at_ms: u64,
    pub kind: ModelFailureKind,
    /// The failure's bounded reason.
    pub reason: Option<String>,
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
            thinking_started_at: progress.thinking_started_at,
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
            retry: progress.retry.map(|retry| RetryProgress {
                attempt: retry.attempt,
                max_attempts: retry.max_attempts,
                retry_at_ms: retry.retry_at_ms,
                kind: retry.kind,
                reason: retry.reason,
            }),
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

    /// A child batch reaches only v4; older clients keep their old projection.
    #[test]
    fn a_child_batch_parent_is_v4_only() {
        let event = Arc::new(AgentEvent {
            id: "child".into(),
            sequence: 2,
            create_at: 1,
            tags: None,
            parent_call_id: Some("c1".into()),
            kind: EventKind::ToolCall(ToolCallData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                calls: vec![ToolCall {
                    id: "c2".into(),
                    name: "echo".into(),
                    input: Default::default(),
                }],
            }),
        });
        assert!(ProductEvent::new(event.clone(), 2).is_none());
        assert!(ProductEvent::new(event.clone(), 3).is_none());
        let v4 = serde_json::to_value(ProductEvent::new(event, 4).unwrap()).unwrap();
        assert_eq!(v4["parent_call_id"], "c1");
    }

    #[test]
    fn a_fork_opening_is_a_v3_fact() {
        let opening = Arc::new(AgentEvent {
            id: "forked".into(),
            sequence: 9,
            create_at: 1,
            tags: None,
            parent_call_id: None,
            kind: EventKind::SessionForked(SessionForkedData {}),
        });
        assert!(ProductEvent::new(opening.clone(), 3).is_some());
        assert!(ProductEvent::new(opening, 2).is_none());
    }

    fn configured(config: Option<ModelConfig>, metadata: Option<RequestMetadata>) -> ProductEvent {
        ProductEvent {
            event: Arc::new(AgentEvent {
                id: "event".into(),
                sequence: 1,
                create_at: 0,
                tags: None,
                parent_call_id: None,
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
            }),
        }
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
