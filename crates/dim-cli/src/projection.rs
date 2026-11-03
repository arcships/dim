//! Folds App Server session events into the transcript the screen draws.
//!
//! The worker applies every event once, in sequence order, and publishes
//! [`ViewSnapshot`]s built here. Only the rows the screen needs are kept; the
//! durable history stays with the App Server.

use std::collections::HashMap;
use std::sync::Arc;

use dim_agent_types::{
    BranchCreateAt, BranchCreateFailedData, BranchSource, CollaborationDelivery,
    CollaborationOutcome, CollaborationTarget, ContentBlock, ContextCompactionResult, ExecStatus,
    InputMode, InsufficientSpacePartName, JsonObject, Message, MessageContent, MessagePart,
    ModelOutputOutcome, ReplacementParts, ToolContent, ToolResult, TurnOutcome, UserInput,
};
use serde_json::Value;

use crate::model::shell::ShellRun;
use crate::model::view::{
    ApprovalView, CompactionPart, CompactionPartSize, CompactionState, CompactionView, LineCut,
    LineState, LineView, MemberCard, MemberConversationState, MemberTarget, QueuedInput, RowKind,
    RowTone, TaskFinish, TaskKind, TaskTarget, TaskView, TurnEnd, ViewRow, ViewSnapshot,
    WorkflowView,
};
use crate::wire::{Event, EventContent, Progress, WorkflowRun, WorkflowState};

/// Reasoning that is still arriving: the row shows what the Timeline holds
/// plus the progress not written yet.
struct OpenThought {
    index: usize,
    /// The request's `step/thinking` deltas so far.
    written: String,
}

#[derive(Default)]
pub struct Projection {
    owner: String,
    /// The lines of the Session, as `main` recorded them (design §14).
    lines: Vec<LineView>,
    /// Each line's own user messages, by input id and submission sequence,
    /// to count where a line was cut (timeline UI §6); a withdrawn one is
    /// dropped.
    messages: HashMap<String, Vec<(String, u64)>>,
    /// Shared with the published snapshots: a snapshot copies pointers, and
    /// a row is copied only when it changes while a snapshot still holds it
    /// (in practice the one that is streaming).
    rows: Vec<Arc<ViewRow>>,
    /// The assistant row each model request streams into.
    assistant_rows: HashMap<String, usize>,
    /// When each running model request started, to time its reasoning.
    request_starts: HashMap<String, u64>,
    /// The reasoning row each model request streams into while it is open.
    thinking_rows: HashMap<String, OpenThought>,
    /// The tool row each call id opened.
    tool_rows: HashMap<String, usize>,
    /// The compaction row each `context/compaction-start` opened, by its
    /// sequence: the result names that start.
    compaction_rows: HashMap<u64, usize>,
    running: bool,
    approvals: Vec<ApprovalView>,
    pub questions: crate::question::Pending,
    /// Written messages without their `input/applied` yet. Each enters the
    /// transcript when it comes, so the transcript keeps the order the model
    /// sees and a withdrawn input never becomes a row; the queue strip shows
    /// them while a turn runs. Withdrawal re-submits the kept `UserInput` as
    /// Steer. Each keeps its input id and the sequence it was submitted at.
    held: Vec<(String, u64, UserInput)>,
    /// The row each written user message draws in, by input id, until its
    /// `input/consumed` settles that row.
    input_rows: HashMap<String, usize>,
    /// Unfinished tasks and the latest 50 terminal results.
    tasks: Vec<TaskView>,
    /// The `exec` call each background task came from, until it ends.
    background: HashMap<TaskTarget, String>,
    /// The newest turn end: its sequence and how the turn ended.
    ended: Option<(u64, TurnEnd)>,
    presented_failure: Option<String>,
    /// The call row carrying each delegation's card, by request event id.
    cards: HashMap<String, usize>,
    /// Native Agent names from `agent.list`, by Agent id.
    agent_names: HashMap<String, String>,
    /// Agent ids a finished `agent.list` read was asked to name, whether or
    /// not it returned them: only a card naming another id reads again.
    agents_read: std::collections::HashSet<String>,
    /// The shown branch's goal, folded from its own events (Goal §A.3).
    goal: crate::goal::Fold,
}

impl Projection {
    pub fn for_branch(branch: &str) -> Self {
        Self {
            owner: branch.to_owned(),
            ..Self::default()
        }
    }

    /// Whether `branch_id` is the line this projection draws.
    fn owns(&self, branch_id: &str) -> bool {
        branch_id
            == if self.owner.is_empty() {
                "main"
            } else {
                &self.owner
            }
    }

    /// Folds one line's creation or end into the Session's lines, whichever
    /// line recorded it: `parent` is the line whose history holds the fact
    /// (design §14).
    fn lines(&mut self, parent: &str, event: &Event) {
        match &event.content {
            EventContent::Input(data) if data.user.is_some() => {
                self.messages
                    .entry(parent.to_owned())
                    .or_default()
                    .push((event.id.clone(), event.sequence));
            }
            EventContent::InputDeleted(data) => {
                if let Some(messages) = self.messages.get_mut(parent) {
                    messages.retain(|(id, _)| *id != data.input_id);
                }
            }
            EventContent::BranchCreate(data) => {
                let purpose = data
                    .prompt
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if !self.lines.iter().any(|line| line.id == data.branch) {
                    let sequence = match data.at {
                        BranchCreateAt::Sequence { sequence } => sequence,
                        BranchCreateAt::Create {} => event.sequence,
                    };
                    let inherited = self
                        .lines
                        .iter()
                        .find(|line| line.id == parent)
                        .and_then(|line| line.cut)
                        .map_or(0, |cut| cut.messages);
                    let own = self.messages.get(parent).map_or(0, |messages| {
                        messages
                            .iter()
                            .take_while(|(_, at)| *at <= sequence)
                            .count()
                    });
                    self.lines.push(LineView {
                        id: data.branch.clone(),
                        parent: Some(parent.to_owned()),
                        purpose,
                        state: LineState::Waiting,
                        cut: Some(LineCut {
                            sequence,
                            messages: inherited + own,
                        }),
                    });
                }
            }
            EventContent::BranchCreateFailed(data) => {
                let branch = match data {
                    BranchCreateFailedData::Failed { branch, .. }
                    | BranchCreateFailedData::Cancelled { branch, .. } => branch,
                };
                if let Some(line) = self.lines.iter_mut().find(|line| line.id == *branch) {
                    line.state = LineState::CreationFailed;
                }
            }
            EventContent::BranchEnded(data) => {
                let state = match (&data.source, &data.ancestor) {
                    (_, Some(ancestor)) => LineState::EndedWith(ancestor.clone()),
                    (BranchSource::Tool { .. }, None) => LineState::EndedItself,
                    (BranchSource::Host { .. }, None) => LineState::EndedByOperation,
                };
                if let Some(line) = self.lines.iter_mut().find(|line| line.id == data.branch) {
                    line.state = state;
                }
            }
            _ => {}
        }
    }

    pub fn apply(&mut self, branch_id: &str, event: &Event) {
        match &event.content {
            EventContent::ToolCall(data) => {
                self.questions.asked(branch_id, &event.id, data);
                if self.owns(branch_id) {
                    for call in data
                        .calls
                        .iter()
                        .filter(|call| call.name == crate::question::TOOL)
                    {
                        if let Some(index) = self.tool_rows.get(&call.id).copied() {
                            self.row(index).parked = true;
                        }
                    }
                }
            }
            EventContent::ToolResult(data) => {
                self.questions.answered(branch_id, data);
            }
            _ => {}
        }
        self.lines(branch_id, event);
        if !self.owns(branch_id)
            && !matches!(
                &event.content,
                EventContent::ExecStarted(_)
                    | EventContent::ExecEnded(_)
                    | EventContent::CollaborationRequest(_)
                    | EventContent::CollaborationDelivered(_)
                    | EventContent::CollaborationResult(_)
            )
        {
            return;
        }
        self.goal.apply(event);
        let sequence = event.sequence;
        match &event.content {
            EventContent::Input(data) => {
                if let Some(user) = &data.user {
                    self.held.push((event.id.clone(), sequence, user.clone()));
                } else if let Some(text) = data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    ShellRun::parse(text).is_some().then(|| text.clone())
                }) {
                    self.push(format!("user-{sequence}"), RowKind::Shell, text);
                } else if let Some(row) = data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    branch_notice(text, data.mode)
                }) {
                    self.push(format!("line-{sequence}"), RowKind::Status, row);
                }
            }
            EventContent::InputApplied(data) => {
                if let Some(index) = self.held.iter().position(|(id, _, _)| *id == data.input_id) {
                    let (id, submitted, user) = self.held.remove(index);
                    self.push_user(&id, sequence, submitted, &user);
                }
            }
            EventContent::InputDeleted(data) => {
                self.held.retain(|(id, _, _)| *id != data.input_id);
            }
            EventContent::BranchCreate(data) => {
                let id = data.branch.get(..8).unwrap_or(&data.branch);
                let purpose = data.prompt.lines().next().unwrap_or_default().trim();
                let mut text = if purpose.is_empty() {
                    format!("line {id}")
                } else {
                    purpose.to_owned()
                };
                let at = match data.at {
                    BranchCreateAt::Sequence { sequence } => sequence,
                    BranchCreateAt::Create {} => sequence,
                };
                if let Some(message) = self.rows.iter().find(|row| row.cut == Some(at)) {
                    let first = message.text.lines().next().unwrap_or_default();
                    let mut quoted: String = first.chars().take(40).collect();
                    if quoted.len() < first.len() {
                        quoted.push('…');
                    }
                    text.push_str(&format!(" · before “{quoted}”"));
                }
                if !purpose.is_empty() {
                    text.push_str(&format!(" · {id}"));
                }
                let index = self.push(format!("opened-{sequence}"), RowKind::Status, text);
                self.row(index).line = Some(data.branch.clone());
            }
            EventContent::BranchEnd(data) => {
                let issuer = match (&data.source, &data.ancestor) {
                    (_, Some(ancestor)) => format!("with {ancestor}"),
                    (BranchSource::Tool { .. }, None) => "itself".into(),
                    (BranchSource::Host { .. }, None) => "an operation".into(),
                };
                self.push(
                    format!("end-{sequence}"),
                    RowKind::Status,
                    format!("This line ended ({issuer})"),
                );
            }
            EventContent::ExecStarted(data) => {
                let label = self
                    .rows
                    .iter()
                    .find(|row| row.row_id == data.call_id)
                    .and_then(|row| row.tool_input.as_ref()?.get("command")?.as_str())
                    .filter(|command| !command.is_empty())
                    .unwrap_or(data.description.as_str())
                    .to_owned();
                let target = TaskTarget {
                    branch_id: branch_id.to_owned(),
                    task_id: data.task_id.clone(),
                };
                self.tasks.push(TaskView {
                    id: event.id.clone(),
                    branch_id: branch_id.to_owned(),
                    kind: TaskKind::Command(target.clone()),
                    label,
                    started_at: event.create_at,
                    finished: None,
                });
                self.background.insert(target, data.call_id.clone());
            }
            EventContent::ExecEnded(data) => {
                let target = TaskTarget {
                    branch_id: branch_id.to_owned(),
                    task_id: data.task_id.clone(),
                };
                if let Some(task) = self
                    .tasks
                    .iter_mut()
                    .find(|task| matches!(&task.kind, TaskKind::Command(at) if at == &target))
                {
                    task.finished = Some(TaskFinish {
                        at: event.create_at,
                        status: match data.status {
                            ExecStatus::Completed => "Completed",
                            ExecStatus::Failed => "Failed",
                            ExecStatus::Cancelled => "Cancelled",
                        },
                        failed: data.status == ExecStatus::Failed
                            || data.result.is_error == Some(true),
                        text: result_text(&data.result),
                        exit_code: data
                            .result
                            .structured_content
                            .as_ref()
                            .and_then(|value| value.get("exitCode"))
                            .and_then(Value::as_i64),
                    });
                }
                if let Some(call_id) = self.background.remove(&target)
                    && self.owns(branch_id)
                    && let Some(index) = self.rows.iter().position(|row| row.row_id == call_id)
                {
                    let row = self.row(index);
                    row.result_is_error =
                        data.status != ExecStatus::Completed || data.result.is_error == Some(true);
                    row.result_text = Some(result_text(&data.result));
                    row.exit_code = data
                        .result
                        .structured_content
                        .as_ref()
                        .and_then(|value| value.get("exitCode"))
                        .and_then(Value::as_i64);
                }
            }
            EventContent::CollaborationRequest(data) => {
                let task = TaskView {
                    id: event.id.clone(),
                    branch_id: branch_id.to_owned(),
                    kind: TaskKind::Member(MemberTarget::Starting),
                    label: data.prompt.lines().next().unwrap_or("").to_owned(),
                    started_at: event.create_at,
                    finished: None,
                };
                if self.owns(branch_id)
                    && let Some(index) = self.tool_rows.get(&data.call_id).copied()
                    && self.rows[index].batch.as_deref() == Some(data.request_id.as_str())
                    && self.rows[index].member.is_none()
                {
                    let (agent, agent_id) = match &data.target {
                        CollaborationTarget::Agent { agent_id }
                        | CollaborationTarget::Member { agent_id, .. } => {
                            let name = self.agent_names.get(agent_id).unwrap_or(agent_id);
                            (
                                name.clone(),
                                (!agent_id.contains(':')).then(|| agent_id.clone()),
                            )
                        }
                        CollaborationTarget::Temporary { profile } => (profile.name.clone(), None),
                    };
                    self.row(index).member = Some(Box::new(MemberCard {
                        task: task.clone(),
                        agent,
                        agent_id,
                        prompt: data.prompt.clone(),
                        model: None,
                        activity: None,
                        observation: None,
                    }));
                    self.cards.insert(event.id.clone(), index);
                }
                self.tasks.push(task);
            }
            EventContent::CollaborationDelivered(data) => {
                let starting = |task: &TaskView| {
                    task.branch_id == branch_id
                        && task.id == data.op_event_id
                        && task.finished.is_none()
                        && task.kind == TaskKind::Member(MemberTarget::Starting)
                };
                let update = |task: &mut TaskView| match &data.delivery {
                    CollaborationDelivery::Ok {
                        agent_id,
                        session_id,
                        ..
                    } => {
                        task.kind = TaskKind::Member(if agent_id.contains(':') {
                            MemberTarget::Provider
                        } else {
                            MemberTarget::Session(session_id.clone())
                        });
                    }
                    CollaborationDelivery::Error { error } => {
                        task.finished = Some(TaskFinish {
                            at: event.create_at,
                            status: "Failed",
                            failed: true,
                            text: serde_json::to_value(error)
                                .expect("typed delivery error serializes")
                                .as_str()
                                .expect("typed delivery error is a string")
                                .to_owned(),
                            exit_code: None,
                        });
                    }
                };
                if let Some(task) = self.tasks.iter_mut().find(|task| starting(task)) {
                    update(task);
                }
                let delivered = match &data.delivery {
                    CollaborationDelivery::Ok { agent_id, .. } => Some((
                        self.agent_names.get(agent_id).unwrap_or(agent_id).clone(),
                        (!agent_id.contains(':')).then(|| agent_id.clone()),
                    )),
                    CollaborationDelivery::Error { .. } => None,
                };
                if let Some(card) = self.card(branch_id, &data.op_event_id)
                    && starting(&card.task)
                {
                    update(&mut card.task);
                    match delivered {
                        Some((name, agent_id)) if card.agent_id.is_some() => {
                            card.agent = name;
                            card.agent_id = agent_id;
                        }
                        Some(_) => {}
                        None => card.agent_id = None,
                    }
                }
            }
            EventContent::CollaborationResult(data) => {
                let (status, failed, text) = match &data.outcome {
                    CollaborationOutcome::Completed => ("Completed", false, data.text.clone()),
                    CollaborationOutcome::Failed { reason } => {
                        ("Failed", true, format!("{reason}\n{}", data.text))
                    }
                    CollaborationOutcome::Cancelled { reason } => {
                        ("Cancelled", false, format!("{reason}\n{}", data.text))
                    }
                };
                let finish = TaskFinish {
                    at: event.create_at,
                    status,
                    failed,
                    text,
                    exit_code: None,
                };
                if let Some(task) = self.tasks.iter_mut().find(|task| {
                    task.branch_id == branch_id
                        && task.id == data.request_event_id
                        && matches!(task.kind, TaskKind::Member(_))
                }) {
                    task.finished = Some(finish.clone());
                }
                if let Some(card) = self.card(branch_id, &data.request_event_id) {
                    card.task.finished = Some(finish);
                }
                if !self.owns(branch_id) {
                    self.retain_tasks();
                    return;
                }
                let (text, tone) = match &data.outcome {
                    CollaborationOutcome::Completed => {
                        (format!("agent result: {}", data.text), None)
                    }
                    CollaborationOutcome::Failed { reason } => {
                        (format!("agent failed: {reason}"), Some(RowTone::Danger))
                    }
                    CollaborationOutcome::Cancelled { reason } => {
                        (format!("agent cancelled: {reason}"), None)
                    }
                };
                let index = self.push(format!("agent-{sequence}"), RowKind::Status, text);
                self.row(index).tone = tone;
            }
            EventContent::ModelStart(data) => {
                self.request_starts
                    .insert(data.request_id.clone(), event.create_at);
            }
            EventContent::Thinking(data) => {
                let index = self.open_thinking(&data.request_id);
                let elapsed = self
                    .request_starts
                    .get(&data.request_id)
                    .map(|start| event.create_at.saturating_sub(*start));
                let thought = self
                    .thinking_rows
                    .get_mut(&data.request_id)
                    .expect("opened above");
                thought.written.push_str(&data.delta);
                let text = thought.written.clone();
                let row = self.row(index);
                row.text = text;
                row.elapsed_ms = elapsed;
            }
            EventContent::Text(data) => {
                self.close_thinking(&data.request_id);
                let index = match self.assistant_rows.get(&data.request_id) {
                    Some(index) => *index,
                    None => {
                        let index = self.push(
                            format!("assistant-{}", data.request_id),
                            RowKind::StreamingAssistant,
                            String::new(),
                        );
                        self.row(index).streaming = true;
                        self.assistant_rows.insert(data.request_id.clone(), index);
                        index
                    }
                };
                self.row(index).text.push_str(&data.delta);
            }
            EventContent::ToolUse(data) => {
                self.close_thinking(&data.request_id);
                self.close_assistant(&data.request_id);
                self.request_starts.remove(&data.request_id);
                let row_id = if self.rows.iter().any(|row| row.row_id == data.call.id) {
                    format!("{}-{}", data.call.id, self.rows.len())
                } else {
                    data.call.id.clone()
                };
                let index = self.push(row_id, RowKind::ToolCall, String::new());
                let row = self.row(index);
                row.tool_name = Some(data.call.name.clone());
                row.tool_input = Some(Value::Object(data.call.input.clone()));
                row.batch = Some(data.request_id.clone());
                row.pending = true;
                self.tool_rows.insert(data.call.id.clone(), index);
            }
            EventContent::ToolCompleted(data) => {
                self.complete_tool(&data.call_id, &data.result);
            }
            EventContent::ToolResult(data) => {
                for result in &data.results {
                    self.complete_tool(&result.call_id, &result.result);
                }
            }
            EventContent::CompactionStart(_) => {
                let index = self.push(
                    format!("compaction-{sequence}"),
                    RowKind::Compaction,
                    String::new(),
                );
                self.row(index).compaction = Some(CompactionView {
                    state: CompactionState::Running,
                    text: String::new(),
                    insufficient: Vec::new(),
                });
                self.compaction_rows.insert(sequence, index);
            }
            EventContent::Compaction(data) => {
                let index = match data
                    .start
                    .and_then(|start| self.compaction_rows.remove(&start))
                {
                    Some(index) => index,
                    None => self.push(
                        format!("compaction-{sequence}"),
                        RowKind::Compaction,
                        String::new(),
                    ),
                };
                self.row(index).compaction = Some(compaction_view(&data.outcome));
            }
            EventContent::ModelEnd(data) => {
                self.close_thinking(&data.request_id);
                self.close_assistant(&data.request_id);
                self.request_starts.remove(&data.request_id);
                if let ModelOutputOutcome::Failed {
                    error,
                    presentation,
                    budget,
                } = &data.outcome
                    && (budget.is_none() || presentation.is_some())
                {
                    let message = presentation
                        .as_ref()
                        .and_then(|value| value.server_message.as_deref())
                        .unwrap_or(error);
                    let action = presentation
                        .as_ref()
                        .and_then(|value| value.action.as_deref())
                        .map(|action| format!(". {action}"))
                        .unwrap_or_default();
                    self.push_error(format!("model failed: {message}{action}"));
                    if presentation.is_some() {
                        self.presented_failure = Some(error.clone());
                    }
                }
            }
            EventContent::TurnEnd(data) => {
                let open: Vec<String> = self.thinking_rows.keys().cloned().collect();
                for request_id in open {
                    self.close_thinking(&request_id);
                }
                self.request_starts.clear();
                self.turn_end(sequence, &data.outcome);
            }
            EventContent::InputConsumed(data) => {
                if let Some(index) = self.input_rows.remove(&data.input_id) {
                    self.row(index).pending = false;
                }
            }
            EventContent::Configuration(_)
            | EventContent::ToolCall(_)
            | EventContent::BranchStart(_)
            | EventContent::SessionForked(_)
            | EventContent::BranchCreateFailed(_)
            | EventContent::BranchEnded(_)
            | EventContent::PermissionChanged(_)
            | EventContent::InputWithdraw(_)
            | EventContent::TurnStart(_)
            | EventContent::ModelUsage(_)
            | EventContent::ToolUseStart(_)
            | EventContent::ToolStart(_)
            | EventContent::Other => {}
        }
        if matches!(
            &event.content,
            EventContent::ExecStarted(_)
                | EventContent::ExecEnded(_)
                | EventContent::CollaborationRequest(_)
                | EventContent::CollaborationDelivered(_)
                | EventContent::CollaborationResult(_)
        ) {
            self.retain_tasks();
        }
    }

    fn turn_end(&mut self, sequence: u64, outcome: &TurnOutcome) {
        let presented = self.presented_failure.take();
        let end = match outcome {
            TurnOutcome::Failed { error } => {
                if presented.as_deref() != Some(error.as_str()) {
                    self.push_error(format!("turn failed: {error}"));
                }
                TurnEnd::Failed
            }
            TurnOutcome::Cancelled { reason } => {
                if reason == crate::model::view::INTERRUPT_REASON {
                    let index = self.push(
                        format!("interrupted-{sequence}"),
                        RowKind::Status,
                        reason.clone(),
                    );
                    self.row(index).tone = Some(RowTone::Warning);
                }
                self.push(
                    format!("stopped-{sequence}"),
                    RowKind::Status,
                    "stopped".into(),
                );
                TurnEnd::Cancelled
            }
            TurnOutcome::Completed { .. } => TurnEnd::Completed,
        };
        self.ended = Some((sequence, end));
    }

    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// A user message as a transcript row: a shell run's record draws as one.
    /// It cuts just before `submitted`, where its input was written, even
    /// when it enters the transcript later (timeline UI §4.1), and a message
    /// stays pending until the model consumes it (§4.8).
    fn push_user(&mut self, input_id: &str, sequence: u64, submitted: u64, user: &UserInput) {
        let text = text_of(&user.content);
        let kind = if ShellRun::parse(&text).is_some() {
            RowKind::Shell
        } else {
            RowKind::UserMessage
        };
        let index = self.push(format!("user-{sequence}"), kind, text);
        let row = self.row(index);
        row.cut = Some(submitted.saturating_sub(1));
        row.display = user_display(&user.content);
        row.pending = kind == RowKind::UserMessage;
        self.input_rows.insert(input_id.to_owned(), index);
    }

    /// The content of a held message, to re-submit it as Steer.
    pub fn held_input(&self, input_id: &str) -> Option<&UserInput> {
        self.held
            .iter()
            .find(|(id, _, _)| id == input_id)
            .map(|(_, _, user)| user)
    }

    pub fn add_approval(
        &mut self,
        branch_id: String,
        request_id: String,
        name: String,
        input: JsonObject,
        reason: String,
        always: Option<String>,
    ) {
        if self
            .approvals
            .iter()
            .any(|approval| approval.branch_id == branch_id && approval.request_id == request_id)
        {
            return;
        }
        if let Some(index) = self.tool_rows.get(&request_id).copied() {
            self.row(index).parked = true;
        }
        self.approvals.push(ApprovalView {
            member: None,
            request_id,
            branch_id,
            reason,
            tool_name: name,
            tool_input: Value::Object(input),
            always,
        });
    }

    /// The command an "always" answer to `request_id` saves a rule for.
    pub fn approval_always(&self, branch_id: &str, request_id: &str) -> Option<String> {
        self.approvals
            .iter()
            .find(|approval| approval.branch_id == branch_id && approval.request_id == request_id)?
            .always
            .clone()
    }

    pub fn member_approval(
        &mut self,
        branch_id: &str,
        request_id: &str,
        detail: Option<
            Result<dim_agent_types::MemberApprovalDescriptor, crate::member_approval::DetailError>,
        >,
    ) {
        if let Some(approval) = self
            .approvals
            .iter_mut()
            .find(|approval| approval.branch_id == branch_id && approval.request_id == request_id)
        {
            approval.member = detail;
        }
    }

    pub fn remove_approval(&mut self, branch_id: &str, request_id: &str) {
        self.approvals.retain(|approval| {
            approval.branch_id != branch_id || approval.request_id != request_id
        });
    }

    pub fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            rows: self.rows.clone(),
            running: self.running,
            approvals: self.approvals.clone(),
            questions: self.questions.calls().cloned().collect(),
            queued: if self.running {
                self.held
                    .iter()
                    .map(|(id, _, user)| QueuedInput {
                        id: id.clone(),
                        text: text_of(&user.content),
                        display: user_display(&user.content),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            tasks: self.tasks.clone(),
            ended: self.ended,
            goal: self.goal.goal(),
            ..ViewSnapshot::default()
        }
    }

    /// The goal in force on this branch, folded from its events so far.
    pub fn goal(&self) -> Option<crate::goal::Goal> {
        self.goal.goal()
    }

    pub fn tasks(&self) -> &[TaskView] {
        &self.tasks
    }

    /// The first message the user sent: the window title's topic until the
    /// Session has a stored title (`ui::title`).
    pub fn first_message(&self) -> Option<String> {
        self.rows
            .iter()
            .find(|row| row.kind == RowKind::UserMessage)
            .map(|row| row.text.clone())
    }

    /// Whether this transcript's turn runs.
    pub fn running(&self) -> bool {
        self.running
    }

    pub fn rows(&self) -> &[Arc<ViewRow>] {
        &self.rows
    }

    /// The lines `main` recorded, without `main` itself (design §14).
    pub fn line_views(&self) -> &[LineView] {
        &self.lines
    }

    pub fn approvals(&self) -> Vec<ApprovalView> {
        self.approvals.clone()
    }

    pub fn questions(&self) -> Vec<crate::question::Call> {
        self.questions.calls().cloned().collect()
    }

    pub fn workflow(&self, run_id: &str) -> Option<&WorkflowView> {
        self.tasks.iter().find_map(|task| match &task.kind {
            TaskKind::Workflow(view) if task.id == run_id => Some(&**view),
            _ => None,
        })
    }

    fn workflow_mut(&mut self, run_id: &str) -> Option<&mut WorkflowView> {
        self.tasks.iter_mut().find_map(|task| match &mut task.kind {
            TaskKind::Workflow(view) if task.id == run_id => Some(&mut **view),
            _ => None,
        })
    }

    /// Adopts a stored run of this Session from `branch_id`.
    pub fn set_workflow(&mut self, branch_id: &str, run: WorkflowRun) {
        let finished = (!run.state.unfinished()).then(|| {
            let (status, failed) = match run.state {
                WorkflowState::Completed => ("Completed", false),
                WorkflowState::Failed => ("Failed", true),
                WorkflowState::Stopped => ("Stopped", false),
                WorkflowState::Interrupted => ("Interrupted", true),
                WorkflowState::Running | WorkflowState::Stopping => unreachable!("terminal"),
            };
            TaskFinish {
                at: run.ended_at.unwrap_or(run.created_at),
                status,
                failed,
                text: String::new(),
                exit_code: None,
            }
        });
        let task = TaskView {
            id: run.run_id.clone(),
            branch_id: branch_id.to_owned(),
            label: run.name.clone(),
            started_at: run.created_at,
            finished,
            kind: TaskKind::Workflow(Box::new(WorkflowView {
                stop_pending: self
                    .workflow(&run.run_id)
                    .is_some_and(|view| view.stop_pending),
                unavailable: false,
                run,
            })),
        };
        match self
            .tasks
            .iter_mut()
            .find(|known| matches!(known.kind, TaskKind::Workflow(_)) && known.id == task.id)
        {
            Some(known) => *known = task,
            None => self.tasks.push(task),
        }
        self.retain_tasks();
    }

    /// A complete discovery: runs it no longer returns leave the list.
    pub fn reconcile_workflows(&mut self, runs: Vec<(String, WorkflowRun)>) {
        self.tasks.retain(|task| {
            !matches!(task.kind, TaskKind::Workflow(_))
                || runs.iter().any(|(_, run)| run.run_id == task.id)
        });
        for (branch_id, run) in runs {
            self.set_workflow(&branch_id, run);
        }
    }

    pub fn workflow_unavailable(&mut self, run_id: &str) {
        if let Some(view) = self.workflow_mut(run_id) {
            view.unavailable = true;
        }
    }

    pub fn workflow_stop_pending(&mut self, run_id: &str, pending: bool) {
        if let Some(view) = self.workflow_mut(run_id) {
            view.stop_pending = pending;
        }
    }

    fn retain_tasks(&mut self) {
        self.tasks.sort_by(|a, b| match (&a.finished, &b.finished) {
            (None, None) => a
                .started_at
                .cmp(&b.started_at)
                .then_with(|| a.branch_id.cmp(&b.branch_id))
                .then_with(|| a.id.cmp(&b.id)),
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a_end), Some(b_end)) => b_end
                .at
                .cmp(&a_end.at)
                .then_with(|| a.branch_id.cmp(&b.branch_id))
                .then_with(|| a.id.cmp(&b.id)),
        });
        let unfinished = self
            .tasks
            .iter()
            .take_while(|task| task.finished.is_none())
            .count();
        self.tasks.truncate(unfinished + 50);
    }

    /// A main-branch delegation's card.
    fn card(&mut self, branch_id: &str, request_event_id: &str) -> Option<&mut MemberCard> {
        if branch_id != "main" {
            return None;
        }
        let index = *self.cards.get(request_event_id)?;
        self.row(index).member.as_deref_mut()
    }

    /// Native Sessions with an unfinished delegation: the members whose
    /// cards this client keeps observed.
    pub fn unfinished_members(&self) -> impl Iterator<Item = &str> {
        self.tasks
            .iter()
            .filter_map(|task| match (&task.kind, &task.finished) {
                (TaskKind::Member(MemberTarget::Session(id)), None) => Some(id.as_str()),
                _ => None,
            })
    }

    /// What this client now observes of a member: its unfinished cards show
    /// it; ended cards keep what they last had.
    pub fn observe_member(
        &mut self,
        session_id: &str,
        model: Option<&str>,
        activity: Option<&str>,
        observation: &MemberConversationState,
    ) {
        let indexes: Vec<usize> = self.cards.values().copied().collect();
        for index in indexes {
            let Some(card) = &self.rows[index].member else {
                continue;
            };
            let observed = card.task.finished.is_none()
                && card.task.kind == TaskKind::Member(MemberTarget::Session(session_id.to_owned()));
            let model = model.map(str::to_owned).or_else(|| card.model.clone());
            if !observed
                || (card.model == model
                    && card.activity.as_deref() == activity
                    && card.observation.as_ref() == Some(observation))
            {
                continue;
            }
            let card = self.row(index).member.as_deref_mut().expect("card row");
            card.model = model;
            card.activity = activity.map(str::to_owned);
            card.observation = Some(observation.clone());
        }
    }

    /// Names native Agents on their cards, from one shared `agent.list`
    /// that was asked to name `asked`.
    pub fn name_agents(&mut self, names: HashMap<String, String>, asked: Vec<String>) {
        self.agents_read.extend(asked);
        let indexes: Vec<usize> = self.cards.values().copied().collect();
        for index in indexes {
            let Some(name) = self.rows[index]
                .member
                .as_ref()
                .and_then(|card| names.get(card.agent_id.as_ref()?))
                .filter(|name| !name.is_empty())
                .cloned()
            else {
                continue;
            };
            self.row(index)
                .member
                .as_deref_mut()
                .expect("card row")
                .agent = name;
        }
        self.agent_names = names;
    }

    /// Native Agents the cards name that no `agent.list` read covered yet.
    pub fn unnamed_agents(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .cards
            .values()
            .filter_map(|index| self.rows[*index].member.as_ref()?.agent_id.clone())
            .filter(|id| !self.agent_names.contains_key(id) && !self.agents_read.contains(id))
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// The member's latest activity as one line: what it runs or says now.
    pub fn activity(&self) -> Option<String> {
        let row = self.rows.iter().rev().find(|row| {
            matches!(
                row.kind,
                RowKind::ToolCall
                    | RowKind::Thinking
                    | RowKind::StreamingAssistant
                    | RowKind::AssistantMessage
            )
        })?;
        let line = match row.kind {
            RowKind::ToolCall => {
                let name = row.tool_name.as_deref().unwrap_or("tool");
                let detail = row
                    .tool_input
                    .as_ref()
                    .and_then(Value::as_object)
                    .and_then(|input| input.values().find_map(Value::as_str))
                    .and_then(|text| text.lines().next())
                    .unwrap_or("");
                format!("{name} {detail}")
            }
            RowKind::Thinking => "thinking".to_owned(),
            _ => row
                .text
                .lines()
                .rfind(|line| !line.trim().is_empty())?
                .to_owned(),
        };
        Some(line.trim().to_owned())
    }

    fn row(&mut self, index: usize) -> &mut ViewRow {
        Arc::make_mut(&mut self.rows[index])
    }

    fn push(&mut self, row_id: String, kind: RowKind, text: String) -> usize {
        self.rows.push(Arc::new(ViewRow {
            row_id,
            kind,
            text,
            display: None,
            streaming: false,
            elapsed_ms: None,
            tone: None,
            tool_name: None,
            tool_input: None,
            batch: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
            exit_code: None,
            member: None,
            cut: None,
            line: None,
            compaction: None,
        }));
        self.rows.len() - 1
    }

    fn push_error(&mut self, text: String) {
        let index = self.push(format!("error-{}", self.rows.len()), RowKind::Error, text);
        self.row(index).tone = Some(RowTone::Danger);
    }

    /// Shows a model request's reasoning that is not written yet. Progress
    /// that the written deltas overtook, or that runs ahead of them, waits
    /// for the next notice.
    pub fn progress(&mut self, progress: &Progress) {
        let Some(request_id) = &progress.request_id else {
            return;
        };
        if progress.thinking.is_empty() {
            return;
        }
        let index = self.open_thinking(request_id);
        let thought = &self.thinking_rows[request_id];
        if thought.written.chars().count() as u64 != progress.thinking_offset {
            return;
        }
        let text = format!("{}{}", thought.written, progress.thinking);
        self.row(index).text = text;
    }

    /// The open reasoning row of a request, pushed on its first output.
    fn open_thinking(&mut self, request_id: &str) -> usize {
        if let Some(thought) = self.thinking_rows.get(request_id) {
            return thought.index;
        }
        let index = self.push(
            format!("thinking-{request_id}-{}", self.rows.len()),
            RowKind::Thinking,
            String::new(),
        );
        self.row(index).streaming = true;
        self.thinking_rows.insert(
            request_id.to_owned(),
            OpenThought {
                index,
                written: String::new(),
            },
        );
        index
    }

    /// A thought settles when the request moves on, as the Timeline holds
    /// it: progress that was never written (an interrupted thought) goes.
    fn close_thinking(&mut self, request_id: &str) {
        if let Some(thought) = self.thinking_rows.remove(request_id) {
            let row = self.row(thought.index);
            row.streaming = false;
            row.text = thought.written;
        }
    }

    fn close_assistant(&mut self, request_id: &str) {
        if let Some(index) = self.assistant_rows.remove(request_id) {
            self.row(index).streaming = false;
            self.row(index).kind = RowKind::AssistantMessage;
        }
    }

    fn complete_tool(&mut self, call_id: &str, result: &ToolResult) {
        let Some(index) = self.tool_rows.remove(call_id) else {
            return;
        };
        let row = self.row(index);
        row.pending = false;
        row.parked = false;
        row.result_is_error = result.is_error == Some(true);
        row.result_text = Some(result_text(result));
        let structured = |key: &str| result.structured_content.as_ref()?.get(key);
        row.diff = structured("diff")
            .and_then(Value::as_str)
            .map(str::to_owned);
        row.exit_code = structured("exitCode").and_then(Value::as_i64);
    }
}

/// A compaction result as its row shows it: a summary and insufficient-space
/// diagnostics when completed, or the recorded reason otherwise.
fn compaction_view(outcome: &ContextCompactionResult) -> CompactionView {
    let (state, text, insufficient) = match outcome {
        ContextCompactionResult::Completed {
            replacement,
            diagnostics,
            replacement_parts,
            ..
        } => {
            let text = replacement_text(replacement, *replacement_parts);
            let insufficient = diagnostics
                .insufficient_space
                .as_ref()
                .map(|space| {
                    space
                        .parts
                        .iter()
                        .map(|part| CompactionPartSize {
                            part: compaction_part(part.part),
                            tokens: part.tokens,
                        })
                        .collect()
                })
                .unwrap_or_default();
            (CompactionState::Completed, text, insufficient)
        }
        ContextCompactionResult::Skipped { reason, .. } => {
            (CompactionState::Skipped, reason.clone(), Vec::new())
        }
        ContextCompactionResult::Failed { error, .. } => {
            (CompactionState::Failed, error.clone(), Vec::new())
        }
        ContextCompactionResult::Cancelled { reason } => {
            (CompactionState::Cancelled, reason.clone(), Vec::new())
        }
    };
    CompactionView {
        state,
        text,
        insufficient,
    }
}

/// A part of the insufficient-space mark, as this client names it.
fn compaction_part(part: InsufficientSpacePartName) -> CompactionPart {
    match part {
        InsufficientSpacePartName::SystemPrompt => CompactionPart::SystemPrompt,
        InsufficientSpacePartName::Tools => CompactionPart::Tools,
        InsufficientSpacePartName::AppendedMessages => CompactionPart::AppendedMessages,
        InsufficientSpacePartName::KeyedBaseline => CompactionPart::KeyedBaseline,
        InsufficientSpacePartName::PinnedInputs => CompactionPart::PinnedInputs,
        InsufficientSpacePartName::Digest => CompactionPart::Digest,
        InsufficientSpacePartName::Summary => CompactionPart::Summary,
    }
}

/// The strategy's messages supply the summary. Without `replacement_parts`,
/// the whole replacement is the strategy's part.
fn replacement_text(replacement: &[Message], parts: Option<ReplacementParts>) -> String {
    let length = replacement.len();
    let strategy = parts.map_or(length, |parts| {
        usize::try_from(parts.strategy)
            .unwrap_or(usize::MAX)
            .min(length)
    });
    messages_text(&replacement[..strategy])
}

/// The text of a run of messages, one per line.
fn messages_text(messages: &[Message]) -> String {
    let mut texts: Vec<&str> = Vec::new();
    for message in messages {
        match &message.content {
            MessageContent::Text(text) => texts.push(text),
            MessageContent::Parts(parts) => texts.extend(parts.iter().filter_map(|part| {
                let MessagePart::Text { text, .. } = part else {
                    return None;
                };
                Some(text.as_str())
            })),
        }
    }
    texts.join("\n")
}

pub(crate) fn result_text(result: &ToolResult) -> String {
    match &result.content {
        ToolContent::Text(text) => text.clone(),
        ToolContent::Blocks(blocks) => text_of(blocks),
    }
}

fn branch_notice(text: &str, mode: Option<InputMode>) -> Option<String> {
    if let Some(rest) = text.strip_prefix("<dim-branch-message") {
        let from = attr(rest, "from")?;
        let steer = match mode {
            Some(InputMode::Steer) => " (steer)",
            _ => "",
        };
        let ending = match attr(rest, "final") {
            Some(_) => ", final",
            None => "",
        };
        let body = unescape(&one_line(&element_body(text, "</dim-branch-message>")));
        return Some(format!("◂ from {from}{steer}{ending}  {body}"));
    }
    if let Some(rest) = text.strip_prefix("<dim-branch-idle") {
        let branch = attr(rest, "branch")?;
        let status = attr(rest, "status").unwrap_or("idle");
        return Some(format!("◂ {branch} is waiting ({status})"));
    }
    let rest = text.strip_prefix("<dim-branch-ended")?;
    let branch = attr(rest, "branch")?;
    Some(format!("◂ {branch} ended"))
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The value of `name="value"` in an element's attribute text.
fn attr<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start = text.find(&needle)? + needle.len();
    let end = text[start..].find('"')? + start;
    Some(&text[start..end])
}

/// The element's body between its first line and `close`.
fn element_body(text: &str, close: &str) -> String {
    let body = text.split_once('\n').map_or("", |(_, body)| body);
    body.strip_suffix(close).unwrap_or(body).trim().to_owned()
}

/// A body on one transcript line: its whitespace collapsed.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
/// The marker a loaded Skill block starts with (Skills §9).
const SKILL_BLOCK: &str = "<dim-skill name=\"";

fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .enumerate()
        .filter(|(index, block)| loaded_skill(*index, block).is_none())
        .filter_map(|(_, block)| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Skill a loaded block names: a text block after the first that starts
/// with the marker carries a Skill's instructions, which are shown as one
/// line instead.
fn loaded_skill(index: usize, block: &ContentBlock) -> Option<String> {
    if index == 0 {
        return None;
    }
    let ContentBlock::Text { text } = block else {
        return None;
    };
    let rest = text.strip_prefix(SKILL_BLOCK)?;
    let name = rest.split('"').next()?;
    (!name.is_empty()).then(|| {
        name.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
    })
}

/// The presentation of an operator message: its text with an image marker per
/// image and one line per loaded Skill; `None` when its text is the message.
fn user_display(blocks: &[ContentBlock]) -> Option<String> {
    let mut images = 0;
    let mut skills = Vec::new();
    let mut lines = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Text { text } => match loaded_skill(index, block) {
                Some(name) => skills.push(name),
                None => lines.push(text.clone()),
            },
            ContentBlock::Image { .. } => {
                images += 1;
                lines.push(crate::images::marker(images, ""));
            }
        }
    }
    if images == 0 && skills.is_empty() {
        return None;
    }
    let mut display = lines.join("\n");
    for skill in skills {
        let line = crate::ui::i18n::tf("Skill loaded: {name}", &[("name", &skill)]);
        if !display.is_empty() {
            display.push('\n');
        }
        display.push_str(&line);
    }
    Some(display)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(projection: &mut Projection, events: &[serde_json::Value]) {
        for event in events {
            let event: Event = serde_json::from_value(event.clone()).unwrap();
            projection.apply("main", &event);
        }
    }

    fn step(sequence: u64, name: &str, delta: &str) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "step",
            "name": name,
            "data": { "turn_id": "t1", "request_id": "r1", "delta": delta },
        })
    }

    /// The strategy-neutral diagnostics every compaction record carries.
    fn diagnostics() -> serde_json::Value {
        serde_json::json!({
            "reason": "auto",
            "trigger": "threshold",
            "estimatedInputTokens": 300.0,
            "entriesReplaced": 3,
            "modelCallCount": 1,
            "durationMs": 10,
            "strategy": {"kind": "summary"},
        })
    }

    fn compaction_start(sequence: u64) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "context",
            "name": "compaction-start",
            "data": {"through": 2, "trigger": "threshold", "strategy": "summary"},
        })
    }

    fn compaction(
        sequence: u64,
        start: Option<u64>,
        outcome: serde_json::Value,
    ) -> serde_json::Value {
        let mut data = serde_json::json!({"through": 2});
        if let Some(start) = start {
            data["start"] = serde_json::json!(start);
        }
        let serde_json::Value::Object(outcome) = outcome else {
            panic!("an outcome is an object")
        };
        for (key, value) in outcome {
            data[key] = value;
        }
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "context",
            "name": "compaction",
            "data": data,
        })
    }

    fn model_start(sequence: u64) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "step",
            "name": "model-start",
            "data": { "turn_id": "t1", "request_id": "r1" },
        })
    }

    fn progress(thinking: &str, offset: u64) -> Progress {
        serde_json::from_value(serde_json::json!({
            "sessionId": "s", "branchId": "main", "requestId": "r1",
            "text": "", "textOffset": 0,
            "thinking": thinking, "thinkingOffset": offset,
        }))
        .unwrap()
    }

    #[test]
    fn reasoning_streams_from_progress_and_settles_as_written() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                serde_json::json!({ "id": "in-1", "sequence": 1, "create_at": 1000, "type": "input", "name": "message", "data": {} }),
                model_start(2),
            ],
        );
        projection.progress(&progress("Look at the\nparser", 0));
        let rows = projection.snapshot().rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, RowKind::Thinking);
        assert!(rows[0].streaming);
        assert_eq!(rows[0].text, "Look at the\nparser");

        apply(
            &mut projection,
            &[step(5, "thinking", "Look at the\nparser first")],
        );
        projection.progress(&progress("Look at the\nparser", 0));
        let rows = projection.snapshot().rows;
        assert_eq!(rows[0].text, "Look at the\nparser first");
        assert_eq!(rows[0].elapsed_ms, Some(3000));

        apply(&mut projection, &[step(6, "text", "Done.")]);
        let rows = projection.snapshot().rows;
        assert!(!rows[0].streaming);
        assert_eq!(rows[1].kind, RowKind::StreamingAssistant);
    }

    #[test]
    fn an_interrupted_thought_keeps_only_what_was_written() {
        let mut projection = Projection::default();
        apply(&mut projection, &[model_start(1)]);
        projection.progress(&progress("half a thought", 0));
        apply(
            &mut projection,
            &[serde_json::json!({
                "sequence": 2, "create_at": 2000, "type": "turn", "name": "end",
                "data": { "turn_id": "t1", "status": "cancelled", "reason": "user" },
            })],
        );
        let rows = projection.snapshot().rows;
        assert!(!rows[0].streaming);
        assert_eq!(rows[0].text, "");
    }

    /// A raw-block input, as a line's notice or a message arrives.
    fn raw_input(sequence: u64, text: &str, mode: &str) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "input",
            "name": "message",
            "data": {"blocks": [{"type": "raw", "text": text}], "mode": mode},
        })
    }

    fn line_event(sequence: u64, name: &str, data: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "branch",
            "name": name,
            "data": data,
        })
    }

    #[test]
    fn a_message_is_received_by_the_line_it_names_and_shown_once() {
        let mut line = Projection::for_branch("b-7c");
        line.apply(
            "b-7c",
            &serde_json::from_value(raw_input(
                4,
                "<dim-branch-message from=\"main\" to=\"b-7c\">\nStop on parser.rs.\n</dim-branch-message>",
                "steer",
            ))
            .unwrap(),
        );
        let rows = line.snapshot().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, RowKind::Status);
        assert_eq!(rows[0].text, "◂ from main (steer)  Stop on parser.rs.");
        let mut other = Projection::for_branch("b-91");
        other.apply(
            "b-91",
            &serde_json::from_value(raw_input(4, "anything", "queue")).unwrap(),
        );
        assert!(other.snapshot().rows.is_empty());
        let mut main = Projection::default();
        apply(
            &mut main,
            &[raw_input(
                5,
                "<dim-branch-ended branch=\"b-02\">\nLine b-02 was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did.\n</dim-branch-ended>",
                "steer",
            )],
        );
        apply(
            &mut main,
            &[raw_input(
                6,
                "<dim-branch-idle branch=\"b-7c\" status=\"completed\">\nLine b-7c's turn ended (completed) without a message to you, and b-7c is waiting. Read it if you need its state; message it with mode \"steer\" to give it more work, or leave it.\n</dim-branch-idle>",
                "steer",
            )],
        );
        let rows = main.snapshot().rows;
        assert_eq!(rows[0].text, "◂ b-02 ended");
        assert_eq!(rows[1].text, "◂ b-7c is waiting (completed)");
        let mut escaped = Projection::default();
        apply(
            &mut escaped,
            &[raw_input(
                7,
                "<dim-branch-message from=\"b-7c\" to=\"main\">\na &amp;&amp; b &lt;Vec&lt;u8&gt;&gt;\n</dim-branch-message>",
                "steer",
            )],
        );
        let rows = escaped.snapshot().rows;
        assert_eq!(rows[0].text, "◂ from b-7c (steer)  a && b <Vec<u8>>");
    }

    #[test]
    fn the_line_list_follows_creation_and_end() {
        let mut main = Projection::default();
        let create = |sequence, branch: &str, prompt: &str| {
            line_event(
                sequence,
                "create",
                serde_json::json!({
                    "at": {"kind": "sequence", "sequence": sequence - 1},
                    "source": {"kind": "tool", "turn_id": "t1", "request_id": "r1", "call_id": "c1"},
                    "branch": branch,
                    "prompt": prompt,
                }),
            )
        };
        let said = |sequence: u64, id: &str, text: &str| {
            serde_json::json!({
                "id": id, "sequence": sequence, "create_at": sequence * 1000, "type": "input", "name": "message",
                "data": {"user": {"content": [{"type": "text", "text": text}]}},
            })
        };
        let applied = |sequence: u64, id: &str| {
            serde_json::json!({
                "id": format!("ap-{sequence}"), "sequence": sequence, "create_at": sequence * 1000,
                "type": "input", "name": "applied", "data": {"input_id": id},
            })
        };
        apply(
            &mut main,
            &[
                said(1, "in-1", "find the leak"),
                said(2, "in-2", "try the parser"),
                applied(3, "in-1"),
                applied(4, "in-2"),
                create(6, "b-7c", "parser race hypothesis\nrest"),
            ],
        );
        assert_eq!(
            main.line_views(),
            &[LineView {
                id: "b-7c".into(),
                parent: Some("main".into()),
                purpose: "parser race hypothesis".into(),
                state: LineState::Waiting,
                cut: Some(LineCut {
                    sequence: 5,
                    messages: 2,
                }),
            }]
        );
        let rows = main.snapshot().rows;
        assert_eq!(
            rows.iter().map(|row| row.cut).collect::<Vec<_>>(),
            [Some(0), Some(1), None],
            "a message cuts just before itself"
        );
        assert_eq!(rows[2].text, "parser race hypothesis · b-7c");
        let mut picked = Projection::default();
        apply(
            &mut picked,
            &[
                said(1, "in-3", "find the leak"),
                said(2, "in-4", "try the parser"),
                applied(3, "in-3"),
                applied(4, "in-4"),
                line_event(
                    5,
                    "create",
                    serde_json::json!({
                        "at": {"kind": "sequence", "sequence": 1},
                        "source": {"kind": "host", "operation_id": "op-0"},
                        "branch": "70560552-954d-4dd2",
                        "prompt": "try the cache",
                    }),
                ),
            ],
        );
        assert_eq!(
            picked.snapshot().rows[2].text,
            "try the cache · before “try the parser” · 70560552",
            "a line cut before a message names it, wherever its marker lands"
        );
        assert_eq!(
            rows[2].line.as_deref(),
            Some("b-7c"),
            "the marker enters the line it opened"
        );
        apply(
            &mut main,
            &[line_event(
                8,
                "ended",
                serde_json::json!({
                    "branch": "b-7c",
                    "endEvent": 6,
                    "source": {"kind": "tool", "turn_id": "t1", "request_id": "r1", "call_id": "c2"},
                }),
            )],
        );
        assert_eq!(main.line_views()[0].state, LineState::EndedItself);
        apply(&mut main, &[create(9, "b-91", "review")]);
        apply(
            &mut main,
            &[line_event(
                12,
                "ended",
                serde_json::json!({
                    "branch": "b-91",
                    "endEvent": 11,
                    "source": {"kind": "host", "operation_id": "op-1"},
                }),
            )],
        );
        assert_eq!(main.line_views()[1].state, LineState::EndedByOperation);
        main.apply(
            "b-7c",
            &serde_json::from_value(said(10, "in-5", "check it")).unwrap(),
        );
        let nested: Event =
            serde_json::from_value(create(13, "b-90", "review the change")).unwrap();
        main.apply("b-7c", &nested);
        assert_eq!(
            main.line_views().last(),
            Some(&LineView {
                id: "b-90".into(),
                parent: Some("b-7c".into()),
                purpose: "review the change".into(),
                state: LineState::Waiting,
                cut: Some(LineCut {
                    sequence: 12,
                    messages: 3,
                }),
            }),
            "a line a line opened is still a line of the Session"
        );
        let nested_end: Event = serde_json::from_value(line_event(
            14,
            "ended",
            serde_json::json!({
                "branch": "b-90",
                "endEvent": 13,
                "source": {"kind": "host", "operation_id": "op-2"},
            }),
        ))
        .unwrap();
        main.apply("b-7c", &nested_end);
        assert_eq!(
            main.line_views().last().map(|line| &line.state),
            Some(&LineState::EndedByOperation)
        );
        let cascaded: Event = serde_json::from_value(line_event(
            15,
            "ended",
            serde_json::json!({
                "branch": "b-90",
                "endEvent": 14,
                "source": {"kind": "tool", "turn_id": "t1", "request_id": "r1", "call_id": "c1"},
                "ancestor": "b-7c",
            }),
        ))
        .unwrap();
        main.apply("b-7c", &cascaded);
        assert_eq!(
            main.line_views().last().map(|line| &line.state),
            Some(&LineState::EndedWith("b-7c".into())),
            "a tool source does not make a cascade an end of its own"
        );
        let mut done = Projection::for_branch("b-90");
        let end: Event = serde_json::from_value(line_event(
            16,
            "end",
            serde_json::json!({
                "source": {"kind": "tool", "turn_id": "t1", "request_id": "r1", "call_id": "c1"},
                "ancestor": "b-7c",
            }),
        ))
        .unwrap();
        done.apply("b-90", &end);
        assert!(
            done.snapshot()
                .rows
                .iter()
                .any(|row| row.text == "This line ended (with b-7c)"),
            "a cascaded line's own transcript names its ancestor"
        );
    }

    #[test]
    fn image_messages_remain_visible_in_history_queue_and_transcript() {
        let image = serde_json::json!({
            "type": "image",
            "source": {"type": "url", "mediaType": "image/png", "url": "https://invalid.example/image.png"},
        });
        let input = |sequence, id, content| {
            serde_json::json!({
                "id": id, "sequence": sequence, "create_at": sequence,
                "type": "input", "name": "message",
                "data": {"user": {"content": content}, "mode": "queue"},
            })
        };
        let history = input(1, "history-image", serde_json::json!([image]));
        let mixed = input(
            4,
            "mixed-images",
            serde_json::json!([
                {"type": "text", "text": "Compare"}, image,
                {"type": "text", "text": "with"}, image,
            ]),
        );
        let applied = serde_json::json!({
            "id": "applied", "sequence": 5, "create_at": 5,
            "type": "input", "name": "applied", "data": {"input_id": "mixed-images"},
        });
        let history_applied = serde_json::json!({
            "id": "history-applied", "sequence": 2, "create_at": 2,
            "type": "input", "name": "applied", "data": {"input_id": "history-image"},
        });
        let history_consumed = serde_json::json!({
            "id": "history-consumed", "sequence": 3, "create_at": 3,
            "type": "input", "name": "consumed", "data": {"input_id": "history-image"},
        });
        let mut live = Projection::default();
        apply(
            &mut live,
            &[
                history.clone(),
                history_applied.clone(),
                history_consumed.clone(),
            ],
        );
        assert!(live.snapshot().rows[0].text.is_empty());
        assert_eq!(
            live.snapshot().rows[0].display.as_deref(),
            Some("[Image #1]")
        );
        live.set_running(true);
        apply(&mut live, std::slice::from_ref(&mixed));
        let expected = "Compare\n[Image #1]\nwith\n[Image #2]";
        assert_eq!(live.snapshot().queued[0].text, "Compare\nwith");
        assert_eq!(live.snapshot().queued[0].display.as_deref(), Some(expected));
        assert_eq!(
            serde_json::to_value(live.held_input("mixed-images").unwrap()).unwrap(),
            mixed["data"]["user"],
            "display markers preserve the original queued image blocks"
        );
        apply(&mut live, std::slice::from_ref(&applied));
        assert!(live.snapshot().queued.is_empty());
        assert_eq!(live.snapshot().rows[1].text, "Compare\nwith");
        assert_eq!(live.snapshot().rows[1].display.as_deref(), Some(expected));
        let previous = crate::ui::i18n::active();
        crate::ui::i18n::set(crate::ui::i18n::Locale::ZhCn);
        let mut replay = Projection::default();
        apply(
            &mut replay,
            &[history, history_applied, history_consumed, mixed, applied],
        );
        assert!(replay.snapshot().rows[0].text.is_empty());
        assert_eq!(
            replay.snapshot().rows[0].display.as_deref(),
            Some("[Image #1]")
        );
        assert_eq!(replay.snapshot().rows[1].text, "Compare\nwith");
        assert_eq!(replay.snapshot().rows[1].display.as_deref(), Some(expected));
        crate::ui::i18n::set(previous);
    }

    /// A loaded Skill block (Skills §9) is one line under the operator's
    /// message, never its instructions; the editable text stays the
    /// operator's own words.
    #[test]
    fn a_loaded_skill_shows_as_one_line_under_the_message() {
        let previous = crate::ui::i18n::active();
        crate::ui::i18n::set(crate::ui::i18n::Locale::En);
        let image = serde_json::json!({
            "type": "image",
            "source": {"type": "url", "mediaType": "image/png", "url": "https://invalid.example/image.png"},
        });
        let block = |name: &str| {
            serde_json::json!({
                "type": "text",
                "text": format!(
                    "<dim-skill name=\"{name}\">\nSkill: {name}\nDescription: D\nFile: /tmp/{name}/SKILL.md\n\nInstructions:\nBODY-{name}\n</dim-skill>"
                ),
            })
        };
        let input = |sequence: u64, id: &str, content: serde_json::Value| {
            serde_json::json!({
                "id": id, "sequence": sequence, "create_at": sequence,
                "type": "input", "name": "message",
                "data": {"user": {"content": content}, "mode": "queue"},
            })
        };
        let applied = |sequence: u64, id: &str| {
            serde_json::json!({
                "id": format!("applied-{id}"), "sequence": sequence, "create_at": sequence,
                "type": "input", "name": "applied", "data": {"input_id": id},
            })
        };
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                input(
                    1,
                    "one",
                    serde_json::json!([
                        {"type": "text", "text": "one $skill: checkin please"},
                        block("checkin"),
                    ]),
                ),
                applied(2, "one"),
                input(
                    3,
                    "two",
                    serde_json::json!([
                        {"type": "text", "text": "two"},
                        block("alpha"),
                        block("beta"),
                    ]),
                ),
                applied(4, "two"),
                input(
                    5,
                    "typed",
                    serde_json::json!([
                        {"type": "text", "text": "<dim-skill name=\"mine\">as typed"},
                    ]),
                ),
                applied(6, "typed"),
                input(
                    7,
                    "images",
                    serde_json::json!([
                        {"type": "text", "text": "look"},
                        image,
                        block("checkin"),
                    ]),
                ),
                applied(8, "images"),
            ],
        );
        let rows = projection.snapshot().rows;
        assert_eq!(rows[0].text, "one $skill: checkin please");
        assert_eq!(
            rows[0].display.as_deref(),
            Some("one $skill: checkin please\nSkill loaded: checkin")
        );
        assert_eq!(rows[1].text, "two");
        assert_eq!(
            rows[1].display.as_deref(),
            Some("two\nSkill loaded: alpha\nSkill loaded: beta")
        );
        assert_eq!(
            rows[2].text, "<dim-skill name=\"mine\">as typed",
            "a first block is the operator's own text, shown as typed"
        );
        assert_eq!(rows[2].display, None);
        assert_eq!(rows[3].text, "look");
        assert_eq!(
            rows[3].display.as_deref(),
            Some("look\n[Image #1]\nSkill loaded: checkin")
        );
        crate::ui::i18n::set(previous);
    }

    #[test]
    fn a_queued_message_cuts_at_its_submission_and_a_withdrawn_one_is_not_counted() {
        let input = |sequence: u64, id: &str, text: &str, mode: &str| {
            serde_json::json!({
                "id": id, "sequence": sequence, "create_at": sequence * 1000,
                "type": "input", "name": "message",
                "data": {"user": {"content": [{"type": "text", "text": text}]}, "mode": mode},
            })
        };
        let control = |sequence: u64, name: &str, id: &str| {
            serde_json::json!({
                "id": format!("ev-{sequence}"), "sequence": sequence, "create_at": sequence * 1000,
                "type": "input", "name": name, "data": {"input_id": id},
            })
        };
        let mut main = Projection::default();
        apply(&mut main, &[input(1, "in-1", "find the leak", "queue")]);
        main.set_running(true);
        apply(
            &mut main,
            &[
                input(2, "in-2", "withdrawn", "queue"),
                control(3, "deleted", "in-2"),
                input(4, "in-4", "later", "queue"),
                control(6, "applied", "in-4"),
            ],
        );
        let rows = main.snapshot().rows;
        let later = rows.iter().find(|row| row.text == "later").unwrap();
        assert_eq!(
            later.cut,
            Some(3),
            "a message that waited in the queue cuts before its submission"
        );
        apply(
            &mut main,
            &[line_event(
                7,
                "create",
                serde_json::json!({
                    "at": {"kind": "sequence", "sequence": 3},
                    "source": {"kind": "host", "operation_id": "op-1"},
                    "branch": "b-1",
                    "prompt": "retry",
                }),
            )],
        );
        assert_eq!(
            main.line_views()[0].cut.map(|cut| cut.messages),
            Some(1),
            "a withdrawn message is not counted"
        );
    }

    #[test]
    fn a_user_message_stays_pending_until_it_is_consumed() {
        let input = |sequence: u64, id: &str, text: &str, mode: &str| {
            serde_json::json!({
                "id": id, "sequence": sequence, "create_at": sequence * 1000,
                "type": "input", "name": "message",
                "data": {"user": {"content": [{"type": "text", "text": text}]}, "mode": mode},
            })
        };
        let control = |sequence: u64, name: &str, id: &str| {
            serde_json::json!({
                "id": format!("ev-{sequence}"), "sequence": sequence, "create_at": sequence * 1000,
                "type": "input", "name": name, "data": {"input_id": id},
            })
        };
        let mut main = Projection::default();
        apply(&mut main, &[input(1, "in-1", "find the leak", "queue")]);
        assert!(
            main.snapshot().rows.is_empty(),
            "a message enters the transcript when the queue applies it"
        );
        assert!(
            main.snapshot().queued.is_empty(),
            "the queue strip belongs to a running turn"
        );
        apply(&mut main, &[control(2, "applied", "in-1")]);
        let rows = main.snapshot().rows;
        assert!(
            rows[0].pending,
            "a message applied but not consumed is pending"
        );
        apply(&mut main, &[control(3, "consumed", "in-1")]);
        assert!(
            !main.snapshot().rows[0].pending,
            "a consumed message settles out of pending"
        );
        main.set_running(true);
        apply(&mut main, &[input(4, "in-2", "and the test", "queue")]);
        assert_eq!(
            main.snapshot().queued.len(),
            1,
            "a message queued while a turn runs waits in the queue strip"
        );
        assert_eq!(
            main.snapshot().rows.len(),
            1,
            "a message without its input/applied is not in the transcript"
        );
        apply(&mut main, &[control(5, "applied", "in-2")]);
        let rows = main.snapshot().rows;
        assert_eq!(rows[1].text, "and the test");
        assert!(
            rows[1].pending,
            "a message applied but not consumed is pending"
        );
        assert!(
            main.snapshot().queued.is_empty(),
            "an applied message leaves the queue strip"
        );
        // A cancelled turn settles its own row after the applied message
        // (timeline UI §4.8): the message has to be committed first.
        apply(
            &mut main,
            &[serde_json::json!({
                "id": "end-6", "sequence": 6, "create_at": 6000,
                "type": "turn", "name": "end",
                "data": {"turn_id": "t1", "status": "cancelled", "reason": "user stop"},
            })],
        );
        let rows = main.snapshot().rows;
        assert_eq!(rows.last().map(|row| row.text.as_str()), Some("stopped"));
        assert!(
            rows.iter()
                .any(|row| row.text == "and the test" && row.pending),
            "a stop leaves the applied message pending"
        );
        apply(&mut main, &[control(7, "consumed", "in-2")]);
        assert!(
            !main.snapshot().rows[1].pending,
            "a consumed message settles out of pending"
        );
        apply(
            &mut main,
            &[
                input(8, "in-3", "withdrawn", "queue"),
                control(9, "deleted", "in-3"),
            ],
        );
        assert!(
            main.snapshot()
                .rows
                .iter()
                .all(|row| row.text != "withdrawn"),
            "a withdrawn message never becomes a row"
        );
        // A reattach folds history before it knows whether the line runs; the
        // same events still draw nothing without their `input/applied`.
        let mut replay = Projection::default();
        apply(
            &mut replay,
            &[
                input(1, "in-5", "withdrawn", "queue"),
                control(2, "deleted", "in-5"),
                input(3, "in-6", "sent later", "queue"),
            ],
        );
        assert!(
            replay.snapshot().rows.is_empty(),
            "replay draws nothing before its input/applied"
        );
        assert!(
            replay.snapshot().queued.is_empty(),
            "replay does not guess that a turn runs"
        );
    }

    #[test]
    fn snapshots_preserve_prior_output_while_streaming_continues() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[step(1, "thinking", "plan"), step(2, "text", "Hel")],
        );
        let before = projection.snapshot().rows;
        apply(&mut projection, &[step(3, "text", "lo")]);
        let after = projection.snapshot().rows;
        assert_eq!(
            (before[1].text.as_str(), after[1].text.as_str()),
            ("Hel", "Hello")
        );
    }

    #[test]
    fn a_shell_run_the_app_server_recorded_is_a_shell_row() {
        let marker = "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"0\" truncated=\"false\">\n$ ls\na\n\n</dim-shell-input>";
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                serde_json::json!({ "id": "in-1", "sequence": 1, "create_at": 1000, "type": "input", "name": "message",
                    "data": { "blocks": [{ "type": "raw", "text": marker }], "mode": "append" } }),
                serde_json::json!({ "id": "in-2", "sequence": 2, "create_at": 2000, "type": "input", "name": "message",
                    "data": { "blocks": [{ "type": "raw", "text": "<other>context</other>" }] } }),
            ],
        );
        let rows = projection.snapshot().rows;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].kind, RowKind::Shell);
        assert_eq!(rows[0].text, marker);
    }

    #[test]
    fn a_compaction_row_runs_until_its_result_shows_what_replaced_the_context() {
        let mut projection = Projection::default();
        apply(&mut projection, &[compaction_start(3)]);
        let rows = projection.snapshot().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, RowKind::Compaction);
        assert_eq!(
            rows[0].compaction,
            Some(CompactionView {
                state: CompactionState::Running,
                text: String::new(),
                insufficient: Vec::new(),
            })
        );

        apply(
            &mut projection,
            &[compaction(
                4,
                Some(3),
                serde_json::json!({
                    "status": "completed",
                    "retain_from": "event-7",
                    "strategy": "summary",
                    "replacement": [{"role": "user", "content": "<dim-summary>\nThe parser is half done.\n</dim-summary>"}],
                    "diagnostics": diagnostics(),
                }),
            )],
        );
        let rows = projection.snapshot().rows;
        assert_eq!(rows.len(), 1, "the result settles the row its start opened");
        assert_eq!(
            rows[0].compaction,
            Some(CompactionView {
                state: CompactionState::Completed,
                text: "<dim-summary>\nThe parser is half done.\n</dim-summary>".into(),
                insufficient: Vec::new(),
            })
        );
    }

    #[test]
    fn a_completed_record_projects_only_its_strategy_messages() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                compaction(
                    2,
                    None,
                    serde_json::json!({
                        "status": "completed",
                        "retain_from": "event-7",
                        "strategy": "summary",
                        "replacement": [
                            {"role": "user", "content": "<dim-summary>\nThe parser is half done.\n</dim-summary>"},
                            {"role": "user", "content": "<dim-digest>\nwrote src/cache.rs\nran exit 0: cargo test -p cache\n</dim-digest>"},
                            {"role": "user", "content": "Try the RwLock before touching the parser."},
                        ],
                        "replacement_parts": {"strategy": 1, "digest": 1},
                        "diagnostics": diagnostics(),
                    }),
                ),
                compaction(
                    3,
                    None,
                    serde_json::json!({
                        "status": "completed",
                        "retain_from": "event-7",
                        "strategy": "summary",
                        "replacement": [
                            {"role": "user", "content": "<dim-summary>\nThe parser is half done.\n</dim-summary>"},
                            {"role": "user", "content": "Try the RwLock before touching the parser."},
                        ],
                        "replacement_parts": {"strategy": 1, "digest": 0},
                        "diagnostics": diagnostics(),
                    }),
                ),
            ],
        );
        let rows = projection.snapshot().rows;
        let summary = "<dim-summary>\nThe parser is half done.\n</dim-summary>";
        let summaries: Vec<&str> = rows
            .iter()
            .map(|row| {
                let compaction = row.compaction.as_ref().expect("a compaction row");
                assert_eq!(compaction.state, CompactionState::Completed);
                compaction.text.as_str()
            })
            .collect();
        assert_eq!(summaries, [summary, summary]);
    }

    #[test]
    fn an_outcome_that_is_not_completed_carries_its_reason() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                compaction_start(2),
                compaction(
                    3,
                    Some(2),
                    serde_json::json!({
                        "status": "skipped",
                        "reason": "nothing to cut",
                        "diagnostics": diagnostics(),
                    }),
                ),
                compaction_start(5),
                compaction(
                    6,
                    Some(5),
                    serde_json::json!({
                        "status": "failed",
                        "error": "the summarizer returned nothing",
                    }),
                ),
                compaction_start(8),
                compaction(
                    9,
                    Some(8),
                    serde_json::json!({
                        "status": "cancelled",
                        "reason": "the capability was reloading",
                    }),
                ),
            ],
        );
        let rows = projection.snapshot().rows;
        let states: Vec<(CompactionState, &str)> = rows
            .iter()
            .map(|row| {
                let compaction = row.compaction.as_ref().expect("a compaction row");
                (compaction.state, compaction.text.as_str())
            })
            .collect();
        assert_eq!(
            states,
            [
                (CompactionState::Skipped, "nothing to cut"),
                (CompactionState::Failed, "the summarizer returned nothing"),
                (CompactionState::Cancelled, "the capability was reloading"),
            ]
        );
    }

    #[test]
    fn a_result_no_start_opened_still_gets_its_row() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[compaction(
                4,
                None,
                serde_json::json!({
                    "status": "failed",
                    "error": "context is not configured",
                }),
            )],
        );
        let rows = projection.snapshot().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].compaction,
            Some(CompactionView {
                state: CompactionState::Failed,
                text: "context is not configured".into(),
                insufficient: Vec::new(),
            })
        );
    }
}
