//! The `/tasks` panel: what runs apart from the turn, a background command,
//! a member agent or a Workflow run, from the session's snapshot. Command
//! details show the existing tail preview; members have a separate read-only
//! conversation; Workflow details show the owner's stored stages and logs.

use crate::model::view::{
    MemberConversationState, MemberTarget, RowKind, TaskKind, TaskOutcome, TaskOutputState,
    TaskTarget, TaskView, ViewSnapshot, WorkflowInventory, WorkflowView,
};
use crate::sanitize::safe;
use crate::ui::i18n::t;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::markdown::MarkdownRenderer;
use crate::ui::render::panel::{bordered, key_hints};
use crate::ui::rows::title_plain;
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::{truncate_cells, wrap_cells};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;
use crate::ui::transcript::block::{opens_group, project_row};
use crate::ui::transcript::tool_group::Detail as ToolDetail;
use crate::wire::{StageState, WorkflowReport, WorkflowState};
use std::cell::Cell;

const HINT: &str = "Enter details · x stop selected · Ctrl+X stop all · Esc close";

pub struct TasksPanel {
    session_id: String,
    pub projection_id: u64,
    history_incomplete: bool,
    workflow_incomplete: bool,
    workflow_stop: bool,
    /// The opened Workflow detail left the inventory.
    gone: bool,
    updated_second: u64,
    list: ListPanel,
    tasks: Vec<TaskView>,
    detail: Option<Box<Detail>>,
    /// Opened directly: leaving the detail returns to the composer.
    from_card: bool,
}

struct Detail {
    task: TaskView,
    body: DetailBody,
    scroll: Cell<usize>,
    page: Cell<usize>,
}

enum DetailBody {
    Command {
        text: String,
        state: TaskOutputState,
    },
    Member {
        snapshot: Box<ViewSnapshot>,
        state: MemberConversationState,
        /// The member's adopted model as observed now, not the task's.
        model: Option<String>,
    },
    Result,
    Workflow,
}

impl TasksPanel {
    pub fn new(snapshot: &ViewSnapshot) -> Self {
        let mut panel = Self {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            history_incomplete: false,
            workflow_incomplete: false,
            workflow_stop: false,
            gone: false,
            updated_second: 0,
            list: ListPanel::new(
                "Background tasks",
                HINT,
                "",
                "Nothing runs in the background.",
                "No matching tasks.",
                false,
            ),
            tasks: Vec::new(),
            detail: None,
            from_card: false,
        };
        panel.set_tasks(snapshot);
        panel
    }

    pub fn set_tasks(&mut self, snapshot: &ViewSnapshot) {
        let selected = self.list.selected_id();
        self.tasks = snapshot.tasks.clone();
        self.history_incomplete =
            snapshot.task_history_loading != 0 || !snapshot.task_history_errors.is_empty();
        self.workflow_incomplete = matches!(
            snapshot.workflow,
            WorkflowInventory::Loading | WorkflowInventory::Failed { .. }
        );
        self.workflow_stop = snapshot.workflow_stop;
        let mut items = Vec::new();
        if snapshot.task_history_loading != 0 {
            let mut item = Item::heading(t("Loading branch history... Stop-all unavailable"));
            item.sticky = true;
            items.push(item);
        }
        for error in &snapshot.task_history_errors {
            let mut item = Item::heading(format!(
                "{}: {}",
                t("Branch history failed; reopen to retry"),
                safe(error)
            ));
            item.sticky = true;
            items.push(item);
        }
        let workflow = match &snapshot.workflow {
            WorkflowInventory::Absent | WorkflowInventory::Ready => None,
            WorkflowInventory::Loading => {
                Some(t("Loading Workflow runs... Stop-all unavailable").to_owned())
            }
            WorkflowInventory::Failed {
                error,
                observed: true,
            } => Some(format!(
                "{}: {}",
                t("Workflow: last observed; refresh failed, reopen to retry"),
                safe(error)
            )),
            WorkflowInventory::Failed {
                error,
                observed: false,
            } => Some(format!(
                "{}: {}",
                t("Workflow runs unavailable; reopen to retry"),
                safe(error)
            )),
        };
        let gone = self
            .gone
            .then(|| t("The selected Workflow run is no longer available").to_owned());
        for text in workflow.into_iter().chain(gone) {
            let mut item = Item::heading(text);
            item.sticky = true;
            items.push(item);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.updated_second = now / 1000;
        items.extend(self.tasks.iter().map(|task| {
            let kind = match &task.kind {
                TaskKind::Command(_) => t("command"),
                TaskKind::Member(_) => t("agent"),
                TaskKind::Workflow(_) => t("workflow"),
            };
            let mut status = task
                .finished
                .as_ref()
                .map_or_else(
                    || match &task.kind {
                        TaskKind::Member(MemberTarget::Starting) => t("Starting"),
                        TaskKind::Workflow(view) if view.run.state == WorkflowState::Stopping => {
                            t("Stopping")
                        }
                        _ => t("Running"),
                    },
                    |finish| t(finish.status),
                )
                .to_owned();
            if let TaskKind::Workflow(view) = &task.kind
                && view.unavailable
            {
                status = format!("{status} · {}", t("last observed"));
            }
            let elapsed = task
                .finished
                .as_ref()
                .map_or(now, |finish| finish.at)
                .saturating_sub(task.started_at)
                / 1000;
            let exit = task
                .finished
                .as_ref()
                .and_then(|finish| finish.exit_code)
                .map_or(String::new(), |code| format!(" · exit {code}"));
            let agent = task
                .agent
                .as_deref()
                .map_or(String::new(), |name| format!(" · {}", safe(name)));
            Item::new(
                task.id.clone(),
                safe(&task.label),
                format!("{kind}{agent} · {status} · {elapsed}s{exit}"),
            )
        }));
        self.list.set_items(Ok(items), selected.as_deref());
        if let Some(detail) = &self.detail
            && matches!(detail.body, DetailBody::Workflow)
            && !self
                .tasks
                .iter()
                .any(|task| matches!(task.kind, TaskKind::Workflow(_)) && task.id == detail.task.id)
        {
            self.detail = None;
            self.gone = true;
            self.set_tasks(snapshot);
            return;
        }
        if let Some(detail) = &mut self.detail
            && let Some(task) = self.tasks.iter().find(|task| {
                task.id == detail.task.id
                    && (task.branch_id == detail.task.branch_id
                        || matches!(task.kind, TaskKind::Workflow(_)))
            })
        {
            detail.task = task.clone();
            if let DetailBody::Command { text, state } = &mut detail.body
                && let Some(finish) = &task.finished
            {
                *text = finish.text.clone();
                *state = TaskOutputState::Finished {
                    status: finish.status,
                    outcome: finish.outcome,
                };
            }
        }
    }

    pub fn tick(&mut self, snapshot: &ViewSnapshot) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now != self.updated_second && self.tasks.iter().any(|task| task.finished.is_none()) {
            self.set_tasks(snapshot);
            true
        } else {
            false
        }
    }

    fn stop(&self, task: &TaskView) -> Effect {
        if task.finished.is_some() {
            return Effect::None;
        }
        match &task.kind {
            TaskKind::Command(target) => Effect::StopTask {
                session_id: self.session_id.clone(),
                projection_id: self.projection_id,
                target: target.clone(),
            },
            TaskKind::Member(MemberTarget::Session(member_id)) => Effect::StopMember {
                session_id: self.session_id.clone(),
                projection_id: self.projection_id,
                member_id: member_id.clone(),
            },
            TaskKind::Member(MemberTarget::Starting) => {
                Effect::Notice(t("Agent is starting; no member Session to stop yet").into())
            }
            TaskKind::Member(MemberTarget::Provider) => Effect::Notice(
                t("Provider members are controlled by the leader; no stop here").into(),
            ),
            TaskKind::Workflow(view) => match self.workflow_stoppable(view) {
                Ok(()) => Effect::StopWorkflow {
                    session_id: self.session_id.clone(),
                    projection_id: self.projection_id,
                    run_id: view.run.run_id.clone(),
                },
                Err(Some(notice)) => Effect::Notice(notice.into()),
                Err(None) => Effect::None,
            },
        }
    }

    /// Only a running run without a pending stop can be stopped again.
    fn workflow_stoppable(&self, view: &WorkflowView) -> Result<(), Option<&'static str>> {
        if !self.workflow_stop {
            Err(Some(t("Workflow stop unavailable")))
        } else if view.run.state == WorkflowState::Running && !view.stop_pending {
            Ok(())
        } else {
            Err(None)
        }
    }

    /// A paste edits the visible list filter.
    pub fn paste(&mut self, text: &str) {
        if self.detail.is_none() {
            self.list.paste(text);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        self.gone = false;
        if let Some(detail) = &self.detail {
            if key.code == KeyCode::Char('x') && key.mods == Modifiers::NONE {
                if matches!(
                    &detail.body,
                    DetailBody::Member {
                        state: MemberConversationState::Closed,
                        ..
                    }
                ) || detail.task.finished.is_some()
                {
                    return Effect::None;
                }
                return self.stop(&detail.task);
            }
            match key.code {
                KeyCode::Esc if self.from_card => return Effect::Close,
                KeyCode::Esc => self.detail = None,
                KeyCode::Up => detail.scroll.set(detail.scroll.get().saturating_add(1)),
                KeyCode::Down => detail.scroll.set(detail.scroll.get().saturating_sub(1)),
                KeyCode::PageUp => detail
                    .scroll
                    .set(detail.scroll.get().saturating_add(detail.page.get())),
                KeyCode::PageDown => detail
                    .scroll
                    .set(detail.scroll.get().saturating_sub(detail.page.get())),
                KeyCode::Home => detail.scroll.set(usize::MAX),
                KeyCode::End => detail.scroll.set(0),
                _ => {}
            }
            return Effect::None;
        }
        if key.code == KeyCode::Char('x') && key.mods == Modifiers::CTRL {
            return if self.history_incomplete {
                Effect::Notice(t("Branch history is incomplete; stop-all unavailable").into())
            } else if self.workflow_incomplete {
                Effect::Notice(t("Workflow inventory is incomplete; stop-all unavailable").into())
            } else {
                Effect::StopTasks {
                    session_id: self.session_id.clone(),
                    projection_id: self.projection_id,
                }
            };
        }
        if key.code == KeyCode::Char('x')
            && key.mods == Modifiers::NONE
            && self.list.filter_is_empty()
            && let Some(id) = self.list.selected_id()
            && let Some(task) = self.tasks.iter().find(|task| task.id == id)
        {
            return self.stop(task);
        }
        match self.list.handle_key(key) {
            Event::None | Event::Refresh => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick { id, .. } => {
                let Some(task) = self.tasks.iter().find(|task| task.id == id).cloned() else {
                    return Effect::None;
                };
                self.open(task)
            }
        }
    }

    /// Opens a transcript card or background row directly, without the list.
    pub fn open_card(&mut self, task: TaskView) -> Effect {
        self.from_card = true;
        self.open(task)
    }

    fn open(&mut self, task: TaskView) -> Effect {
        let body = match &task.kind {
            TaskKind::Command(_) => DetailBody::Command {
                text: task
                    .finished
                    .as_ref()
                    .map_or(String::new(), |finish| finish.text.clone()),
                state: task
                    .finished
                    .as_ref()
                    .map_or(TaskOutputState::Loading, |finish| {
                        TaskOutputState::Finished {
                            status: finish.status,
                            outcome: finish.outcome,
                        }
                    }),
            },
            TaskKind::Member(MemberTarget::Session(_)) => DetailBody::Member {
                snapshot: Box::default(),
                state: MemberConversationState::Loading,
                model: None,
            },
            TaskKind::Member(_) if task.finished.is_some() => DetailBody::Result,
            TaskKind::Member(MemberTarget::Starting) => {
                return Effect::Notice(
                    t("Agent is starting; no member Session to stop yet").into(),
                );
            }
            TaskKind::Member(MemberTarget::Provider) => {
                return Effect::Notice(
                    t("Provider members have no conversation to open here").into(),
                );
            }
            TaskKind::Workflow(_) => DetailBody::Workflow,
        };
        self.detail = Some(Box::new(Detail {
            task,
            body,
            scroll: Cell::new(0),
            page: Cell::new(1),
        }));
        Effect::None
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        if self.detail.is_some() {
            None
        } else {
            self.list.cursor(width)
        }
    }

    pub fn output_target(&self) -> Option<&TaskTarget> {
        let detail = self.detail.as_ref()?;
        match (&detail.task.kind, &detail.body) {
            (TaskKind::Command(target), DetailBody::Command { state, .. })
                if !matches!(state, TaskOutputState::Finished { .. }) =>
            {
                Some(target)
            }
            _ => None,
        }
    }

    pub fn member_target(&self) -> Option<&str> {
        let detail = self.detail.as_ref()?;
        match (&detail.task.kind, &detail.body) {
            (TaskKind::Member(MemberTarget::Session(id)), DetailBody::Member { .. }) => Some(id),
            _ => None,
        }
    }

    pub fn compaction_targets(&self) -> Vec<crate::model::view::CompactionTarget> {
        let Some(detail) = &self.detail else {
            return Vec::new();
        };
        let (
            TaskKind::Member(MemberTarget::Session(session_id)),
            DetailBody::Member { snapshot, .. },
        ) = (&detail.task.kind, &detail.body)
        else {
            return Vec::new();
        };
        snapshot
            .rows
            .iter()
            .filter_map(|row| {
                let detail = row.compaction.as_ref()?.detail.as_ref()?;
                Some(crate::model::view::CompactionTarget {
                    session_id: session_id.clone(),
                    branch_id: "main".into(),
                    event_id: detail.event_id.clone(),
                    sequence: detail.sequence,
                })
            })
            .collect()
    }

    pub fn output(&mut self, target: &TaskTarget, text: Option<String>, state: TaskOutputState) {
        if let Some(Detail {
            task:
                TaskView {
                    kind: TaskKind::Command(at),
                    ..
                },
            body:
                DetailBody::Command {
                    text: current,
                    state: status,
                },
            ..
        }) = self.detail.as_deref_mut()
            && at == target
        {
            if let Some(text) = text {
                *current = text;
            }
            *status = state;
        }
    }

    pub fn member(
        &mut self,
        session_id: &str,
        snapshot: Box<ViewSnapshot>,
        state: MemberConversationState,
        model: Option<String>,
    ) {
        if let Some(Detail {
            task:
                TaskView {
                    kind: TaskKind::Member(MemberTarget::Session(id)),
                    ..
                },
            body:
                DetailBody::Member {
                    snapshot: current,
                    state: status,
                    model: observed,
                },
            ..
        }) = self.detail.as_deref_mut()
            && id == session_id
        {
            *current = snapshot;
            *status = state;
            *observed = model;
        }
    }

    pub fn render(
        &self,
        width: usize,
        rows: usize,
        p: &Palette,
        markdown: &MarkdownRenderer,
    ) -> Vec<String> {
        let Some(detail) = &self.detail else {
            return self.list.render(width, rows, p);
        };
        let inner = width.saturating_sub(4).max(1);
        let mut content = Vec::new();
        let mut lines = Vec::new();
        let mut hint = "Esc back";
        let mut title = safe(&detail.task.label);
        if let Some(agent) = &detail.task.agent {
            title.push_str(&format!(" · {}", safe(agent)));
        }
        match &detail.body {
            DetailBody::Command { text, state } => {
                let (status, color) = match state {
                    TaskOutputState::Loading | TaskOutputState::Live => (
                        t("Tail preview - last 2,000 lines / 50 KiB").to_owned(),
                        &p.muted,
                    ),
                    TaskOutputState::Unavailable => {
                        (t("Live output unavailable").to_owned(), &p.muted)
                    }
                    TaskOutputState::Unsupported => (
                        t("Live output unavailable in this profile").to_owned(),
                        &p.muted,
                    ),
                    TaskOutputState::ReadError(error) => (
                        format!("{}: {}", t("Output read failed"), safe(error)),
                        &p.danger,
                    ),
                    TaskOutputState::Finished { status, outcome } => (
                        t(status).to_owned(),
                        if *outcome == TaskOutcome::Failed {
                            &p.danger
                        } else {
                            &p.muted
                        },
                    ),
                };
                content.push(fg(&truncate_cells(&status, inner), color));
                if let Some(command) = &detail.task.command {
                    lines.extend(
                        wrap_cells(&safe(command), inner)
                            .into_iter()
                            .map(|line| fg(&line, &p.muted)),
                    );
                    lines.push(String::new());
                }
                let text = if text.is_empty() {
                    match state {
                        TaskOutputState::Loading => t("Loading output..."),
                        TaskOutputState::Live => t("No output yet"),
                        _ => "",
                    }
                } else {
                    text
                };
                lines.extend(
                    wrap_cells(&safe(text), inner)
                        .into_iter()
                        .map(|line| fg(&line, &p.text)),
                );
                if detail.task.finished.is_none() {
                    hint = "x stop command · Esc back";
                }
            }
            DetailBody::Member {
                snapshot,
                state,
                model,
            } => {
                title = format!("{} - {}", title, t("read-only"));
                let TaskKind::Member(MemberTarget::Session(id)) = &detail.task.kind else {
                    unreachable!("member detail has a delivered identity")
                };
                content.push(fg(
                    &truncate_cells(&format!("{}: {}", t("Member Session"), safe(id)), inner),
                    &p.muted,
                ));
                let model = model
                    .as_deref()
                    .map_or_else(|| t("No model observed").to_owned(), safe);
                content.push(fg(
                    &truncate_cells(&format!("{}: {model}", t("Member model")), inner),
                    &p.muted,
                ));
                let status = match state {
                    MemberConversationState::Loading => t("Loading conversation...").to_owned(),
                    MemberConversationState::Live if snapshot.rows.is_empty() => {
                        t("No conversation yet").to_owned()
                    }
                    MemberConversationState::Live if snapshot.running => t("Running").to_owned(),
                    MemberConversationState::Live => t("Observing conversation").to_owned(),
                    MemberConversationState::ReadError(error) => {
                        format!("{}: {}", t("Conversation unavailable"), safe(error))
                    }
                    MemberConversationState::Closed => t("Session closed").to_owned(),
                };
                content.push(fg(
                    &truncate_cells(&status, inner),
                    if matches!(state, MemberConversationState::ReadError(_)) {
                        &p.danger
                    } else {
                        &p.muted
                    },
                ));
                lines.push(fg(t("Member conversation"), &p.muted));
                for (index, row) in snapshot.rows.iter().enumerate() {
                    if row.kind == RowKind::ToolCall && !opens_group(&snapshot.rows, index) {
                        continue;
                    }
                    let block = project_row(&snapshot.rows, index, ToolDetail::Expanded);
                    lines.extend(crate::ui::transcript::blocks::render_block(
                        &block, inner, p, markdown, false,
                    ));
                    lines.push(String::new());
                }
                if let Some(finish) = &detail.task.finished {
                    lines.push(fg(
                        &format!("{}: {}", t("Delegation result"), t(finish.status)),
                        &p.muted,
                    ));
                    lines.extend(
                        wrap_cells(&safe(&finish.text), inner)
                            .into_iter()
                            .map(|line| fg(&line, &p.text)),
                    );
                    lines.push(String::new());
                }
                if detail.task.finished.is_none()
                    && !matches!(state, MemberConversationState::Closed)
                {
                    hint = "x interrupt member · Esc back";
                }
            }
            DetailBody::Workflow => {
                let TaskKind::Workflow(view) = &detail.task.kind else {
                    unreachable!("workflow detail has a run")
                };
                title = format!("{title} - {}", t("workflow"));
                content.extend(
                    wrap_cells(
                        &format!(
                            "{}: {} · {}: {} · {}",
                            t("Run"),
                            safe(&view.run.run_id),
                            t("Source"),
                            safe(&view.run.source),
                            t(workflow_state(view.run.state)),
                        ),
                        inner,
                    )
                    .into_iter()
                    .map(|line| fg(&line, &p.muted)),
                );
                if view.unavailable {
                    content.push(fg(
                        &truncate_cells(t("Run no longer available; last observed record"), inner),
                        &p.danger,
                    ));
                }
                lines.extend(workflow_lines(view, inner, p));
                if self.workflow_stoppable(view).is_ok() {
                    hint = "x stop workflow · Esc back";
                }
            }
            DetailBody::Result => {
                let finish = detail
                    .task
                    .finished
                    .as_ref()
                    .expect("a delivery result is terminal");
                title = format!("{}: {title}", t("Delegation result"));
                content.push(fg(
                    t(finish.status),
                    if finish.failed() { &p.danger } else { &p.muted },
                ));
                lines.extend(
                    wrap_cells(&safe(&finish.text), inner)
                        .into_iter()
                        .map(|line| fg(&line, &p.text)),
                );
            }
        }
        let page = rows.saturating_sub(content.len() + 2).max(1);
        detail.page.set(page);
        let scroll = detail.scroll.get().min(lines.len().saturating_sub(page));
        detail.scroll.set(scroll);
        let end = lines.len().saturating_sub(scroll);
        content.extend(lines[end.saturating_sub(page)..end].iter().cloned());
        bordered(
            &content,
            width,
            Some(rows),
            &p.border,
            Some(&title_plain(&title, inner, p)),
            Some(&key_hints(t(hint), p)),
        )
    }
}

impl TasksPanel {
    /// Whether the panel shows a member's conversation: fullscreen draws it
    /// over the whole screen with [`TasksPanel::render_member_screen`].
    pub fn shows_member(&self) -> bool {
        matches!(
            self.detail.as_deref(),
            Some(Detail {
                body: DetailBody::Member { .. },
                ..
            })
        )
    }

    /// The member conversation as fullscreen draws it: the member's rows at
    /// the transcript's detail setting under a boundary naming the task, and
    /// a read-only bar in the composer's place listing the keys it accepts.
    /// `width` and `rows` are the whole terminal.
    pub fn render_member_screen(
        &self,
        width: usize,
        rows: usize,
        p: &Palette,
        markdown: &MarkdownRenderer,
        tool_detail: ToolDetail,
    ) -> Vec<String> {
        let Some(Detail {
            task,
            body:
                DetailBody::Member {
                    snapshot,
                    state,
                    model,
                },
            scroll,
            page,
            ..
        }) = self.detail.as_deref()
        else {
            unreachable!("shows_member")
        };
        let content = width.saturating_sub(2).max(1);
        let stoppable =
            task.finished.is_none() && !matches!(state, MemberConversationState::Closed);
        let status = match (state, &task.finished) {
            (MemberConversationState::ReadError(error), _) => {
                format!("{}: {}", t("Conversation unavailable"), safe(error))
            }
            (MemberConversationState::Closed, _) => t("Session closed").to_owned(),
            (_, Some(finish)) => t(finish.status).to_owned(),
            (MemberConversationState::Loading, None) => t("Loading conversation...").to_owned(),
            (MemberConversationState::Live, None) if snapshot.running => t("Running").to_owned(),
            (MemberConversationState::Live, None) => t("Observing conversation").to_owned(),
        };
        let agent = task.agent.as_deref().map_or_else(String::new, safe);
        let model = model
            .as_deref()
            .map_or_else(|| t("No model observed").to_owned(), safe);
        let note = if stoppable {
            t("Read-only: you can watch this agent or stop it.")
        } else {
            t("Read-only: this agent has finished.")
        };
        let keys = if stoppable {
            t("Esc back · x stop agent · ↑↓ PgUp PgDn scroll · Home/End jump")
        } else {
            t("Esc back · ↑↓ PgUp PgDn scroll · Home/End jump")
        };
        let bar = bordered(
            &[
                fg(&truncate_cells(note, content), &p.muted),
                key_hints(&truncate_cells(keys, content), p),
            ],
            width,
            None,
            &p.border,
            Some(&title_plain(&format!("{agent} · {model}"), content, p)),
            Some(&fg(&truncate_cells(&status, content), &p.muted)),
        );
        let mut title = safe(&task.label);
        if !agent.is_empty() {
            title.push_str(&format!(" · {agent}"));
        }
        title.push_str(&format!(" · {}", t("read-only")));
        let mut lines = Vec::new();
        if snapshot.rows.is_empty() {
            lines.push(fg(
                match state {
                    MemberConversationState::Loading => t("Loading conversation..."),
                    _ => t("No conversation yet"),
                },
                &p.muted,
            ));
        }
        for (index, row) in snapshot.rows.iter().enumerate() {
            if row.kind == RowKind::ToolCall && !opens_group(&snapshot.rows, index) {
                continue;
            }
            let block = project_row(&snapshot.rows, index, tool_detail);
            lines.extend(crate::ui::transcript::blocks::render_block(
                &block, content, p, markdown, false,
            ));
            lines.push(String::new());
        }
        let view = rows.saturating_sub(bar.len() + 1).max(1);
        page.set(view);
        let offset = scroll.get().min(lines.len().saturating_sub(view));
        scroll.set(offset);
        let end = lines.len().saturating_sub(offset);
        let mut screen = vec![fg(
            &crate::ui::transcript::blocks::boundary_line(&title, width),
            &p.muted,
        )];
        screen.extend(
            lines[end.saturating_sub(view)..end]
                .iter()
                .map(|line| format!(" {line}")),
        );
        screen.resize(rows.saturating_sub(bar.len()), String::new());
        screen.extend(bar);
        screen.truncate(rows);
        screen
    }
}

fn workflow_state(state: WorkflowState) -> &'static str {
    match state {
        WorkflowState::Running => "Running",
        WorkflowState::Stopping => "Stopping",
        WorkflowState::Completed => "Completed",
        WorkflowState::Failed => "Failed",
        WorkflowState::Stopped => "Stopped",
        WorkflowState::Interrupted => "Interrupted",
    }
}

fn stage_state(state: StageState) -> &'static str {
    match state {
        StageState::Waiting => "waiting",
        StageState::Launching => "launching",
        StageState::Running => "running",
        StageState::Completed => "completed",
        StageState::Failed => "failed",
        StageState::Stopped => "stopped",
        StageState::Skipped => "skipped",
        StageState::Lost => "lost",
    }
}

/// The stored stages, their results, the log and the report settlement.
fn workflow_lines(view: &WorkflowView, width: usize, p: &Palette) -> Vec<String> {
    let run = &view.run;
    let text = |text: &str| {
        wrap_cells(&safe(text), width)
            .into_iter()
            .map(|line| fg(&line, &p.text))
    };
    let mut lines = vec![fg(t("Stages"), &p.muted)];
    for stage in &run.stages {
        lines.extend(text(&format!(
            "{}  {}  {}  {}",
            stage.step + 1,
            stage.id,
            t(stage_state(stage.state)),
            stage.session_id.as_deref().unwrap_or("-"),
        )));
    }
    for stage in &run.stages {
        if let Some(result) = stage.result_text.as_ref().or(stage.reason.as_ref()) {
            lines.push(String::new());
            lines.push(fg(
                &truncate_cells(&format!("{} {}", safe(&stage.id), t("result")), width),
                &p.muted,
            ));
            lines.extend(text(result));
        }
    }
    lines.push(String::new());
    lines.push(fg(t("Log"), &p.muted));
    lines.extend(run.log.iter().flat_map(|entry| text(&entry.message)));
    lines.push(String::new());
    let report = match &run.report {
        WorkflowReport::None {} => t("none").to_owned(),
        WorkflowReport::Pending {} => t("pending").to_owned(),
        WorkflowReport::Delivered {} => t("delivered").to_owned(),
        WorkflowReport::Undeliverable { reason } => {
            format!("{}: {reason}", t("undeliverable"))
        }
    };
    lines.extend(text(&format!("{}: {report}", t("Report"))));
    lines
}
