use dim_agent_capabilities::{
    methods::RpcError, prompt_state::PromptSnapshot, timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    AgentError, Branch, QueryEvent, QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect,
    QuerySpec, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::{
    AgentEvent, BranchCreateFailedData, ContentBlock, DEFAULT_BRANCH_ID, EventKind, FinishReason,
    MessagePart, ModelId, ModelOutputOutcome, TimelinePosition, TurnOutcome,
    markup::runtime_context,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use uuid::Uuid;

const CANDIDATES: u64 = 32;
const NEARBY: usize = 16;
const KINDS: &[&str] = &[
    "step/model-output-end",
    "turn/start",
    "turn/end",
    "branch/create-failed",
    "branch/start",
    "branch/resume",
    "branch/end",
    "branch/ended",
];
const ACTIVITY_KINDS: &[&str] = &["step/model-start", "step/tool-start", "step/tool-completed"];

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum ErrorReference {
    Observed {
        observed: Observed,
    },
    Timeline {
        branch_id: String,
        sequence: u64,
        observed: Observed,
    },
}

impl ErrorReference {
    pub(super) async fn capture(
        self,
        tree: Option<&TreeCommands>,
    ) -> Result<(Snapshot, Option<ModelId>), RpcError> {
        let (observed, branch, timeline) = match self {
            Self::Observed { observed } => {
                // A report raised without a reference reads the line the
                // Session's conversation is on; a provider without a Session
                // has none, and reads the root (design §9, §15).
                let branch = match tree {
                    Some(tree) => tree.default_branch()?,
                    None => DEFAULT_BRANCH_ID.to_owned(),
                };
                (observed, branch, Err(Missing::NoReference))
            }
            Self::Timeline {
                branch_id,
                sequence,
                observed,
            } => {
                let tree = tree.ok_or_else(|| RpcError::invalid("Expected an observed error"))?;
                if branch_id.is_empty() || branch_id.len() > 128 || sequence == 0 {
                    return Err(RpcError::invalid("Invalid failure reference"));
                }
                let timeline = read(tree, &branch_id, sequence, observed.operation).await;
                (observed, branch_id, timeline)
            }
        };
        if observed.client_version.is_empty() {
            return Err(RpcError::invalid("Client version is required"));
        }
        let client_version = Some(observed.client_version.clone());
        let default_model = tree
            .and_then(|tree| tree.get(&branch).ok())
            .as_ref()
            .and_then(adopted_model);
        Ok((
            Snapshot {
                error: Some(observed.into()),
                timeline,
                client_version,
                branch: None,
                conversation_through: None,
            },
            default_model,
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Observed {
    operation: Operation,
    code: Option<String>,
    #[serde(with = "dim_agent_capabilities::timestamp")]
    observed_at: Timestamp,
    client_version: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Operation {
    SessionRead,
    SessionFork,
    BranchCreate,
    ModelGeneration,
}

impl Operation {
    fn summary(self) -> &'static str {
        match self {
            Self::SessionRead => "Could not read conversation state",
            Self::SessionFork => "Could not fork the conversation",
            Self::BranchCreate => "Could not create the branch",
            Self::ModelGeneration => "Model generation failed",
        }
    }

    fn code(self, received: Option<&str>) -> &'static str {
        let registered: &[&str] = match self {
            Self::SessionRead => &[
                "session_closed",
                "session_build_failed",
                "session_protocol_version",
                "session_invalid_input",
                "branch_ended",
            ],
            Self::SessionFork | Self::BranchCreate => &[
                "session_closed",
                "session_invalid_input",
                "branch_ended",
                "operation_interrupted",
            ],
            Self::ModelGeneration => &[],
        };
        registered
            .iter()
            .copied()
            .find(|code| Some(*code) == received)
            .unwrap_or("operation_failed")
    }

    fn matches_failure(self, kind: &EventKind) -> bool {
        match (self, kind) {
            (
                Self::BranchCreate,
                EventKind::BranchCreateFailed(BranchCreateFailedData::Failed { .. }),
            ) => true,
            (Self::ModelGeneration, EventKind::ModelOutputEnd(end)) => {
                matches!(end.outcome, ModelOutputOutcome::Failed { .. })
            }
            _ => false,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CapturedError {
    operation: Operation,
    pub(super) summary: &'static str,
    #[serde(with = "dim_agent_capabilities::timestamp")]
    pub(super) observed_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) code: Option<&'static str>,
}

impl From<Observed> for CapturedError {
    fn from(observed: Observed) -> Self {
        Self {
            operation: observed.operation,
            summary: observed.operation.summary(),
            observed_at: observed.observed_at,
            code: Some(observed.operation.code(observed.code.as_deref())),
        }
    }
}

pub(super) struct Snapshot {
    pub(super) error: Option<CapturedError>,
    pub(super) timeline: Result<TimelineEvidence, Missing>,
    /// The label the reporting client sent for itself, when it sent one: a
    /// comparison with the daemon's own label is what a report shows.
    pub(super) client_version: Option<String>,
    /// The line the snapshot's conversation was read from, and its cutoff: a
    /// report's evidence stays on the line the Session's conversation was on
    /// when it was prepared (design §9, §15).
    pub(super) branch: Option<String>,
    pub(super) conversation_through: Option<u64>,
}

impl Snapshot {
    pub(super) async fn capture_session(tree: &TreeCommands) -> (Self, Option<ModelId>) {
        let default = tree.default_branch().ok();
        let branch = default.as_deref().and_then(|id| tree.get(id).ok());
        let conversation_through = branch.as_ref().map(Branch::sequence);
        let timeline = match (default.as_deref(), conversation_through) {
            (Some(id), Some(through)) => read_window(tree, id, through, ACTIVITY_KINDS)
                .await
                .map(|(events, truncated)| {
                    let (recent, truncated) = excerpt(&events, truncated, true);
                    TimelineEvidence::Recent { recent, truncated }
                })
                .map_err(|_| Missing::ReadFailed),
            _ => Err(Missing::ReadFailed),
        };
        let model = branch.as_ref().and_then(adopted_model);
        (
            Self {
                error: None,
                timeline,
                client_version: None,
                branch: default,
                conversation_through,
            },
            model,
        )
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConversationExcerpt {
    pub(super) role: &'static str,
    pub(super) sequence: u64,
    pub(super) text: String,
}

pub(super) async fn capture_conversation(
    tree: &TreeCommands,
    branch: &str,
    through: u64,
) -> Vec<ConversationExcerpt> {
    let Ok((events, _)) = read_window(
        tree,
        branch,
        through,
        &[
            "input/message",
            "input/consumed",
            "step/model-start",
            "step/text",
            "step/model-content",
        ],
    )
    .await
    else {
        return Vec::new();
    };
    let mut excerpts = Vec::new();
    let mut pending = std::collections::HashMap::new();
    let mut output: Option<(String, String)> = None;
    for row in events {
        let event = row.event;
        if event.parent_call_id.is_some() {
            continue;
        }
        let selected = match &event.kind {
            EventKind::InputMessage(input) => {
                if let Some(user) = &input.user {
                    let mut text = String::new();
                    for (index, block) in user.content.iter().enumerate() {
                        if let ContentBlock::Text { text: value } = block
                            && !runtime_context(index, value)
                        {
                            append_visible(&mut text, value);
                        }
                        if text.len() == 2048 {
                            break;
                        }
                    }
                    pending.insert(event.id.clone(), text);
                }
                None
            }
            EventKind::InputConsumed(data) => {
                pending.remove(&data.input_id).map(|text| ("user", text))
            }
            EventKind::ModelStart(data) => {
                output = Some((data.request_id.clone(), String::new()));
                None
            }
            EventKind::Text(data) => {
                if let Some((request, text)) = &mut output
                    && *request == data.request_id
                {
                    append_bounded(text, &data.delta, 2048);
                }
                None
            }
            EventKind::ModelContent(data) => {
                let mut text = String::new();
                for part in &data.content {
                    if let MessagePart::Text { text: value, .. } = part {
                        append_bounded(&mut text, value, 2048);
                    }
                }
                output = Some((data.request_id.clone(), text));
                None
            }
            EventKind::ModelOutputEnd(data) => output.take().and_then(|(request, text)| {
                (request == data.request_id
                    && matches!(
                        data.outcome,
                        ModelOutputOutcome::Completed {
                            finish_reason: FinishReason::EndTurn | FinishReason::StopSequence
                        }
                    ))
                .then_some(("assistant", text))
            }),
            _ => None,
        };
        if let Some((role, text)) = selected {
            let mut selected = String::new();
            append_visible(&mut selected, &text);
            if !selected.trim().is_empty() {
                excerpts.push(ConversationExcerpt {
                    role,
                    sequence: event.sequence,
                    text: selected,
                });
            }
        }
    }
    let mut budget = 8 * 1024;
    let mut recent = Vec::new();
    for mut excerpt in excerpts.into_iter().rev().take(6) {
        if budget == 0 {
            break;
        }
        let mut text = String::new();
        append_bounded(&mut text, &excerpt.text, budget);
        excerpt.text = text;
        budget -= excerpt.text.len();
        recent.push(excerpt);
    }
    recent.reverse();
    recent
}

fn append_bounded(target: &mut String, text: &str, limit: usize) {
    let mut end = text.len().min(limit.saturating_sub(target.len()));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}

fn append_visible(target: &mut String, mut text: &str) {
    while !text.is_empty() && target.len() < 2048 {
        let end = text.find(['\r', '\n']).unwrap_or(text.len());
        let (line, rest) = text.split_at(end);
        let rest = rest.strip_prefix('\r').unwrap_or(rest);
        text = rest.strip_prefix('\n').unwrap_or(rest);
        let mut bounded = String::new();
        append_bounded(&mut bounded, line, 2048);
        if eligible_line(&bounded) {
            if !target.is_empty() {
                append_bounded(target, "\n", 2048);
            }
            append_bounded(target, &bounded, 2048);
        }
    }
}

fn eligible_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ![
        "bearer ",
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "password",
        "secret",
        "token=",
        "token:",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "-----begin",
        "akia",
        "eyj",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

fn adopted_model(branch: &Branch) -> Option<ModelId> {
    let configuration = branch.get::<PromptSnapshot>()?.configuration?;
    Some(ModelId {
        provider_id: configuration.provider_id,
        model_id: configuration.model_id,
    })
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Missing {
    NoReference,
    FailureUnavailable,
    ReadFailed,
    Omitted,
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub(super) enum TimelineEvidence {
    Failure {
        failure: Record,
        nearby: Vec<Record>,
        #[serde(skip_serializing_if = "Option::is_none")]
        correlation: Option<Correlation>,
        truncated: bool,
    },
    Recent {
        recent: Vec<Record>,
        truncated: bool,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Record {
    kind: &'static str,
    timestamp_ms: u64,
    sequence: u64,
    outcome: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Correlation {
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<Uuid>,
}

fn project(event: &AgentEvent, include_completed_model: bool) -> Option<Record> {
    let outcome = match &event.kind {
        EventKind::ModelOutputEnd(end) => match end.outcome {
            ModelOutputOutcome::Failed { .. } => "failed",
            ModelOutputOutcome::Cancelled { .. } => "cancelled",
            ModelOutputOutcome::Completed { .. } if include_completed_model => "completed",
            ModelOutputOutcome::Completed { .. } => return None,
        },
        EventKind::TurnEnd(end) => match end.outcome {
            TurnOutcome::Failed { .. } => "failed",
            TurnOutcome::Cancelled { .. } => "cancelled",
            TurnOutcome::Completed { .. } => "completed",
        },
        EventKind::BranchCreateFailed(BranchCreateFailedData::Failed { .. }) => "failed",
        EventKind::BranchCreateFailed(BranchCreateFailedData::Cancelled { .. }) => "cancelled",
        EventKind::TurnStart(_)
        | EventKind::BranchStart(_)
        | EventKind::ModelStart(_)
        | EventKind::ToolStart(_) => "started",
        EventKind::ToolCompleted(end) if end.result.is_error == Some(true) => "failed",
        EventKind::ToolCompleted(_) => "completed",
        EventKind::BranchResume(_) => "resumed",
        EventKind::BranchEnd(_) | EventKind::BranchEnded(_) => "ended",
        _ => return None,
    };
    let kind = KINDS
        .iter()
        .chain(ACTIVITY_KINDS)
        .copied()
        .find(|kind| *kind == event.kind.key())?;
    Some(Record {
        kind,
        timestamp_ms: event.create_at,
        sequence: event.sequence,
        outcome,
    })
}

async fn read(
    tree: &TreeCommands,
    branch: &str,
    sequence: u64,
    operation: Operation,
) -> Result<TimelineEvidence, Missing> {
    let (mut events, truncated) =
        read_window(tree, branch, sequence, &[])
            .await
            .map_err(|error| match error {
                AgentError::UnknownBranch(_) | AgentError::InvalidInput(_) => {
                    Missing::FailureUnavailable
                }
                _ => Missing::ReadFailed,
            })?;
    let anchor = events.pop().ok_or(Missing::FailureUnavailable)?.event;
    if anchor.sequence != sequence || !operation.matches_failure(&anchor.kind) {
        return Err(Missing::FailureUnavailable);
    }
    let failure = project(&anchor, false).ok_or(Missing::FailureUnavailable)?;
    let (turn_id, request_id) = match &anchor.kind {
        EventKind::ModelOutputEnd(end) => (
            Uuid::parse_str(&end.turn_id).ok(),
            Uuid::parse_str(&end.request_id).ok(),
        ),
        _ => (None, None),
    };
    let event_id = Uuid::parse_str(&anchor.id).ok();
    let correlation =
        (event_id.is_some() || turn_id.is_some() || request_id.is_some()).then_some(Correlation {
            event_id,
            turn_id,
            request_id,
        });
    let (nearby, truncated) = excerpt(&events, truncated, false);
    Ok(TimelineEvidence::Failure {
        failure,
        nearby,
        correlation,
        truncated,
    })
}

async fn read_window(
    tree: &TreeCommands,
    branch: &str,
    through: u64,
    extra_kinds: &[&str],
) -> Result<(Vec<QueryEvent>, bool), AgentError> {
    if through == 0 {
        return Ok((Vec::new(), false));
    }
    let from = through.saturating_sub(CANDIDATES - 1).max(1);
    let result = tree
        .query(
            branch.to_owned(),
            TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: Some(TimelinePosition {
                    branch_id: branch.to_owned(),
                    sequence: from,
                }),
                through: Some(through),
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: KINDS
                            .iter()
                            .chain(extra_kinds)
                            .map(|kind| (*kind).into())
                            .collect(),
                    }),
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::new(CANDIDATES).unwrap(),
                    after: None,
                }),
            },
        )
        .await?;
    let QueryOutput::Events { events } = result.result else {
        unreachable!("event selection returns events");
    };
    Ok((events, from > 1))
}

fn excerpt(
    events: &[QueryEvent],
    truncated: bool,
    include_completed_model: bool,
) -> (Vec<Record>, bool) {
    let mut records: Vec<_> = events
        .iter()
        .filter_map(|row| project(&row.event, include_completed_model))
        .collect();
    let omitted = records.len().saturating_sub(NEARBY);
    records.drain(..omitted);
    (records, truncated || omitted > 0)
}
