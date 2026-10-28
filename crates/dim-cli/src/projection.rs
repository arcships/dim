//! Folds App Server session events into the transcript the screen draws.
//!
//! The worker applies every event once, in sequence order, and publishes
//! [`ViewSnapshot`]s built here. Only the rows the screen needs are kept; the
//! durable history stays with the App Server.

use std::collections::HashMap;
use std::sync::Arc;

use dim_agent_types::{
    BranchCreateFailedData, BranchSource, CollaborationDelivery, CollaborationOutcome,
    CollaborationTarget, ContentBlock, ExecStatus, InputMode, JsonObject, ModelOutputOutcome,
    ToolContent, ToolResult, TurnOutcome, UserInput,
};
use serde_json::Value;

use crate::model::shell::ShellRun;
use crate::model::view::{
    ApprovalView, LineState, LineView, MemberCard, MemberConversationState, MemberTarget,
    QueuedInput, RowKind, RowTone, TaskFinish, TaskKind, TaskTarget, TaskView, TurnEnd, ViewRow,
    ViewSnapshot, WorkflowView,
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
    running: bool,
    approvals: Vec<ApprovalView>,
    pub questions: crate::question::Pending,
    /// Messages InputQueue still holds: submitted while a turn ran and not
    /// applied yet. Shown in the queue strip; each enters the transcript
    /// when its `input/applied` comes, so the transcript keeps the order
    /// the model sees. Withdrawal re-submits the kept `UserInput` as Steer.
    held: Vec<(String, UserInput)>,
    /// Unfinished tasks and the latest 50 terminal results.
    tasks: Vec<TaskView>,
    /// The `exec` call each background task came from, until it ends.
    background: HashMap<TaskTarget, String>,
    /// The newest turn end: its sequence and how the turn ended.
    ended: Option<(u64, TurnEnd)>,
    /// The call row carrying each delegation's card, by request event id.
    cards: HashMap<String, usize>,
    /// Native Agent names from `agent.list`, by Agent id.
    agent_names: HashMap<String, String>,
    /// Agent ids a finished `agent.list` read was asked to name, whether or
    /// not it returned them: only a card naming another id reads again.
    agents_read: std::collections::HashSet<String>,
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
            EventContent::BranchCreate(data) => {
                let purpose = data
                    .prompt
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if !self.lines.iter().any(|line| line.id == data.branch) {
                    self.lines.push(LineView {
                        id: data.branch.clone(),
                        parent: Some(parent.to_owned()),
                        purpose,
                        state: LineState::Waiting,
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
        let sequence = event.sequence;
        match &event.content {
            EventContent::Input(data) => {
                if let Some(user) = &data.user {
                    let held = self.running && data.mode != Some(InputMode::Append);
                    if held {
                        self.held.push((event.id.clone(), user.clone()));
                    } else {
                        self.push_user(sequence, user);
                    }
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
                if let Some(index) = self.held.iter().position(|(id, _)| *id == data.input_id) {
                    let (_, user) = self.held.remove(index);
                    self.push_user(sequence, &user);
                }
            }
            EventContent::InputDeleted(data) => {
                self.held.retain(|(id, _)| *id != data.input_id);
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
            EventContent::ModelEnd(data) => {
                self.close_thinking(&data.request_id);
                self.close_assistant(&data.request_id);
                self.request_starts.remove(&data.request_id);
                if let ModelOutputOutcome::Failed { error, .. } = &data.outcome {
                    self.push_error(format!("model failed: {error}"));
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
            EventContent::Configuration(_)
            | EventContent::ToolCall(_)
            | EventContent::BranchStart(_)
            | EventContent::BranchCreate(_)
            | EventContent::BranchCreateFailed(_)
            | EventContent::BranchEnded(_)
            | EventContent::PermissionChanged(_)
            | EventContent::InputConsumed(_)
            | EventContent::InputWithdraw(_)
            | EventContent::TurnStart(_)
            | EventContent::ModelUsage(_)
            | EventContent::ToolUseStart(_)
            | EventContent::ToolStart(_)
            | EventContent::CompactionStart(_)
            | EventContent::Compaction(_)
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
        let end = match outcome {
            TurnOutcome::Failed { error } => {
                self.push_error(format!("turn failed: {error}"));
                TurnEnd::Failed
            }
            TurnOutcome::Cancelled { .. } => {
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
    fn push_user(&mut self, sequence: u64, user: &UserInput) {
        let text = text_of(&user.content);
        let kind = if ShellRun::parse(&text).is_some() {
            RowKind::Shell
        } else {
            RowKind::UserMessage
        };
        self.push(format!("user-{sequence}"), kind, text);
    }

    /// The content of a held message, to re-submit it as Steer.
    pub fn held_input(&self, input_id: &str) -> Option<&UserInput> {
        self.held
            .iter()
            .find(|(id, _)| id == input_id)
            .map(|(_, user)| user)
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
        if let Some(index) = self.tool_rows.get(&request_id).copied() {
            self.row(index).parked = true;
        }
        self.approvals.push(ApprovalView {
            request_id,
            branch_id,
            reason,
            tool_name: name,
            tool_input: Value::Object(input),
            always,
        });
    }

    /// The command an "always" answer to `request_id` saves a rule for.
    pub fn approval_always(&self, request_id: &str) -> Option<String> {
        self.approvals
            .iter()
            .find(|approval| approval.request_id == request_id)?
            .always
            .clone()
    }

    pub fn remove_approval(&mut self, request_id: &str) {
        self.approvals
            .retain(|approval| approval.request_id != request_id);
    }

    pub fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            rows: self.rows.clone(),
            running: self.running,
            approvals: self.approvals.clone(),
            questions: self.questions.calls().cloned().collect(),
            queued: self
                .held
                .iter()
                .map(|(id, user)| QueuedInput {
                    id: id.clone(),
                    text: text_of(&user.content),
                })
                .collect(),
            tasks: self.tasks.clone(),
            ended: self.ended,
            ..ViewSnapshot::default()
        }
    }

    pub fn tasks(&self) -> &[TaskView] {
        &self.tasks
    }

    /// The first message the user sent: the window title's topic.
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
        apply(
            &mut main,
            &[create(4, "b-7c", "parser race hypothesis\nrest")],
        );
        assert_eq!(
            main.line_views(),
            &[LineView {
                id: "b-7c".into(),
                parent: Some("main".into()),
                purpose: "parser race hypothesis".into(),
                state: LineState::Waiting,
            }]
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
    fn snapshots_share_rows_and_a_delta_copies_only_its_own() {
        let mut projection = Projection::default();
        apply(
            &mut projection,
            &[step(1, "thinking", "plan"), step(2, "text", "Hel")],
        );
        let before = projection.snapshot().rows;
        apply(&mut projection, &[step(3, "text", "lo")]);
        let after = projection.snapshot().rows;
        assert!(Arc::ptr_eq(&before[0], &after[0]));
        assert!(!Arc::ptr_eq(&before[1], &after[1]));
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
}
