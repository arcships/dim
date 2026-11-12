//! Folds App Server session events into the transcript the screen draws.
//!
//! The worker applies every event once, in sequence order, and publishes
//! [`ViewSnapshot`]s built here. Only the rows the screen needs are kept; the
//! durable history stays with the App Server.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dim_agent_types::{
    BranchCreateAt, BranchCreateFailedData, BranchSource, CollaborationDelivery,
    CollaborationOutcome, CollaborationTarget, ContentBlock, ContextCompactionResult, ExecStatus,
    InputMode, InsufficientSpacePartName, JsonObject, Message, MessageContent, MessagePart,
    ModelFailure, ModelFailureKind, ModelOutputOutcome, ReplacementParts, ToolContent, ToolResult,
    TurnOutcome, UserInput, markup,
};
use serde_json::Value;

use crate::model::shell::ShellRun;
use crate::model::view::{
    ApprovalView, CompactionDetail, CompactionDetailState, CompactionPart, CompactionPartSize,
    CompactionState, CompactionView, FailureDetail, LineCut, LineState, LineView, MemberCard,
    MemberConversationState, MemberTarget, QueuedInput, RetryView, RowKind, RowTone, TaskFinish,
    TaskKind, TaskOutcome, TaskTarget, TaskView, TurnEnd, ViewRow, ViewSnapshot, WorkflowView,
};
use crate::wire::{Event, EventContent, Progress, WorkflowRun, WorkflowState};

/// Reasoning that is still arriving: the row shows what the Timeline holds
/// plus the progress not written yet.
struct OpenThought {
    index: usize,
    /// The request's `step/thinking` deltas so far.
    written: String,
    /// When the block's first durable `step/thinking` arrived; the block's
    /// start. `None` while only progress shows it.
    started_at: Option<u64>,
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
    /// The reasoning row each model request streams into while it is open.
    thinking_rows: HashMap<String, OpenThought>,
    thinking_characters: HashMap<String, u64>,
    /// The tool row each call id opened.
    tool_rows: HashMap<(String, String), usize>,
    /// The compaction row each `context/compaction-start` opened, by its
    /// sequence: the result names that start.
    compaction_rows: HashMap<u64, usize>,
    running: bool,
    approvals: Vec<ApprovalView>,
    pub questions: crate::question::Pending,
    inputs: Vec<dim_agent_types::PendingInput>,
    /// The row each written user message draws in, by input id, until its
    /// `input/consumed` settles that row.
    input_rows: HashMap<String, usize>,
    /// Unfinished tasks and the latest 50 terminal results.
    tasks: Vec<TaskView>,
    /// The `exec` call each background task came from, until it ends.
    background: HashMap<TaskTarget, (String, String)>,
    /// The newest turn end: its sequence and how the turn ended.
    ended: Option<(u64, TurnEnd)>,
    /// Requests whose text, thinking or tool row already showed.
    showed: HashSet<String>,
    /// The `error` of a model failure already shown this turn: the turn's
    /// repeat of it prints nothing.
    shown_failure: Option<String>,
    /// The retry the running request waits for, while it sleeps.
    retry: Option<RetryView>,
    /// The call row carrying each delegation's card, by request event id.
    cards: HashMap<String, usize>,
    /// Native Agent names from `agent.list`, by Agent id.
    agent_names: HashMap<String, String>,
    /// Agent ids a finished `agent.list` read was asked to name, whether or
    /// not it returned them: only a card naming another id reads again.
    agents_read: std::collections::HashSet<String>,
    pub goal: Option<crate::goal::Goal>,
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

    /// Applies an event the live stream carried.
    pub fn apply(&mut self, branch_id: &str, event: &Event) {
        self.fold(branch_id, event, false);
    }

    /// Applies an event a read replayed from recorded history: a task it ends
    /// had already ended before this interface attached.
    pub fn replay(&mut self, branch_id: &str, event: &Event) {
        self.fold(branch_id, event, true);
    }

    fn fold(&mut self, branch_id: &str, event: &Event, replayed: bool) {
        match &event.content {
            EventContent::ToolCall(data) => {
                self.questions
                    .asked(branch_id, &event.id, &data.request_id, &data.calls);
                if self.owns(branch_id) {
                    if let Some(parent) = &event.parent_call_id {
                        if !self
                            .tool_rows
                            .contains_key(&(data.request_id.clone(), parent.clone()))
                        {
                            return;
                        }
                        for call in &data.calls {
                            if self
                                .tool_rows
                                .contains_key(&(data.request_id.clone(), call.id.clone()))
                            {
                                continue;
                            }
                            let row_id = serde_json::to_string(&(&data.request_id, &call.id))
                                .expect("tool identity is JSON");
                            let index = self.push(row_id, RowKind::ToolCall, String::new());
                            let row = self.row(index);
                            row.call_id = Some(call.id.clone());
                            row.tool_lookup_done = true;
                            row.tool_name = Some(call.name.clone());
                            row.tool_input = Some(Value::Object(call.input.clone()));
                            row.batch = Some(data.request_id.clone());
                            row.parent_call_id = Some(parent.clone());
                            row.pending = true;
                            self.tool_rows
                                .insert((data.request_id.clone(), call.id.clone()), index);
                        }
                    }
                    for call in data
                        .calls
                        .iter()
                        .filter(|call| call.name == crate::question::TOOL)
                    {
                        if let Some(index) = self
                            .tool_rows
                            .get(&(data.request_id.clone(), call.id.clone()))
                            .copied()
                        {
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
        let sequence = event.sequence;
        match &event.content {
            EventContent::Input(data) => {
                if let Some(user) = &data.user {
                    self.inputs.push(dim_agent_types::PendingInput {
                        input_id: event.id.clone(),
                        sequence,
                        mode: data.effective_mode(),
                        user: Some(user.clone()),
                        applied: false,
                    });
                } else if let Some(text) = data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    ShellRun::parse(text).is_some().then(|| text.clone())
                }) {
                    self.push(format!("user-{sequence}"), RowKind::Shell, text);
                } else if let Some(body) = data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    hook_follow_up(text)
                }) {
                    self.push(format!("follow-up-{sequence}"), RowKind::HookFollowUp, body);
                } else if let Some(row) = data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    branch_notice(text, data.mode)
                }) {
                    self.push(format!("line-{sequence}"), RowKind::Status, row);
                }
            }
            EventContent::InputApplied(data) => {
                if let Some(input) = self
                    .inputs
                    .iter_mut()
                    .find(|input| input.input_id == data.input_id && !input.applied)
                {
                    input.applied = true;
                    let input = input.clone();
                    self.push_user(
                        &input.input_id,
                        sequence,
                        input.sequence,
                        input.user.as_ref().expect("visible input"),
                    );
                }
            }
            EventContent::InputDeleted(data) => {
                self.inputs.retain(|input| input.input_id != data.input_id);
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
                self.exec_started(branch_id, data, &event.id, event.create_at)
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
                        outcome: match data.status {
                            ExecStatus::Completed => TaskOutcome::Completed,
                            ExecStatus::Failed => TaskOutcome::Failed,
                            ExecStatus::Cancelled => TaskOutcome::Stopped,
                        },
                        replayed,
                        text: result_text(&data.result),
                        exit_code: data
                            .result
                            .structured_content
                            .as_ref()
                            .and_then(|value| value.get("exitCode"))
                            .and_then(Value::as_i64),
                    });
                }
                if let Some((request_id, call_id)) = self.background.remove(&target)
                    && self.owns(branch_id)
                    && let Some(index) = self.rows.iter().position(|row| {
                        row.call_id.as_deref() == Some(call_id.as_str())
                            && row.batch.as_deref() == Some(request_id.as_str())
                    })
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
                self.delegation_started(branch_id, data, &event.id, event.create_at)
            }
            EventContent::CollaborationDelivered(data) => {
                self.delegation_delivered(branch_id, data, event.create_at, replayed)
            }
            EventContent::CollaborationResult(data) => {
                let (status, outcome, text) = match &data.outcome {
                    CollaborationOutcome::Completed => {
                        ("Completed", TaskOutcome::Completed, data.text.clone())
                    }
                    CollaborationOutcome::Failed { reason } => (
                        "Failed",
                        TaskOutcome::Failed,
                        format!("{reason}\n{}", data.text),
                    ),
                    CollaborationOutcome::Cancelled { reason } => (
                        "Cancelled",
                        TaskOutcome::Stopped,
                        format!("{reason}\n{}", data.text),
                    ),
                };
                let finish = TaskFinish {
                    at: event.create_at,
                    status,
                    outcome,
                    replayed,
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
            EventContent::ModelStart(_) => {}
            EventContent::Thinking(data) => {
                *self
                    .thinking_characters
                    .entry(data.request_id.clone())
                    .or_default() += data.delta.chars().count() as u64;
                self.showed.insert(data.request_id.clone());
                let index = self.open_thinking(&data.request_id);
                let thought = self
                    .thinking_rows
                    .get_mut(&data.request_id)
                    .expect("opened above");
                let first = thought.written.is_empty();
                thought.started_at.get_or_insert(event.create_at);
                thought.written.push_str(&data.delta);
                let text = thought.written.clone();
                let row = self.row(index);
                row.text = text;
                if first {
                    row.row_id = format!("thinking-{}", event.id);
                }
            }
            EventContent::Text(data) => {
                self.showed.insert(data.request_id.clone());
                self.close_thinking(&data.request_id, event.create_at);
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
                self.showed.insert(data.request_id.clone());
                self.close_thinking(&data.request_id, event.create_at);
                self.close_assistant(&data.request_id);
                let row_id = serde_json::to_string(&(&data.request_id, &data.call.id))
                    .expect("tool identity is JSON");
                let index = self.push(row_id, RowKind::ToolCall, String::new());
                let row = self.row(index);
                row.call_id = Some(data.call.id.clone());
                row.tool_lookup_done = true;
                row.tool_name = Some(data.call.name.clone());
                row.tool_input = Some(Value::Object(data.call.input.clone()));
                row.batch = Some(data.request_id.clone());
                row.pending = true;
                self.tool_rows
                    .insert((data.request_id.clone(), data.call.id.clone()), index);
            }
            EventContent::ToolUseStart(data) => {
                self.close_thinking(&data.request_id, event.create_at);
            }
            EventContent::ToolCompleted(data) => {
                self.complete_tool(&data.request_id, &data.call_id, &data.result, true);
            }
            EventContent::ToolResult(data) => {
                for result in &data.results {
                    self.complete_tool(
                        &data.request_id,
                        &result.call_id,
                        &result.result,
                        event.parent_call_id.is_none(),
                    );
                }
            }
            EventContent::CompactionStart(_) => {
                let index = self.push(
                    format!("compaction-{sequence}"),
                    RowKind::Compaction,
                    String::new(),
                );
                self.row(index).compaction = Some(CompactionView {
                    detail: None,
                    state: CompactionState::Running,
                    text: String::new().into(),
                    insufficient: Vec::new(),
                });
                self.compaction_rows.insert(sequence, index);
            }
            EventContent::Compaction(data) => {
                self.finish_compaction(sequence, data.start, compaction_view(&data.outcome));
            }
            EventContent::CompactionSummary(data) => {
                self.finish_compaction(
                    sequence,
                    data.start,
                    CompactionView {
                        state: CompactionState::Completed,
                        detail: Some(CompactionDetail {
                            event_id: event.id.clone(),
                            sequence,
                            state: CompactionDetailState::Unloaded,
                        }),
                        text: String::new().into(),
                        insufficient: compaction_sizes(data.insufficient_space.as_ref()),
                    },
                );
            }
            EventContent::ModelEnd(data) => {
                self.thinking_characters.remove(&data.request_id);
                let incomplete = self.showed.remove(&data.request_id);
                self.close_thinking(&data.request_id, event.create_at);
                self.close_assistant(&data.request_id);
                self.retry = None;
                if let ModelOutputOutcome::Failed {
                    error,
                    presentation,
                    failure,
                    budget,
                } = &data.outcome
                {
                    let line = match (presentation, failure) {
                        (Some(presentation), _) => {
                            Some((presentation_line(presentation, error), None))
                        }
                        (None, Some(failure)) => Some((
                            failure_line(failure, incomplete),
                            Some(failure_detail(failure)),
                        )),
                        (None, None) if budget.is_none() => {
                            Some((format!("model failed: {error}"), None))
                        }
                        (None, None) => None,
                    };
                    if let Some((text, detail)) = line {
                        self.push_error(text, detail);
                        self.shown_failure = Some(error.clone());
                    }
                }
            }
            EventContent::TurnEnd(data) => {
                let open: Vec<String> = self.thinking_rows.keys().cloned().collect();
                for request_id in open {
                    self.close_thinking(&request_id, event.create_at);
                }
                self.turn_end(sequence, &data.outcome);
            }
            EventContent::InputConsumed(data) => {
                self.inputs.retain(|input| input.input_id != data.input_id);
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
        let shown = self.shown_failure.take();
        let end = match outcome {
            TurnOutcome::Failed { error } => {
                if shown.as_deref() != Some(error.as_str()) {
                    self.push_error(format!("turn failed: {error}"), None);
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

    pub fn active_output(&mut self, output: &dim_agent_types::ActiveOutput) {
        self.thinking_characters
            .insert(output.request_id.clone(), output.thinking_characters);
    }

    pub fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    /// A user message as a transcript row: a shell run's record draws as one.
    /// It cuts just before `submitted`, where its input was written, even
    /// when it enters the transcript later (timeline UI §4.1), and a message
    /// stays pending until the model consumes it (§4.8).
    ///
    /// The row carries the operator's words. Context a capability appended
    /// after them is not theirs, so each `dim-hooks` block becomes its own row
    /// right after this one (`crates/dim-agent-types/src/markup.rs`). Each
    /// names this row as its parent, so whatever settles the message settles
    /// them and they never commit it on their own.
    fn push_user(&mut self, input_id: &str, sequence: u64, submitted: u64, user: &UserInput) {
        let text = user_text(&user.content);
        let kind = if ShellRun::parse(&text).is_some() {
            RowKind::Shell
        } else {
            RowKind::UserMessage
        };
        let index = self.push(format!("user-{sequence}"), kind, text);
        let row_id = self.rows[index].row_id.clone();
        let row = self.row(index);
        row.cut = Some(submitted.saturating_sub(1));
        row.display = user_display(&user.content);
        row.pending = kind == RowKind::UserMessage;
        if row.pending {
            self.input_rows.insert(input_id.to_owned(), index);
        }
        for (at, body) in hook_contexts(&user.content).into_iter().enumerate() {
            let hook = self.push(format!("hook-{sequence}-{at}"), RowKind::HookContext, body);
            self.row(hook).parent_row = Some(row_id.clone());
        }
    }

    /// The content of a held message, to re-submit it as Steer.
    pub fn held_input(&self, input_id: &str) -> Option<&UserInput> {
        self.inputs
            .iter()
            .find(|input| input.input_id == input_id && !input.applied)
            .and_then(|input| input.user.as_ref())
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
        if let Some(index) = self
            .tool_rows
            .iter()
            .find_map(|((_, call), index)| (call == &request_id).then_some(*index))
        {
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
        let mut pending: Vec<_> = self.inputs.iter().collect();
        pending.sort_by_key(|input| (!input.applied, input.sequence));
        let queued = pending
            .into_iter()
            .filter_map(|input| {
                let user = input.user.as_ref()?;
                Some(QueuedInput {
                    id: input.input_id.clone(),
                    text: user_text(&user.content),
                    display: user_display(&user.content),
                    held: !input.applied,
                    recallable: !input.applied && recallable(&user.content),
                })
            })
            .collect();
        ViewSnapshot {
            rows: self.rows.clone(),
            running: self.running,
            approvals: self.approvals.clone(),
            questions: self.questions.calls().cloned().collect(),
            queued,
            tasks: self.tasks.clone(),
            ended: self.ended,
            goal: self.goal.clone(),
            retry: self.retry.clone(),
            ..ViewSnapshot::default()
        }
    }

    pub fn exec_started(
        &mut self,
        branch_id: &str,
        data: &dim_agent_types::ExecStartedData,
        event_id: &str,
        started_at: u64,
    ) {
        if self
            .tasks
            .iter()
            .any(|task| task.branch_id == branch_id && task.id == event_id)
        {
            return;
        }

        let label = self
            .rows
            .iter()
            .find(|row| {
                row.call_id.as_deref() == Some(data.call_id.as_str())
                    && row.batch.as_deref() == Some(data.request_id.as_str())
            })
            .and_then(|row| row.tool_input.as_ref()?.get("command")?.as_str())
            .filter(|command| !command.is_empty())
            .unwrap_or(data.description.as_str())
            .to_owned();
        let target = TaskTarget {
            branch_id: branch_id.to_owned(),
            task_id: data.task_id.clone(),
        };
        self.tasks.push(TaskView {
            id: event_id.to_owned(),
            branch_id: branch_id.to_owned(),
            kind: TaskKind::Command(target.clone()),
            label,
            started_at,
            finished: None,
        });
        self.background
            .insert(target, (data.request_id.clone(), data.call_id.clone()));
    }
    pub fn delegation_started(
        &mut self,
        branch_id: &str,
        data: &dim_agent_types::CollaborationRequestData,
        event_id: &str,
        started_at: u64,
    ) {
        if self
            .tasks
            .iter()
            .any(|task| task.branch_id == branch_id && task.id == event_id)
        {
            return;
        }

        let task = TaskView {
            id: event_id.to_owned(),
            branch_id: branch_id.to_owned(),
            kind: TaskKind::Member(MemberTarget::Starting),
            label: data.prompt.lines().next().unwrap_or("").to_owned(),
            started_at,
            finished: None,
        };
        if self.owns(branch_id)
            && let Some(index) = self
                .tool_rows
                .get(&(data.request_id.clone(), data.call_id.clone()))
                .copied()
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
            self.cards.insert(event_id.to_owned(), index);
        }
        self.tasks.push(task);
    }
    pub fn delegation_delivered(
        &mut self,
        branch_id: &str,
        data: &dim_agent_types::CollaborationDeliveredData,
        started_at: u64,
        replayed: bool,
    ) {
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
                    at: started_at,
                    status: "Failed",
                    outcome: TaskOutcome::Failed,
                    replayed,
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

    pub fn pending_exec(&mut self, branch: &str, task: &dim_agent_types::PendingExec) {
        if self
            .tasks
            .iter()
            .any(|known| known.branch_id == branch && known.id == task.event_id)
        {
            return;
        }
        let target = TaskTarget {
            branch_id: branch.into(),
            task_id: task.task_id.clone(),
        };
        self.tasks.push(TaskView {
            id: task.event_id.clone(),
            branch_id: branch.into(),
            kind: TaskKind::Command(target.clone()),
            label: task.description.clone(),
            started_at: task.started_at,
            finished: None,
        });
        self.background
            .insert(target, (task.request_id.clone(), task.call_id.clone()));
    }

    pub fn clear_inventory(&mut self, branch: &str) {
        self.questions.clear_branch(branch);
        self.tasks
            .retain(|task| task.branch_id != branch || task.finished.is_some());
        self.background
            .retain(|target, _| target.branch_id != branch);
        if self.owns(branch) {
            self.inputs.clear();
            for (_, index) in self.input_rows.drain() {
                Arc::make_mut(&mut self.rows[index]).pending = false;
            }
        }
    }

    pub fn pending_input(&mut self, input: &dim_agent_types::PendingInput) {
        if input.user.is_none() {
            return;
        }
        if input.applied
            && let Some(index) = self.rows.iter().position(|row| {
                row.cut == Some(input.sequence.saturating_sub(1))
                    && row.kind == RowKind::UserMessage
            })
        {
            self.row(index).pending = true;
            self.input_rows.insert(input.input_id.clone(), index);
        }
        self.inputs.push(input.clone());
    }

    pub fn prepend(&mut self, mut older: Self) {
        older.rows.retain(|row| {
            if let Some(existing) = self
                .rows
                .iter_mut()
                .find(|existing| existing.row_id == row.row_id)
            {
                if matches!(
                    row.kind,
                    RowKind::AssistantMessage | RowKind::StreamingAssistant
                ) {
                    let existing = Arc::make_mut(existing);
                    existing.text = format!("{}{}", row.text, existing.text);
                } else if row.kind == RowKind::ToolCall && row.batch == existing.batch {
                    let existing = Arc::make_mut(existing);
                    existing.tool_lookup_done |= row.tool_lookup_done;
                    if existing.tool_input.is_none() {
                        existing.tool_name = row.tool_name.clone();
                        existing.tool_input = row.tool_input.clone();
                    }
                }
                false
            } else {
                true
            }
        });
        let count = older.rows.len();
        for map in [
            &mut self.assistant_rows,
            &mut self.input_rows,
            &mut self.cards,
        ] {
            for index in map.values_mut() {
                *index += count;
            }
        }
        for index in self.tool_rows.values_mut() {
            *index += count;
        }
        for index in self.compaction_rows.values_mut() {
            *index += count;
        }
        for thought in self.thinking_rows.values_mut() {
            thought.index += count;
        }
        older.rows.append(&mut self.rows);
        self.rows = older.rows;
    }

    pub fn trim(&mut self, limit: usize) {
        let count = self.rows.len().saturating_sub(limit);
        if count == 0 {
            return;
        }
        self.rows.drain(..count);
        for map in [
            &mut self.assistant_rows,
            &mut self.input_rows,
            &mut self.cards,
        ] {
            map.retain(|_, index| match index.checked_sub(count) {
                Some(at) => {
                    *index = at;
                    true
                }
                None => false,
            });
        }
        self.tool_rows
            .retain(|_, index| match index.checked_sub(count) {
                Some(at) => {
                    *index = at;
                    true
                }
                None => false,
            });
        self.compaction_rows
            .retain(|_, index| match index.checked_sub(count) {
                Some(at) => {
                    *index = at;
                    true
                }
                None => false,
            });
        self.thinking_rows
            .retain(|_, thought| match thought.index.checked_sub(count) {
                Some(at) => {
                    thought.index = at;
                    true
                }
                None => false,
            });
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

    pub fn missing_tool(&self) -> Option<(String, String)> {
        self.rows.iter().find_map(|row| {
            if row.kind != RowKind::ToolCall || row.tool_lookup_done {
                return None;
            }
            Some((row.batch.clone()?, row.call_id.clone()?))
        })
    }

    pub fn tool_lookup(
        &mut self,
        request: &str,
        call: &str,
        origin: Option<dim_agent_types::ToolUseData>,
    ) {
        if let Some(index) = self.rows.iter().position(|row| {
            row.batch.as_deref() == Some(request) && row.call_id.as_deref() == Some(call)
        }) {
            let row = self.row(index);
            row.tool_lookup_done = true;
            if let Some(origin) = origin {
                row.tool_name = Some(origin.call.name);
                row.tool_input = Some(Value::Object(origin.call.input));
            }
        }
    }

    pub fn rows(&self) -> &[Arc<ViewRow>] {
        &self.rows
    }

    /// The lines `main` recorded, without `main` itself (design §14).
    pub fn directory(&mut self, entry: &dim_client::wire::BranchEntry) {
        if entry.branch_id == "main" {
            return;
        }
        let previous = self
            .lines
            .iter()
            .position(|line| line.id == entry.branch_id);
        if previous.is_some()
            && entry.purpose.is_none()
            && entry.cut_messages.is_none()
            && entry.end.is_none()
        {
            return;
        }
        let view = LineView {
            id: entry.branch_id.clone(),
            parent: entry.parent_branch_id.clone(),
            purpose: entry.purpose.clone().unwrap_or_default(),
            state: if entry.state == "ended" {
                match &entry.end {
                    Some(end) => match (&end.source, &end.ancestor) {
                        (_, Some(ancestor)) => LineState::EndedWith(ancestor.clone()),
                        (BranchSource::Tool { .. }, None) => LineState::EndedItself,
                        (BranchSource::Host { .. }, None) => LineState::EndedByOperation,
                    },
                    None => LineState::Ended,
                }
            } else {
                LineState::Waiting
            },
            cut: entry
                .branch_point
                .zip(entry.cut_messages)
                .map(|(sequence, messages)| LineCut {
                    sequence,
                    messages: messages
                        .try_into()
                        .expect("message count fits the client address space"),
                }),
        };
        if let Some(index) = previous {
            self.lines[index] = view;
        } else {
            self.lines.push(view);
        }
    }

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

    /// Adopts a stored run of this Session from `branch_id`. Every caller
    /// reads the run's own record, so a run that already ended is history.
    pub fn set_workflow(&mut self, branch_id: &str, run: WorkflowRun) {
        let finished = (!run.state.unfinished()).then(|| {
            let (status, outcome) = match run.state {
                WorkflowState::Completed => ("Completed", TaskOutcome::Completed),
                WorkflowState::Failed => ("Failed", TaskOutcome::Failed),
                WorkflowState::Stopped => ("Stopped", TaskOutcome::Stopped),
                WorkflowState::Interrupted => ("Interrupted", TaskOutcome::Failed),
                WorkflowState::Running | WorkflowState::Stopping => unreachable!("terminal"),
            };
            TaskFinish {
                at: run.ended_at.unwrap_or(run.created_at),
                status,
                outcome,
                replayed: true,
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
            call_id: None,
            tool_lookup_done: false,
            tool_name: None,
            tool_input: None,
            batch: None,
            parent_call_id: None,
            pending: false,
            parked: false,
            result_text: None,
            result_is_error: false,
            diff: None,
            exit_code: None,
            member: None,
            cut: None,
            line: None,
            parent_row: None,
            compaction: None,
            failure: None,
            reportable: false,
        }));
        self.rows.len() - 1
    }

    pub fn mark_reportable_error(&mut self, previous_rows: usize) -> bool {
        if previous_rows >= self.rows.len() {
            return false;
        }
        let Some(row) = self
            .rows
            .last_mut()
            .filter(|row| row.kind == RowKind::Error)
        else {
            return false;
        };
        Arc::make_mut(row).reportable = true;
        true
    }

    fn push_error(&mut self, text: String, failure: Option<FailureDetail>) {
        let index = self.push(format!("error-{}", self.rows.len()), RowKind::Error, text);
        let row = self.row(index);
        row.tone = Some(RowTone::Danger);
        row.failure = failure;
    }

    /// Shows a model request's reasoning that is not written yet. Progress
    /// that the written deltas overtook, or that runs ahead of them, waits
    /// for the next notice.
    pub fn progress(&mut self, progress: &Progress) {
        self.retry = progress.retry.as_ref().map(|retry| RetryView {
            attempt: retry.attempt,
            max_attempts: retry.max_attempts,
            retry_at_ms: retry.retry_at_ms,
            kind: retry.kind,
            reason: retry.reason.clone(),
        });
        let Some(request_id) = &progress.request_id else {
            return;
        };
        if !progress.thinking.is_empty() || !progress.text.is_empty() {
            self.showed.insert(request_id.clone());
        }
        if progress.thinking.is_empty() {
            return;
        }
        let index = self.open_thinking(request_id);
        if let Some(started_at) = progress.thinking_started_at {
            self.thinking_rows
                .get_mut(request_id)
                .expect("opened above")
                .started_at
                .get_or_insert(started_at);
        }
        let thought = &self.thinking_rows[request_id];
        if self
            .thinking_characters
            .get(request_id)
            .copied()
            .unwrap_or(0)
            != progress.thinking_offset
        {
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
                started_at: None,
            },
        );
        index
    }

    /// A thought settles when the request moves on, as the Timeline holds
    /// it: progress that was never written (an interrupted thought) goes, and
    /// its duration spans its first event to `closed_at`.
    fn close_thinking(&mut self, request_id: &str, closed_at: u64) {
        if let Some(thought) = self.thinking_rows.remove(request_id) {
            let row = self.row(thought.index);
            row.streaming = false;
            row.text = thought.written;
            row.elapsed_ms = thought
                .started_at
                .map(|start| closed_at.saturating_sub(start));
        }
    }

    fn close_assistant(&mut self, request_id: &str) {
        if let Some(index) = self.assistant_rows.remove(request_id) {
            self.row(index).streaming = false;
            self.row(index).kind = RowKind::AssistantMessage;
        }
    }

    fn finish_compaction(&mut self, sequence: u64, start: Option<u64>, view: CompactionView) {
        let index = match start.and_then(|start| self.compaction_rows.remove(&start)) {
            Some(index) => index,
            None => self.push(
                format!("compaction-{sequence}"),
                RowKind::Compaction,
                String::new(),
            ),
        };
        self.row(index).compaction = Some(view);
    }

    pub fn compaction_detail(
        &mut self,
        event_id: &str,
        sequence: u64,
        state: CompactionDetailState,
        data: Option<&dim_agent_types::ContextCompactionData>,
    ) -> bool {
        let Some(index) = self.rows.iter().position(|row| {
            row.compaction
                .as_ref()
                .and_then(|view| view.detail.as_ref())
                .is_some_and(|detail| detail.event_id == event_id && detail.sequence == sequence)
        }) else {
            return false;
        };
        let view = self.row(index).compaction.as_mut().expect("compaction row");
        if let Some(data) = data {
            let loaded = compaction_view(&data.outcome);
            view.text = loaded.text;
        } else {
            view.text = "".into();
        }
        view.detail.as_mut().expect("detail identity").state = state;
        true
    }

    fn complete_tool(
        &mut self,
        request_id: &str,
        call_id: &str,
        result: &ToolResult,
        create_if_missing: bool,
    ) {
        let index = self
            .tool_rows
            .remove(&(request_id.to_owned(), call_id.to_owned()))
            .or_else(|| {
                self.rows.iter().position(|row| {
                    row.call_id.as_deref() == Some(call_id)
                        && row.batch.as_deref() == Some(request_id)
                })
            })
            .or_else(|| {
                if !create_if_missing {
                    return None;
                }
                let row_id =
                    serde_json::to_string(&(request_id, call_id)).expect("tool identity is JSON");
                let index = self.push(row_id, RowKind::ToolCall, String::new());
                let row = self.row(index);
                row.call_id = Some(call_id.into());
                row.batch = Some(request_id.into());
                Some(index)
            });
        let Some(index) = index else {
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
            let insufficient = compaction_sizes(diagnostics.insufficient_space.as_ref());
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
        detail: None,
        state,
        text: text.into(),
        insufficient,
    }
}

fn compaction_sizes(space: Option<&dim_agent_types::InsufficientSpace>) -> Vec<CompactionPartSize> {
    space
        .into_iter()
        .flat_map(|space| &space.parts)
        .map(|part| CompactionPartSize {
            part: compaction_part(part.part),
            tokens: part.tokens,
        })
        .collect()
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

/// The body of a `Stop` follow-up: the input a hook wrote to continue the
/// conversation. Any other element and any other hook kind writes no row.
///
/// The body is stored escaped, as every runtime block's is, and is unescaped
/// here for the row that draws it.
///
/// The attribute search stops at the opening tag's `>` on purpose: a hook's
/// body escapes `<` and `>` but not `"`, so its text can hold a literal
/// `kind="follow-up"` and must not answer for the marker.
///
/// One follow-up input carries one block: Hooks writes it in `Branch::stop`
/// (`hooks/runner.rs`), one Queue input per round from
/// `vec![raw(follow_up, None)]`. Both callers take the first match, so a
/// second writer has to keep that shape or change this reader.
fn hook_follow_up(text: &str) -> Option<String> {
    if markup::element(text)? != "dim-hooks" {
        return None;
    }
    let open = text.strip_prefix("<dim-hooks")?;
    let attributes = open.split_once('>')?.0;
    (attr(attributes, "kind")? == "follow-up")
        .then(|| unescape(&element_body(text, "</dim-hooks>")))
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

/// The text of `blocks` verbatim, one block per line. For the model's own
/// text — a tool result, a provider message — where a marker is part of what
/// it said (`<dim-hooks …>` in a tool result stays as written).
fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The operator's own words of a message: its text blocks, without the runtime
/// context a capability appended after them
/// (`crates/dim-agent-types/src/markup.rs`). The first block is always theirs.
/// A later block that opens any of the vocabulary's names is not theirs; only
/// `dim-hooks` and `dim-skill` have a row of their own.
pub(crate) fn user_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .enumerate()
        .filter(|(index, block)| match block {
            ContentBlock::Text { text } => !markup::runtime_context(*index, text),
            ContentBlock::Image { .. } => true,
        })
        .filter_map(|(_, block)| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of each hook context a message carries, in order, unescaped: the
/// producers store it escaped, and the row draws the hook's own words.
pub(crate) fn hook_contexts(blocks: &[ContentBlock]) -> Vec<String> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let ContentBlock::Text { text } = block else {
                return None;
            };
            (markup::runtime_element(index, text) == Some("dim-hooks"))
                .then(|| unescape(&element_body(text, "</dim-hooks>")))
        })
        .collect()
}

/// The one line the plain log prints for an input no operator wrote: a
/// `Stop` follow-up's label. Every other raw block stays silent there, as it
/// always has, because the log has no disclosure to open the body behind.
pub(crate) fn raw_input_line(blocks: &[dim_agent_types::InputBlock]) -> Option<String> {
    blocks.iter().find_map(|block| {
        let dim_agent_types::InputBlock::Raw { text, .. } = block;
        let body = hook_follow_up(text)?;
        Some(hook_follow_up_label(body.trim().lines().count().max(1)))
    })
}

/// The one line that names a loaded Skill, in the interface language: the
/// message's presentation and the plain log both say it.
pub(crate) fn skill_loaded_label(name: &str) -> String {
    crate::ui::i18n::tf("Skill loaded: {name}", &[("name", name)])
}

/// The text of each message block the log prints after the operator's words, in
/// the order the content has them: one line per loaded Skill, then a hook's
/// context as the row names it. `None` for the words, which the `> ` record has.
pub(crate) fn context_lines(blocks: &[ContentBlock]) -> Vec<String> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ContentBlock::Text { text } => {
                if let Some(name) = loaded_skill(index, block) {
                    Some(skill_loaded_label(&name))
                } else if markup::runtime_element(index, text) == Some("dim-hooks") {
                    let body = element_body(text, "</dim-hooks>");
                    Some(hook_context_label(body.trim().lines().count().max(1)))
                } else {
                    None
                }
            }
            ContentBlock::Image { .. } => None,
        })
        .collect()
}

/// The one line that names a hook's context, in the interface language: the
/// TUI row and the plain log both say it. `lines` is the stored line count of
/// the body, which is one for a paragraph the terminal will wrap.
pub(crate) fn hook_context_label(lines: usize) -> String {
    match lines {
        1 => crate::ui::i18n::tf("Hook context · {count} line", &[("count", "1")]),
        count => crate::ui::i18n::tf(
            "Hook context · {count} lines",
            &[("count", &count.to_string())],
        ),
    }
}

/// The one line that names a `Stop` hook's follow-up, in the interface
/// language: the TUI row and the plain log both say it. `lines` counts the
/// stored lines of the body the same way [`hook_context_label`] does.
pub(crate) fn hook_follow_up_label(lines: usize) -> String {
    match lines {
        1 => crate::ui::i18n::tf("Stop hook follow-up · {count} line", &[("count", "1")]),
        count => crate::ui::i18n::tf(
            "Stop hook follow-up · {count} lines",
            &[("count", &count.to_string())],
        ),
    }
}

/// Whether a queued message can go back into the draft by Shift+Left: its own
/// words first, then only context the runtime appended for the model. An input
/// with an image or with a second operator text block stays queued, as it did
/// when the rule was "exactly one text block" — the notice about attachments
/// depends on that, not on appended context.
fn recallable(blocks: &[ContentBlock]) -> bool {
    blocks.iter().enumerate().all(|(index, block)| match block {
        ContentBlock::Text { text } => index == 0 || markup::runtime_context(index, text),
        ContentBlock::Image { .. } => false,
    })
}

/// The Skill a loaded block names: a text block after the first that opens the
/// Skill element names it, and its instructions are shown as one line instead.
fn loaded_skill(index: usize, block: &ContentBlock) -> Option<String> {
    let ContentBlock::Text { text } = block else {
        return None;
    };
    if markup::runtime_element(index, text) != Some("dim-skill") {
        return None;
    }
    let rest = text.strip_prefix(SKILL_BLOCK)?;
    let name = rest.split('"').next()?;
    (!name.is_empty()).then(|| {
        name.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
    })
}

/// The presentation of an operator message: its words with an image marker per
/// image and one line per loaded Skill; `None` when its words are the message.
fn user_display(blocks: &[ContentBlock]) -> Option<String> {
    let mut images = 0;
    let mut skills = Vec::new();
    let mut lines = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Text { text } => match loaded_skill(index, block) {
                Some(name) => skills.push(name),
                None if markup::runtime_context(index, text) => {}
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
        let line = skill_loaded_label(&skill);
        if !display.is_empty() {
            display.push('\n');
        }
        display.push_str(&line);
    }
    Some(display)
}

/// A provider presentation's line, as today: its own words, prefixed.
fn presentation_line(presentation: &dim_agent_types::ProviderPresentation, error: &str) -> String {
    let message = presentation.server_message.as_deref().unwrap_or(error);
    let action = presentation
        .action
        .as_deref()
        .map(|action| format!(". {action}"))
        .unwrap_or_default();
    format!("model failed: {message}{action}")
}

/// `<meaning>: <reason>`, worded from the kind through the interface
/// language; the reason is trimmed and loses its trailing period. Just the
/// meaning when the failure carries no reason.
pub(crate) fn failure_phrase(kind: ModelFailureKind, reason: Option<&str>) -> String {
    let reason = reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| reason.strip_suffix('.').unwrap_or(reason));
    let meaning = crate::ui::i18n::t(kind.meaning());
    match reason {
        Some(reason) => format!("{meaning}: {reason}"),
        None => meaning.to_owned(),
    }
}

/// The one line a model failure shows; `incomplete` prefixes it when the
/// request had output.
pub(crate) fn failure_line(failure: &ModelFailure, incomplete: bool) -> String {
    let prefix = if incomplete {
        format!(
            "{} ",
            crate::ui::i18n::t("The response above is incomplete.")
        )
    } else {
        String::new()
    };
    let attempts = if failure.attempts > 1 {
        let seconds = failure.waited_ms.saturating_add(500) / 1000;
        format!(
            " {}",
            crate::ui::i18n::tf(
                "Tried {count} times over {seconds} s.",
                &[
                    ("count", &failure.attempts.to_string()),
                    ("seconds", &seconds.to_string()),
                ]
            )
        )
    } else {
        String::new()
    };
    format!(
        "{prefix}{}. {}{attempts}",
        failure_phrase(failure.kind, failure.reason.as_deref()),
        crate::ui::i18n::t(failure.kind.action())
    )
}

/// Whole seconds until a retry starts, never below zero.
pub(crate) fn retry_seconds(retry_at_ms: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    retry_at_ms.saturating_sub(now).div_ceil(1000)
}

fn failure_detail(failure: &ModelFailure) -> FailureDetail {
    FailureDetail {
        kind: failure.kind,
        status: failure.status,
        provider_code: failure.provider_code.clone(),
        request_id: failure.request_id.clone(),
        attempts: failure.attempts,
        waited_ms: failure.waited_ms,
        retry_after_ms: failure.retry_after_ms,
        cause: failure.cause.clone(),
    }
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
        assert_eq!(
            rows[0].elapsed_ms, None,
            "an open thought claims no duration"
        );

        apply(&mut projection, &[step(6, "text", "Done.")]);
        let rows = projection.snapshot().rows;
        assert!(!rows[0].streaming);
        assert_eq!(rows[0].elapsed_ms, Some(1000), "the span of the block");
        assert_eq!(rows[1].kind, RowKind::StreamingAssistant);
    }

    #[test]
    fn progress_start_times_a_thought_before_its_first_written_event() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                serde_json::json!({ "id": "in-1", "sequence": 1, "create_at": 1000, "type": "input", "name": "message", "data": {} }),
                model_start(2),
            ],
        );
        let open: Progress = serde_json::from_value(serde_json::json!({
            "sessionId": "s", "branchId": "main", "requestId": "r1",
            "text": "", "textOffset": 0,
            "thinking": "weigh", "thinkingOffset": 0,
            "thinkingStartedAt": 4000,
        }))
        .unwrap();
        projection.progress(&open);
        assert_eq!(projection.snapshot().rows[0].text, "weigh");

        apply(&mut projection, &[step(5, "thinking", "weigh")]);
        apply(&mut projection, &[step(6, "text", "Done.")]);
        assert_eq!(
            projection.snapshot().rows[0].elapsed_ms,
            Some(2000),
            "the producer start, not the first written event, times the block"
        );
    }

    #[test]
    fn a_tool_start_settles_a_thought_before_its_arguments_arrive() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                serde_json::json!({ "id": "in-1", "sequence": 1, "create_at": 1000, "type": "input", "name": "message", "data": {} }),
                model_start(2),
            ],
        );
        apply(&mut projection, &[step(5, "thinking", "weigh")]);
        apply(
            &mut projection,
            &[serde_json::json!({
                "id": "tool-start-1", "sequence": 6, "create_at": 6000,
                "type": "step", "name": "tool-use-start",
                "data": { "turn_id": "t1", "request_id": "r1", "call_id": "call", "name": "echo" },
            })],
        );
        let rows = projection.snapshot().rows;
        assert!(!rows[0].streaming);
        assert_eq!(
            rows[0].elapsed_ms,
            Some(1000),
            "the thought ends when the call starts, not when its arguments finish"
        );
    }

    #[test]
    fn a_window_keeps_an_older_calls_result_and_earlier_load_supplies_its_arguments() {
        let result = serde_json::json!({"id":"result","sequence":20,"create_at":20,
            "type":"step","name":"tool-result","data":{"turn_id":"turn","request_id":"request",
            "results":[{"call_id":"call","result":{"content":"finished"}}]}});
        let mut current = Projection::default();
        apply(&mut current, &[result.clone(), result]);
        assert_eq!(current.rows().len(), 1);
        assert_eq!(current.rows()[0].result_text.as_deref(), Some("finished"));
        assert!(current.rows()[0].tool_input.is_none());
        let mut older = Projection::default();
        apply(
            &mut older,
            &[serde_json::json!({"id":"use","sequence":3,"create_at":3,
            "type":"step","name":"tool-use","data":{"turn_id":"turn","request_id":"request",
            "call":{"id":"call","name":"exec","input":{"command":"echo finished"}}}})],
        );
        current.prepend(older);
        assert_eq!(current.rows().len(), 1);
        let row = &current.rows()[0];
        assert_eq!(row.tool_name.as_deref(), Some("exec"));
        assert_eq!(row.tool_input.as_ref().unwrap()["command"], "echo finished");
        assert_eq!(row.result_text.as_deref(), Some("finished"));
        assert!(!row.pending);
    }

    #[test]
    fn a_window_resumes_reasoning_after_omitted_blocks_without_copying_the_prefix() {
        let mut projection = Projection::default();
        apply(&mut projection, &[step(100, "thinking", "visible")]);
        projection.active_output(&dim_agent_types::ActiveOutput {
            request_id: "r1".into(),
            text_characters: 500,
            thinking_characters: 1007,
        });
        projection.progress(&progress(" tail", 1007));
        assert_eq!(projection.snapshot().rows[0].text, "visible tail");
        apply(
            &mut projection,
            &[step(101, "thinking", " tail"), step(102, "text", "reply")],
        );
        projection.progress(&progress("next thought", 1012));
        let rows = projection.snapshot().rows;
        assert_eq!(rows[0].text, "visible tail");
        assert_eq!(rows[2].text, "next thought");
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
        assert!(!live.snapshot().queued[0].held);
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

    /// #1380: a prompt hook's context is not the operator's words. The row
    /// carries what they sent, the hook's block becomes its own row, and the
    /// queued form of the same input does not repeat the body.
    #[test]
    fn a_hooks_context_is_its_own_row_and_never_the_operators_words() {
        let hook = "<dim-hooks kind=\"prompt-submit\">\nread src/parser.rs\nif a &lt; b &amp;&amp; c &gt; d, keep it short\n</dim-hooks>";
        let input = serde_json::json!({
            "id": "hooked", "sequence": 9, "create_at": 1,
            "type": "input", "name": "message",
            "data": {"user": {"content": [
                {"type": "text", "text": "why does the parser stall?"},
                {"type": "text", "text": hook},
            ]}, "mode": "queue"},
        });
        let applied = serde_json::json!({
            "id": "hooked-applied", "sequence": 10, "create_at": 1,
            "type": "input", "name": "applied", "data": {"input_id": "hooked"},
        });
        let mut projection = Projection::default();
        apply(&mut projection, &[input]);

        // While it waits, the queue shows the operator's words only.
        let queued = projection.snapshot().queued;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].text, "why does the parser stall?");
        assert_eq!(
            queued[0].display, None,
            "the hook's body is not part of the message's presentation"
        );
        assert!(
            queued[0].recallable,
            "a message a hook added context to can still go back to the draft"
        );

        apply(&mut projection, &[applied]);
        let rows = projection.snapshot().rows;
        assert_eq!(rows[0].kind, RowKind::UserMessage);
        assert_eq!(rows[0].text, "why does the parser stall?");
        assert_eq!(rows[1].kind, RowKind::HookContext);
        assert_eq!(
            rows[1].text, "read src/parser.rs\nif a < b && c > d, keep it short",
            "the row draws the hook's words, not their escaped form"
        );
        assert_eq!(
            rows.len(),
            2,
            "one hook block is one row, and the message is the other"
        );
    }

    /// #1381: three capabilities write a `user`-less input to the same channel
    /// — a `Stop` follow-up, a `SessionStart` context and a `PostToolUse`
    /// context — and a Goal continuation travels it too. Only the follow-up
    /// draws a row, and it is not a message.
    #[test]
    fn only_a_follow_up_draws_a_hook_row() {
        let raw = |sequence: u64, text: &str| {
            serde_json::json!({
                "id": format!("raw-{sequence}"), "sequence": sequence, "create_at": 1,
                "type": "input", "name": "message",
                "data": {"blocks": [{"type": "raw", "text": text}], "mode": "queue"},
            })
        };
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                raw(
                    1,
                    "<dim-hooks kind=\"follow-up\">\nrun the suite\nfix parser::tests\nif a &lt; b &gt; c &amp;&amp; d, keep the diff small\n</dim-hooks>",
                ),
                raw(
                    2,
                    "<dim-hooks kind=\"session-start\">\nthe project's conventions\n</dim-hooks>",
                ),
                raw(
                    3,
                    "<dim-hooks kind=\"post-tool\" call_id=\"c1\" tool=\"exec\">\nthe command failed\n</dim-hooks>",
                ),
                raw(
                    4,
                    "<dim-goal kind=\"continuation\" branch=\"main\">\nkeep going\n</dim-goal>",
                ),
            ],
        );
        let rows = projection.snapshot().rows;
        assert_eq!(
            rows.len(),
            1,
            "one row for four inputs: {:?}",
            rows.iter().map(|row| &row.text).collect::<Vec<_>>()
        );
        assert_eq!(rows[0].kind, RowKind::HookFollowUp);
        assert_eq!(
            rows[0].text,
            "run the suite\nfix parser::tests\nif a < b > c && d, keep the diff small",
            "the row draws the hook's words, not their escaped form"
        );
        assert!(
            !rows[0].kind.is_message(),
            "a follow-up is not a message: no pick, count or rail reads it"
        );
        assert_eq!(rows[0].cut, None, "it carries no cut");
    }

    /// #1381: an operator message can still be pending when a `Stop` follow-up
    /// is written after it — `turn/end` and the follow-up write are separated
    /// by the hook run, and `Hooks.md` only drops the follow-up for an input
    /// *consumed* before the write. The follow-up then follows timeline UI
    /// §4.8 like a branch notice: it is a settled row after a pending message,
    /// and it never joins that message's rows the way a hook context does.
    #[test]
    fn a_follow_up_after_a_pending_message_is_its_own_settled_row() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                serde_json::json!({
                    "id": "spoken", "sequence": 1, "create_at": 1,
                    "type": "input", "name": "message",
                    "data": {"user": {"content": [
                        {"type": "text", "text": "still there?"},
                        {"type": "text", "text": "<dim-hooks kind=\"prompt-submit\">\nread the briefing\n</dim-hooks>"},
                    ]}, "mode": "queue"},
                }),
                serde_json::json!({
                    "id": "spoken-applied", "sequence": 2, "create_at": 1,
                    "type": "input", "name": "applied",
                    "data": {"input_id": "spoken", "mode": "queue"},
                }),
                serde_json::json!({
                    "id": "follow-up", "sequence": 3, "create_at": 1,
                    "type": "input", "name": "message",
                    "data": {"blocks": [{"type": "raw", "text":
                        "<dim-hooks kind=\"follow-up\">\nrun the suite\n</dim-hooks>"}],
                        "mode": "queue"},
                }),
            ],
        );
        let rows = projection.snapshot().rows;
        let kinds: Vec<RowKind> = rows.iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            [
                RowKind::UserMessage,
                RowKind::HookContext,
                RowKind::HookFollowUp
            ],
            "the pending message, its hook context, then the later follow-up"
        );
        assert!(rows[0].pending, "the message is still unread");
        assert_eq!(
            rows[1].parent_row.as_deref(),
            Some(rows[0].row_id.as_str()),
            "a hook context belongs to its message's group"
        );
        assert_eq!(
            rows[2].parent_row, None,
            "a follow-up belongs to no message: it is a later input, so nothing \
             that settles that message settles it"
        );
        assert!(
            !rows[2].pending,
            "a follow-up has no pending state: a raw input cannot be withdrawn \
             from the queue UI, so its row never changes"
        );
    }

    /// The first block is the operator's even when it opens a hook marker, and
    /// an input carrying their own second text block stays out of the draft as
    /// it did before, so the attachments notice keeps its meaning.
    #[test]
    fn an_operators_own_marker_is_their_words_and_two_blocks_stay_queued() {
        let input = |id: &str, sequence: u64, content: serde_json::Value| {
            serde_json::json!({
                "id": id, "sequence": sequence, "create_at": 1,
                "type": "input", "name": "message",
                "data": {"user": {"content": content}, "mode": "queue"},
            })
        };
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                input(
                    "typed",
                    1,
                    serde_json::json!([{"type": "text", "text": "<dim-hooks kind=\"prompt-submit\">mine</dim-hooks>"}]),
                ),
                input(
                    "two",
                    2,
                    serde_json::json!([
                        {"type": "text", "text": "first paragraph"},
                        {"type": "text", "text": "second paragraph"},
                    ]),
                ),
            ],
        );
        let queued = projection.snapshot().queued;
        let typed = queued.iter().find(|input| input.id == "typed").unwrap();
        assert_eq!(
            typed.text,
            "<dim-hooks kind=\"prompt-submit\">mine</dim-hooks>"
        );
        assert!(typed.recallable);
        let two = queued.iter().find(|input| input.id == "two").unwrap();
        assert_eq!(two.text, "first paragraph\nsecond paragraph");
        assert!(
            !two.recallable,
            "two operator blocks are not what Shift+Left scrolls back"
        );
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
        assert!(main.snapshot().queued[0].held);
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
        assert!(!main.snapshot().queued[0].held);
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
        assert_eq!(replay.snapshot().queued[0].id, "in-6");
        assert!(replay.snapshot().queued[0].held);
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
                detail: None,
                state: CompactionState::Running,
                text: String::new().into(),
                insufficient: Vec::new(),
            })
        );
        let mut lazy = Projection::default();
        apply(
            &mut lazy,
            &[
                compaction_start(3),
                serde_json::json!({"id":"event-4","sequence":4,"create_at":4,
                "type":"display","name":"compaction","data":{"start":3,"strategy":"summary"}}),
            ],
        );
        let rows = lazy.snapshot().rows;
        assert_eq!(rows.len(), 1, "the summary settles the same running row");
        let view = rows[0].compaction.as_ref().unwrap();
        assert_eq!(view.state, CompactionState::Completed);
        assert!(view.text.is_empty(), "the body has not been requested");
        assert_eq!(
            view.detail,
            Some(CompactionDetail {
                event_id: "event-4".into(),
                state: CompactionDetailState::Unloaded,
                sequence: 4
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
                detail: None,
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
                compaction.text.as_ref()
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
                (compaction.state, compaction.text.as_ref())
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
                detail: None,
                state: CompactionState::Failed,
                text: "context is not configured".into(),
                insufficient: Vec::new(),
            })
        );
    }

    fn model_end(sequence: u64, data: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "sequence": sequence,
            "create_at": sequence * 1000,
            "type": "step",
            "name": "model-output-end",
            "data": data,
        })
    }

    fn failure_data(extra: serde_json::Value) -> serde_json::Value {
        let mut data = serde_json::json!({
            "turn_id": "t1",
            "request_id": "r1",
            "status": "failed",
            "error": "fixture error",
        });
        for (key, value) in extra.as_object().unwrap() {
            data[key] = value.clone();
        }
        data
    }

    fn failure_fact() -> serde_json::Value {
        serde_json::json!({
            "kind": "server_error",
            "status": 503,
            "providerCode": "overloaded",
            "requestId": "req-provider",
            "reason": "The server is overloaded",
            "cause": ["upstream 503"],
            "attempts": 3,
            "waitedMs": 4200,
        })
    }

    #[test]
    fn a_failure_fact_is_one_line_with_its_details_and_no_prefix() {
        use dim_agent_types::ModelFailureKind;
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[model_end(
                1,
                failure_data(serde_json::json!({"failure": failure_fact()})),
            )],
        );
        let rows = projection.snapshot().rows;
        let row = rows
            .iter()
            .find(|row| row.kind == RowKind::Error)
            .expect("a failure row");
        assert_eq!(
            row.text,
            "The provider reported a server error: The server is overloaded. Try again later or choose another model. Tried 3 times over 4 s."
        );
        let details = row.failure.as_ref().expect("details");
        assert_eq!(details.kind, ModelFailureKind::ServerError);
        assert_eq!(details.status, Some(503));
        assert_eq!(details.provider_code.as_deref(), Some("overloaded"));
        assert_eq!(details.request_id.as_deref(), Some("req-provider"));
        assert_eq!((details.attempts, details.waited_ms), (3, 4200));
        assert_eq!(details.cause, ["upstream 503"]);
    }

    #[test]
    fn a_presentation_still_wins_over_the_failure_line() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[model_end(
                1,
                failure_data(serde_json::json!({
                    "failure": failure_fact(),
                    "presentation": {
                        "source": "fixture_provider",
                        "reason": "unknown_error",
                        "serverMessage": "provider words",
                        "action": "Try later.",
                    },
                })),
            )],
        );
        let rows = projection.snapshot().rows;
        let row = rows.iter().find(|row| row.kind == RowKind::Error).unwrap();
        assert_eq!(row.text, "model failed: provider words. Try later.");
        assert!(row.failure.is_none());
    }

    #[test]
    fn an_old_event_shows_the_error_with_its_prefix() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[model_end(1, failure_data(serde_json::json!({})))],
        );
        let rows = projection.snapshot().rows;
        let row = rows.iter().find(|row| row.kind == RowKind::Error).unwrap();
        assert_eq!(row.text, "model failed: fixture error");
    }

    #[test]
    fn a_failure_after_output_starts_with_the_incomplete_sentence() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                step(1, "text", "half an answer"),
                model_end(
                    2,
                    failure_data(serde_json::json!({"failure": failure_fact()})),
                ),
            ],
        );
        let rows = projection.snapshot().rows;
        let row = rows.iter().find(|row| row.kind == RowKind::Error).unwrap();
        assert!(
            row.text
                .starts_with("The response above is incomplete. The provider reported"),
            "{}",
            row.text
        );
    }

    #[test]
    fn a_single_attempt_has_no_tried_suffix() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[model_end(
                1,
                failure_data(serde_json::json!({"failure": {
                    "kind": "server_error",
                    "reason": "The server is overloaded",
                    "attempts": 1,
                    "waitedMs": 0,
                }})),
            )],
        );
        let rows = projection.snapshot().rows;
        let row = rows.iter().find(|row| row.kind == RowKind::Error).unwrap();
        assert_eq!(
            row.text,
            "The provider reported a server error: The server is overloaded. Try again later or choose another model."
        );
    }

    #[test]
    fn a_tool_row_alone_starts_the_incomplete_sentence() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                serde_json::json!({
                    "sequence": 1, "create_at": 1000, "type": "step", "name": "tool-use",
                    "data": {
                        "turn_id": "t1", "request_id": "r1",
                        "call": {"id": "c1", "name": "read", "input": {}},
                    },
                }),
                model_end(
                    2,
                    failure_data(serde_json::json!({"failure": failure_fact()})),
                ),
            ],
        );
        let rows = projection.snapshot().rows;
        let row = rows.iter().find(|row| row.kind == RowKind::Error).unwrap();
        assert!(
            row.text.starts_with("The response above is incomplete."),
            "{}",
            row.text
        );
    }

    #[test]
    fn the_turn_repeat_is_dropped_for_a_failure_without_presentation() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[
                model_end(
                    1,
                    failure_data(serde_json::json!({"failure": failure_fact()})),
                ),
                serde_json::json!({
                    "sequence": 2, "create_at": 2000, "type": "turn", "name": "end",
                    "data": {"turn_id": "t1", "status": "failed", "error": "fixture error"},
                }),
            ],
        );
        let rows = projection.snapshot().rows;
        let errors: Vec<_> = rows
            .iter()
            .filter(|row| row.kind == RowKind::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0]
                .text
                .starts_with("The provider reported a server error")
        );
    }

    #[test]
    fn a_turn_failure_that_is_not_a_model_failure_still_prints() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[serde_json::json!({
                "sequence": 1, "create_at": 1000, "type": "turn", "name": "end",
                "data": {"turn_id": "t1", "status": "failed", "error": "context preparation failed"},
            })],
        );
        let rows = projection.snapshot().rows;
        let row = rows.iter().find(|row| row.kind == RowKind::Error).unwrap();
        assert_eq!(row.text, "turn failed: context preparation failed");
    }

    /// Every end the command owner writes arrives as `exec/ended` with a
    /// status: a timeout, a kill and a non-zero exit are `failed`, and a stop
    /// is `cancelled`. The status is authoritative, so a stop stays stopped
    /// even though `ExecProcess::finish_background` marks its result as an
    /// error, which is the payload the owner writes.
    #[test]
    fn a_timed_out_killed_or_non_zero_exit_task_finishes_failed() {
        let started = |task: &str| {
            serde_json::json!({
                "id": format!("started-{task}"),
                "sequence": 1,
                "create_at": 1000,
                "type": "exec",
                "name": "started",
                "data": {
                    "turn_id": "turn", "request_id": "request",
                    "call_id": task, "task_id": task,
                    "description": "cargo build",
                    "output_path": "/server/private/output",
                },
            })
        };
        for (task, status, result, outcome) in [
            (
                "timed-out",
                "failed",
                serde_json::json!({"content": "Command timed out.", "isError": true}),
                TaskOutcome::Failed,
            ),
            (
                "killed",
                "failed",
                serde_json::json!({
                    "content": "Command terminated with SIGKILL.",
                    "isError": true,
                    "structuredContent": {"signal": "SIGKILL"},
                }),
                TaskOutcome::Failed,
            ),
            (
                "exit-seven",
                "failed",
                serde_json::json!({
                    "content": "Command failed with exit code 7.",
                    "isError": true,
                    "structuredContent": {"exitCode": 7},
                }),
                TaskOutcome::Failed,
            ),
            (
                "done",
                "completed",
                serde_json::json!({"content": "Command finished."}),
                TaskOutcome::Completed,
            ),
            (
                "stopped",
                "cancelled",
                serde_json::json!({
                    "content": "Command cancelled by user.",
                    "isError": true,
                    "structuredContent": {"signal": "SIGTERM"},
                }),
                TaskOutcome::Stopped,
            ),
        ] {
            let mut projection = Projection::default();
            apply(
                &mut projection,
                &[
                    started(task),
                    serde_json::json!({
                        "id": format!("ended-{task}"),
                        "sequence": 2,
                        "create_at": 2000,
                        "type": "exec",
                        "name": "ended",
                        "data": {"task_id": task, "status": status, "result": result},
                    }),
                ],
            );
            let snapshot = projection.snapshot();
            let finish = snapshot.tasks[0]
                .finished
                .as_ref()
                .expect("a finished task");
            assert_eq!(finish.outcome, outcome, "{task}: {finish:?}");
            assert_eq!(finish.at, 2000, "{task}: the end's own time");
            assert!(!finish.replayed, "{task}: the live stream carried the end");
        }
    }

    #[test]
    fn a_retry_progress_reaches_the_snapshot_and_clears() {
        let mut projection = Projection::default();
        let retrying: Progress = serde_json::from_value(serde_json::json!({
            "sessionId": "s", "branchId": "main", "requestId": "r1",
            "text": "", "textOffset": 0, "thinking": "", "thinkingOffset": 0,
            "retry": {
                "attempt": 3, "maxAttempts": 8, "retryAtMs": 1_700_000_000_000u64,
                "kind": "server_error", "reason": "The server is overloaded",
            },
        }))
        .unwrap();
        projection.progress(&retrying);
        let retry = projection.snapshot().retry.expect("a retry");
        assert_eq!((retry.attempt, retry.max_attempts), (3, 8));
        assert_eq!(retry.kind, dim_agent_types::ModelFailureKind::ServerError);
        assert_eq!(retry.reason.as_deref(), Some("The server is overloaded"));
        let done: Progress = serde_json::from_value(serde_json::json!({
            "sessionId": "s", "branchId": "main", "requestId": "r1",
            "text": "", "textOffset": 0, "thinking": "", "thinkingOffset": 0,
        }))
        .unwrap();
        projection.progress(&done);
        assert!(projection.snapshot().retry.is_none());
    }
}
