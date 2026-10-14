//! Runtime prompts of the scrollback interface: the exit confirmation, tool
//! permission requests and AskUser questions (ports of `prompts/model.ts`,
//! `prompts/view.ts` and the overlay state machines they sit on).
//!
//! Each prompt is a small state value with a key handler returning an
//! [`Effect`]; the app owns the side effects (RPCs, notices, exit).

use std::collections::BTreeMap;

use crate::view::{Answer, Answers, Question, QuestionKind};
use serde_json::Value;

use crate::render::panel::{
    IDLE_MARKER, SELECTED_MARKER, bordered, choices, confirm_panel, visible_width,
};
use crate::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::theme::Palette;
use crate::theme::ansi::{Paint, fg, paint};
use crate::transcript::blocks::{display_width, truncate_cells, wrap_cells};

/// What a key did to a prompt.
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// Nothing for the app to do (the state may have changed).
    None,
    /// The prompt wants to leave without answering anything.
    Close,
    Exit,
    /// Answer an approval request. `audit` is the transcript note.
    Approval {
        request_id: String,
        branch_id: String,
        allow: bool,
        remember: bool,
        audit: String,
    },
    Answers {
        request_id: String,
        branch_id: String,
        answers: Answers,
    },
    Cancel {
        request_id: String,
        branch_id: String,
        reason: String,
    },
    Notice(String),
    /// Switch to the stored session with this id.
    Resume(String),
    /// Reload the session list.
    Refresh,
    /// Set the default tool-approval mode.
    SetApprovals(String),
    /// Paint with this theme without saving it.
    ThemePreview(String),
    /// Keep this theme and save it.
    ThemeApply(String),
    /// Leave the picker and paint with this theme again.
    ThemeRestore(String),
    /// Change a provider (`/connect`).
    Provider(crate::session_host::ProviderAction),
    /// Begin a sign-in on the worker.
    OauthStart(crate::session_host::OauthStart),
    /// Abandon the sign-in in progress.
    OauthCancel,
    /// Leave the OAuth step for the provider list (panel-internal).
    OauthBack,
    /// A callback URL the user pasted.
    OauthPaste(String),
    /// A signed-in provider: close the panel and offer its models.
    Connected(String),
    /// Commit this text to the transcript as a status block.
    Print(String),
    /// Make this model the default and the session's model.
    SelectModel {
        provider_id: String,
        model_id: String,
    },
}

fn plain(key: KeyEvent) -> bool {
    key.mods.0 & !Modifiers::SHIFT.0 == 0
}

fn collapse(text: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for c in text.chars() {
        let c = if matches!(c, '\r' | '\n' | '\t') {
            ' '
        } else {
            c
        };
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

// ---- exit confirmation ------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    AgentRunning,
    AskUserPending,
    PermissionPending,
    Background,
}

impl ExitReason {
    fn text(self) -> &'static str {
        match self {
            Self::AgentRunning => "The agent is still running.",
            Self::AskUserPending => "AskUser is waiting for an answer.",
            Self::PermissionPending => "A permission request is waiting.",
            Self::Background => "Background work may still be active.",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExitConfirm {
    pub reason: ExitReason,
    /// 0 = exit, 1 = stay (the default).
    pub focused: usize,
}

impl ExitConfirm {
    pub fn new(reason: ExitReason) -> Self {
        Self { reason, focused: 1 }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') if plain(key) => Effect::Exit,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc if plain(key) => Effect::Close,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.focused = 1 - self.focused;
                Effect::None
            }
            KeyCode::Enter => {
                if self.focused == 0 {
                    Effect::Exit
                } else {
                    Effect::Close
                }
            }
            _ => Effect::None,
        }
    }

    pub fn render(&self, width: usize, p: &Palette) -> Vec<String> {
        confirm_panel(
            "Exit dimcode?",
            &[self.reason.text().to_owned()],
            &["Exit".to_owned(), "Stay".to_owned()],
            self.focused,
            "y exit · n stay · ←→ move · enter confirm",
            width,
            p,
        )
    }
}

// ---- permission -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    AllowOnce,
    AllowAlways,
    Reject,
    ApproveForMe,
    FullAccess,
}

impl PermissionAction {
    fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allow once",
            Self::AllowAlways => "Allow always",
            Self::Reject => "Reject",
            Self::ApproveForMe => "Approve for me",
            Self::FullAccess => "Full access",
        }
    }
}

/// Decisions for this call first, then the two session-wide switches (the
/// loudest last).
const ACTIONS: [PermissionAction; 5] = [
    PermissionAction::AllowOnce,
    PermissionAction::AllowAlways,
    PermissionAction::Reject,
    PermissionAction::ApproveForMe,
    PermissionAction::FullAccess,
];

#[derive(Debug, Clone)]
pub struct PermissionPrompt {
    pub request_id: String,
    pub branch_id: String,
    pub reason: String,
    pub tool_name: String,
    pub input: Value,
    pub selected: usize,
}

impl PermissionPrompt {
    pub fn new(
        request_id: String,
        branch_id: String,
        reason: String,
        tool_name: String,
        input: Value,
    ) -> Self {
        Self {
            request_id,
            branch_id,
            reason,
            tool_name,
            input,
            selected: 0,
        }
    }

    fn subject(&self) -> String {
        collapse(&self.tool_name)
    }

    fn command_preview(&self) -> String {
        let field = |key: &str| self.input.get(key);
        match field("command").or_else(|| field("cmd")) {
            Some(Value::String(text)) => return collapse(text),
            Some(Value::Array(parts)) => {
                return parts
                    .iter()
                    .map(|p| match p {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            _ => {}
        }
        if self.input.is_null() {
            return self.tool_name.clone();
        }
        format!("{} {}", self.tool_name, self.input)
    }

    fn lines(&self) -> Vec<String> {
        let description = if self.reason.trim().is_empty() {
            format!("Run {}", self.tool_name)
        } else {
            collapse(&self.reason)
        };
        vec![
            format!("# {description}"),
            format!("$ {}", self.command_preview()),
        ]
    }

    fn decide(&self, allow: bool, remember: bool) -> Effect {
        let audit = match (allow, remember) {
            (true, false) => format!("approved once: {}", self.subject()),
            (true, true) => format!("approved always: {}", self.subject()),
            _ => format!("rejected: {}", self.subject()),
        };
        Effect::Approval {
            request_id: self.request_id.clone(),
            branch_id: self.branch_id.clone(),
            allow,
            remember,
            audit,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') if plain(key) => self.decide(true, false),
            KeyCode::Char('n') | KeyCode::Char('N') if plain(key) => self.decide(false, false),
            KeyCode::Right | KeyCode::Tab => {
                self.selected = (self.selected + 1) % ACTIONS.len();
                Effect::None
            }
            KeyCode::Left | KeyCode::BackTab => {
                self.selected = (self.selected + ACTIONS.len() - 1) % ACTIONS.len();
                Effect::None
            }
            KeyCode::Enter => match ACTIONS[self.selected] {
                PermissionAction::AllowOnce => self.decide(true, false),
                PermissionAction::AllowAlways => self.decide(true, true),
                PermissionAction::Reject => self.decide(false, false),
                PermissionAction::ApproveForMe | PermissionAction::FullAccess => Effect::Notice(
                    "Changing the approvals mode from here is not yet available".to_owned(),
                ),
            },
            _ => Effect::None,
        }
    }

    pub fn render(&self, width: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let mut content = vec![paint(
            &truncate_cells("Permission required", inner),
            Paint {
                fg: Some(&p.warning),
                bold: true,
                ..Paint::default()
            },
        )];
        content.push(String::new());
        for line in self.lines() {
            let text = truncate_cells(&line, inner);
            let text = if text.is_empty() {
                " ".to_owned()
            } else {
                text
            };
            let color = if line.starts_with("# ") {
                &p.muted
            } else {
                &p.text
            };
            content.push(fg(&text, color));
        }
        content.push(String::new());
        let labels: Vec<String> = ACTIONS.iter().map(|a| a.label().to_owned()).collect();
        content.push(choices(&labels, self.selected, p));
        content.push(String::new());
        content.push(fg(
            &truncate_cells("y allow · n reject · ←→/tab move · enter confirm", inner),
            &p.muted,
        ));
        bordered(&content, width, None, &p.border, None)
    }
}

// ---- AskUser ----------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AskUserPrompt {
    pub request_id: String,
    pub branch_id: String,
    pub questions: Vec<Question>,
    pub active_tab: usize,
    pub option_index: usize,
    pub selections: BTreeMap<String, Vec<String>>,
    pub custom_answers: BTreeMap<String, String>,
    pub typing: bool,
    pub typing_draft: String,
    pub missing_tab: Option<usize>,
}

impl AskUserPrompt {
    pub fn new(request_id: String, branch_id: String, questions: Vec<Question>) -> Self {
        Self {
            request_id,
            branch_id,
            questions,
            active_tab: 0,
            option_index: 0,
            selections: BTreeMap::new(),
            custom_answers: BTreeMap::new(),
            typing: false,
            typing_draft: String::new(),
            missing_tab: None,
        }
    }

    fn on_confirm_tab(&self) -> bool {
        self.active_tab >= self.questions.len()
    }

    fn question(&self) -> Option<&Question> {
        self.questions.get(self.active_tab)
    }

    fn multi(question: &Question) -> bool {
        question.kind == QuestionKind::MultiSelect
    }

    fn move_tab(&mut self, direction: isize) {
        if self.questions.is_empty() {
            return;
        }
        let next =
            (self.active_tab as isize + direction).clamp(0, self.questions.len() as isize) as usize;
        self.set_tab(next);
    }

    fn set_tab(&mut self, next: usize) {
        if self.questions.is_empty() || next == self.active_tab {
            return;
        }
        let next = next.min(self.questions.len());
        self.active_tab = next;
        self.option_index = 0;
        self.typing = false;
        self.typing_draft = self
            .questions
            .get(next)
            .and_then(|q| self.custom_answers.get(&q.id).cloned())
            .unwrap_or_default();
    }

    fn move_option(&mut self, direction: isize) {
        let Some(question) = self.question() else {
            return;
        };
        let rows = question.options.len() + 1;
        let next = (self.option_index as isize + direction).clamp(0, rows as isize - 1) as usize;
        if next != self.option_index {
            self.option_index = next;
            self.typing = false;
        }
    }

    fn toggle(&mut self, label: &str) {
        let Some(question) = self.question() else {
            return;
        };
        let id = question.id.clone();
        let current = self.selections.entry(id).or_default();
        if let Some(at) = current.iter().position(|v| v == label) {
            current.remove(at);
        } else {
            current.push(label.to_owned());
        }
        self.missing_tab = None;
    }

    fn select_single(&mut self, label: &str) {
        let Some(question) = self.question() else {
            return;
        };
        let id = question.id.clone();
        self.selections.insert(id.clone(), vec![label.to_owned()]);
        self.custom_answers.remove(&id);
        self.missing_tab = None;
    }

    fn activate_preset(&mut self) {
        let Some(question) = self.question() else {
            return;
        };
        let Some(option) = question.options.get(self.option_index) else {
            return;
        };
        let label = option.label.clone();
        if Self::multi(question) {
            self.toggle(&label);
        } else {
            self.select_single(&label);
            self.move_tab(1);
        }
    }

    fn enter_typing(&mut self) {
        let Some(question) = self.question() else {
            return;
        };
        if self.option_index != question.options.len() {
            return;
        }
        let draft = self
            .custom_answers
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        self.typing = true;
        self.typing_draft = draft;
    }

    fn commit_typing(&mut self) {
        if !self.typing {
            return;
        }
        let Some(question) = self.question() else {
            self.typing = false;
            self.typing_draft.clear();
            return;
        };
        let id = question.id.clone();
        let multi = Self::multi(question);
        let text = self.typing_draft.trim().to_owned();
        self.typing = false;
        self.typing_draft.clear();
        if text.is_empty() {
            return;
        }
        self.custom_answers.insert(id.clone(), text);
        self.missing_tab = None;
        if !multi {
            self.selections.remove(&id);
            self.move_tab(1);
        }
    }

    fn collect(&self) -> Result<Answers, usize> {
        let mut answers = Answers::new();
        for (index, question) in self.questions.iter().enumerate() {
            let selections = self
                .selections
                .get(&question.id)
                .cloned()
                .unwrap_or_default();
            let custom = self
                .custom_answers
                .get(&question.id)
                .map(|c| c.trim().to_owned())
                .unwrap_or_default();
            if Self::multi(question) {
                let mut values = selections;
                if !custom.is_empty() {
                    values.push(custom);
                }
                if values.is_empty() {
                    return Err(index);
                }
                answers.insert(question.id.clone(), Answer::Multiple(values));
                continue;
            }
            if !custom.is_empty() {
                answers.insert(question.id.clone(), Answer::Single(custom));
                continue;
            }
            match selections.into_iter().next() {
                Some(first) => {
                    answers.insert(question.id.clone(), Answer::Single(first));
                }
                None => return Err(index),
            }
        }
        Ok(answers)
    }

    fn answer_text(&self, question: &Question) -> Option<String> {
        let custom = self
            .custom_answers
            .get(&question.id)
            .map(|c| c.trim().to_owned())
            .unwrap_or_default();
        let mut parts: Vec<String> = self
            .selections
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        if !custom.is_empty() {
            parts.push(custom);
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    fn cancel(&self) -> Effect {
        Effect::Cancel {
            request_id: self.request_id.clone(),
            branch_id: self.branch_id.clone(),
            reason: "Cancelled by user".to_owned(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.typing {
            return self.handle_typing_key(key);
        }
        match key.code {
            KeyCode::Esc => return self.cancel(),
            KeyCode::Tab => {
                self.move_tab(if key.mods.contains(Modifiers::SHIFT) {
                    -1
                } else {
                    1
                });
            }
            KeyCode::BackTab | KeyCode::Left => self.move_tab(-1),
            KeyCode::Right => self.move_tab(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_option(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_option(1),
            KeyCode::Char(' ') => {
                let Some(question) = self.question() else {
                    return Effect::None;
                };
                if self.option_index >= question.options.len() {
                    self.enter_typing();
                } else {
                    let label = question.options[self.option_index].label.clone();
                    if Self::multi(question) {
                        self.toggle(&label);
                    } else {
                        self.select_single(&label);
                    }
                }
            }
            KeyCode::Enter => {
                if self.on_confirm_tab() {
                    return match self.collect() {
                        Ok(answers) => Effect::Answers {
                            request_id: self.request_id.clone(),
                            branch_id: self.branch_id.clone(),
                            answers,
                        },
                        Err(missing) => {
                            self.active_tab = missing;
                            self.option_index = 0;
                            self.missing_tab = Some(missing);
                            self.typing = false;
                            self.typing_draft.clear();
                            Effect::None
                        }
                    };
                }
                let Some(question) = self.question() else {
                    return Effect::None;
                };
                if self.option_index >= question.options.len() {
                    self.enter_typing();
                } else if Self::multi(question) {
                    self.move_tab(1);
                } else {
                    self.activate_preset();
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let digit = c as usize - '0' as usize;
                if self.on_confirm_tab() {
                    if digit <= self.questions.len() {
                        self.set_tab(digit - 1);
                    }
                } else if let Some(question) = self.question()
                    && digit <= question.options.len()
                {
                    self.option_index = digit - 1;
                    self.activate_preset();
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn handle_typing_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Esc => {
                self.typing = false;
                self.typing_draft.clear();
            }
            KeyCode::Backspace => {
                self.typing_draft.pop();
            }
            KeyCode::Enter => self.commit_typing(),
            KeyCode::Char(c) if plain(key) => self.typing_draft.push(c),
            _ => {}
        }
        Effect::None
    }

    // ---- view -----------------------------------------------------------

    fn tab_row(&self, inner: usize, p: &Palette) -> String {
        let mut labels: Vec<String> = self
            .questions
            .iter()
            .map(|q| {
                let tick = if self.answer_text(q).is_some() {
                    "✓"
                } else {
                    " "
                };
                format!("{tick} {}", q.header)
            })
            .collect();
        labels.push("Confirm".to_owned());
        let tabs = choices(&labels, self.active_tab, p);
        let counter = format!("{}/{}", self.active_tab + 1, labels.len());
        let used = visible_width(&tabs) + counter.len();
        if inner < used + 2 {
            return tabs;
        }
        format!(
            "{tabs}{}{}",
            " ".repeat(inner - used),
            fg(&counter, &p.faint)
        )
    }

    pub fn render(&self, width: usize, max_rows: usize, p: &Palette) -> Vec<String> {
        const CHROME: usize = 6;
        let inner = width.saturating_sub(4).max(1);
        let limit = max_rows.saturating_sub(CHROME).max(1);
        let roomy = self.body(inner, true);
        let body = if body_rows(&roomy) <= limit {
            roomy
        } else {
            self.body(inner, false)
        };
        let mut content = vec![self.tab_row(inner, p), String::new()];
        for line in fit_body(&body, limit) {
            content.push(line_text(&line, inner, p));
        }
        content.push(String::new());
        content.push(fg(&truncate_cells(&self.hint(), inner), &p.muted));
        bordered(&content, width, None, &p.border, None)
    }

    fn hint(&self) -> String {
        if self.typing {
            return "type answer · enter save · esc back".to_owned();
        }
        if self.on_confirm_tab() {
            return "enter submit · number edit · tab/←→ back · esc cancel".to_owned();
        }
        match self.question() {
            Some(q) if Self::multi(q) => {
                "↑↓ move · space toggle · enter next · tab/←→ switch · esc cancel".to_owned()
            }
            _ => "↑↓ move · number select · enter next · tab/←→ switch · esc cancel".to_owned(),
        }
    }

    fn body(&self, width: usize, roomy: bool) -> Body {
        if self.on_confirm_tab() {
            return self.review(roomy);
        }
        let Some(question) = self.question() else {
            return Body {
                head: vec![Line::warn("AskUserQuestion received an invalid payload.")],
                rows: Vec::new(),
            };
        };
        let mut head: Vec<Line> = wrap_cells(&question.question, width)
            .into_iter()
            .map(Line::text)
            .collect();
        if self.missing_tab == Some(self.active_tab) {
            head.push(Line::warn(format!(
                "Missing answer for Q{}",
                self.active_tab + 1
            )));
        }
        if roomy {
            head.push(Line::text(String::new()));
        }
        let selected_values = self
            .selections
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        let index_width = (question.options.len() + 1).to_string().len();
        let detail_indent = " ".repeat(2 + index_width + 1 + 4);
        let multi = Self::multi(question);
        let mut rows: Vec<Row> = Vec::new();
        for (index, option) in question.options.iter().enumerate() {
            let selected = self.option_index == index;
            let prefix = row_prefix(
                selected,
                index,
                index_width,
                mark(multi, selected_values.contains(&option.label)),
            );
            let rendered = match &option.description {
                None => vec![Line::choice(format!("{prefix}{}", option.label), selected)],
                Some(description) if !roomy => vec![Line::choice(
                    format!("{prefix}{} · {description}", option.label),
                    selected,
                )],
                Some(description) => vec![
                    Line::choice(format!("{prefix}{}", option.label), selected),
                    Line::detail(format!("{detail_indent}{description}"), selected),
                ],
            };
            rows.push(Row {
                lines: rendered,
                selected,
            });
        }
        // The free-text row closes the list.
        let index = question.options.len();
        let selected = self.option_index == index;
        let stored = self
            .custom_answers
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        let draft = if self.typing {
            self.typing_draft.clone()
        } else {
            stored.clone()
        };
        let prefix = row_prefix(
            selected,
            index,
            index_width,
            mark(multi, !stored.is_empty()),
        );
        let label = format!("{prefix}Type your own answer");
        let lines = if !self.typing && draft.is_empty() {
            vec![Line::choice(label, selected)]
        } else {
            let value = format!("{draft}{}", if self.typing { "▌" } else { "" });
            if roomy {
                vec![
                    Line::choice(label, selected),
                    Line::detail(format!("{detail_indent}{value}"), selected),
                ]
            } else {
                vec![Line::choice(format!("{label}: {value}"), selected)]
            }
        };
        rows.push(Row { lines, selected });
        Body { head, rows }
    }

    fn review(&self, roomy: bool) -> Body {
        let mut head = vec![Line {
            text: "Review your answers and press Enter to submit.".to_owned(),
            tone: Tone::Detail,
            ..Line::text(String::new())
        }];
        if roomy {
            head.push(Line::text(String::new()));
        }
        let index_width = self.questions.len().to_string().len();
        let rows = self
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let answer = self.answer_text(question);
                let label = format!(
                    "{}{:>index_width$} {}",
                    " ".repeat(2),
                    index + 1,
                    question.header
                );
                let shown = answer.clone().unwrap_or_else(|| "Not answered".to_owned());
                let lines = if roomy {
                    vec![
                        Line::text(label),
                        Line {
                            text: format!("{}{shown}", " ".repeat(2 + index_width + 1)),
                            tone: if answer.is_some() {
                                Tone::FocusedDetail
                            } else {
                                Tone::Warning
                            },
                            ..Line::text(String::new())
                        },
                    ]
                } else {
                    vec![Line {
                        text: format!("{label}: {shown}"),
                        tone: if answer.is_some() {
                            Tone::Text
                        } else {
                            Tone::Warning
                        },
                        ..Line::text(String::new())
                    }]
                };
                Row {
                    lines,
                    selected: false,
                }
            })
            .collect();
        Body { head, rows }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Text,
    Detail,
    FocusedDetail,
    Warning,
    Selected,
}

#[derive(Debug, Clone)]
struct Line {
    text: String,
    tone: Tone,
}

impl Line {
    fn text(text: String) -> Self {
        Self {
            text,
            tone: Tone::Text,
        }
    }
    fn warn(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Warning,
        }
    }
    fn detail(text: String, selected: bool) -> Self {
        Self {
            text,
            tone: if selected {
                Tone::FocusedDetail
            } else {
                Tone::Detail
            },
        }
    }
    fn choice(text: String, selected: bool) -> Self {
        Self {
            text,
            tone: if selected { Tone::Selected } else { Tone::Text },
        }
    }
}

struct Row {
    lines: Vec<Line>,
    selected: bool,
}

struct Body {
    head: Vec<Line>,
    rows: Vec<Row>,
}

fn body_rows(body: &Body) -> usize {
    body.head.len() + body.rows.iter().map(|r| r.lines.len()).sum::<usize>()
}

fn row_prefix(selected: bool, index: usize, index_width: usize, mark: &str) -> String {
    format!(
        "{}{:>index_width$} {mark} ",
        if selected {
            SELECTED_MARKER
        } else {
            IDLE_MARKER
        },
        index + 1
    )
}

fn mark(multi: bool, checked: bool) -> &'static str {
    match (multi, checked) {
        (true, true) => "[x]",
        (true, false) => "[ ]",
        (false, true) => "(•)",
        (false, false) => "( )",
    }
}

/// Keeps the focused row visible and grows the window outwards from it.
fn window(rows: &[Row], limit: usize) -> Vec<Line> {
    let anchor = rows.iter().position(|r| r.selected).unwrap_or(0);
    let Some(focused) = rows.get(anchor) else {
        return Vec::new();
    };
    if focused.lines.len() >= limit {
        return focused.lines.iter().take(limit).cloned().collect();
    }
    let (mut start, mut end, mut used, mut prefer_end) =
        (anchor, anchor, focused.lines.len(), true);
    loop {
        let next_end = rows.get(end + 1);
        let next_start = if start > 0 { rows.get(start - 1) } else { None };
        let fits_end = next_end.is_some_and(|r| used + r.lines.len() <= limit);
        let fits_start = next_start.is_some_and(|r| used + r.lines.len() <= limit);
        if fits_end && (prefer_end || !fits_start) {
            end += 1;
            used += rows[end].lines.len();
            prefer_end = false;
        } else if fits_start {
            start -= 1;
            used += rows[start].lines.len();
            prefer_end = true;
        } else {
            break;
        }
    }
    rows[start..=end]
        .iter()
        .flat_map(|r| r.lines.clone())
        .collect()
}

fn fit_body(body: &Body, limit: usize) -> Vec<Line> {
    if body_rows(body) <= limit {
        let mut out = body.head.clone();
        out.extend(body.rows.iter().flat_map(|r| r.lines.clone()));
        return out;
    }
    if body.head.len() >= limit {
        return body.head.iter().take(limit).cloned().collect();
    }
    let mut out = body.head.clone();
    out.extend(window(&body.rows, limit - body.head.len()));
    out
}

fn line_text(line: &Line, width: usize, p: &Palette) -> String {
    if line.text.is_empty() {
        return String::new();
    }
    let color = match line.tone {
        Tone::Selected => &p.accent,
        Tone::Warning => &p.warning,
        Tone::Detail => &p.faint,
        Tone::FocusedDetail => &p.muted,
        Tone::Text => &p.text,
    };
    paint(
        &truncate_cells(&line.text, width),
        Paint {
            fg: Some(color),
            bold: line.tone == Tone::Selected,
            ..Paint::default()
        },
    )
}

#[allow(dead_code)]
fn used(text: &str) -> usize {
    display_width(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Appearance, ColorMode, Theme};
    use crate::view::QuestionOption;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut skip = false;
        for c in s.chars() {
            match (skip, c) {
                (false, '\x1b') => skip = true,
                (true, 'm') => skip = false,
                (false, c) => out.push(c),
                _ => {}
            }
        }
        out
    }

    fn dinner(kind: QuestionKind) -> AskUserPrompt {
        AskUserPrompt::new(
            "ask-1".into(),
            "main".into(),
            vec![Question {
                id: "dinner".into(),
                header: "Dinner".into(),
                question: "What should we eat tonight?".into(),
                kind,
                options: vec![
                    QuestionOption {
                        label: "Hotpot".into(),
                        description: Some("Takes longer".into()),
                    },
                    QuestionOption {
                        label: "Ramen".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "Fried rice".into(),
                        description: None,
                    },
                ],
            }],
        )
    }

    #[test]
    fn permission_keys_map_to_decisions() {
        let mut p = PermissionPrompt::new(
            "r1".into(),
            "main".into(),
            "source: tool_rule".into(),
            "edit".into(),
            serde_json::json!({"path": "a"}),
        );
        assert!(matches!(
            p.handle_key(key(KeyCode::Char('y'))),
            Effect::Approval {
                allow: true,
                remember: false,
                ..
            }
        ));
        assert!(matches!(
            p.handle_key(key(KeyCode::Char('n'))),
            Effect::Approval { allow: false, .. }
        ));
        p.handle_key(key(KeyCode::Right));
        match p.handle_key(key(KeyCode::Enter)) {
            Effect::Approval {
                allow,
                remember,
                audit,
                ..
            } => {
                assert!(allow && remember);
                assert_eq!(audit, "approved always: edit");
            }
            other => panic!("{other:?}"),
        }
        // The session-wide switches are not wired yet and never answer.
        p.selected = 3;
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            Effect::Notice(_)
        ));
    }

    #[test]
    fn permission_panel_matches_the_reference_layout() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let p = PermissionPrompt::new(
            "r1".into(),
            "main".into(),
            "source: tool_rule".into(),
            "edit".into(),
            serde_json::json!({"path": "src/index.ts"}),
        );
        let rows: Vec<String> = p
            .render(100, &theme.palette)
            .iter()
            .map(|r| strip(r))
            .collect();
        assert!(rows[1].contains("Permission required"));
        assert!(rows[3].contains("# source: tool_rule"));
        assert!(rows[4].contains("$ edit {\"path\":\"src/index.ts\"}"));
        assert!(rows[6].contains("› Allow once   "));
        assert!(rows[6].contains("Allow always   "));
        assert!(rows[6].contains("Approve for me   "));
        assert!(rows[8].contains("y allow · n reject · ←→/tab move · enter confirm"));
    }

    #[test]
    fn multi_select_collects_two_answers_through_the_review_tab() {
        let mut ask = dinner(QuestionKind::MultiSelect);
        ask.handle_key(key(KeyCode::Char(' ')));
        ask.handle_key(key(KeyCode::Down));
        ask.handle_key(key(KeyCode::Down));
        ask.handle_key(key(KeyCode::Char(' ')));
        // Enter on a multi-select moves on to the review tab.
        ask.handle_key(key(KeyCode::Enter));
        assert!(ask.on_confirm_tab());
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let text: Vec<String> = ask
            .render(100, 20, &theme.palette)
            .iter()
            .map(|r| strip(r))
            .collect();
        assert!(
            text.iter().any(|r| r.contains("Hotpot, Fried rice")),
            "{text:#?}"
        );
        match ask.handle_key(key(KeyCode::Enter)) {
            Effect::Answers { answers, .. } => {
                assert_eq!(
                    answers.get("dinner"),
                    Some(&Answer::Multiple(vec![
                        "Hotpot".into(),
                        "Fried rice".into()
                    ]))
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn single_select_enter_advances_and_review_flags_missing_answers() {
        let mut ask = dinner(QuestionKind::SingleSelect);
        // Review without answering: the missing question is focused and flagged.
        ask.set_tab(1);
        assert_eq!(ask.handle_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!(ask.active_tab, 0);
        assert_eq!(ask.missing_tab, Some(0));
        // A digit picks and advances.
        ask.handle_key(key(KeyCode::Char('2')));
        assert!(ask.on_confirm_tab());
        assert!(matches!(
            ask.handle_key(key(KeyCode::Enter)),
            Effect::Answers { .. }
        ));
    }

    #[test]
    fn custom_answer_is_typed_and_saved() {
        let mut ask = dinner(QuestionKind::SingleSelect);
        ask.option_index = 3;
        ask.handle_key(key(KeyCode::Enter));
        assert!(ask.typing);
        for c in "pizza".chars() {
            ask.handle_key(key(KeyCode::Char(c)));
        }
        ask.handle_key(key(KeyCode::Enter));
        assert!(!ask.typing && ask.on_confirm_tab());
        match ask.handle_key(key(KeyCode::Enter)) {
            Effect::Answers { answers, .. } => {
                assert_eq!(answers.get("dinner"), Some(&Answer::Single("pizza".into())));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ask_user_panel_rows_match_the_reference() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let ask = dinner(QuestionKind::MultiSelect);
        let text: Vec<String> = ask
            .render(100, 20, &theme.palette)
            .iter()
            .map(|r| strip(r))
            .collect();
        assert!(
            text[1].contains("›   Dinner     Confirm") && text[1].trim_end().ends_with("1/2 │")
        );
        assert!(text[3].contains("What should we eat tonight?"));
        assert!(text[5].contains("› 1 [ ] Hotpot"));
        assert!(text[6].contains("        Takes longer"));
        assert!(text[7].contains("  2 [ ] Ramen"));
        assert!(text[9].contains("  4 [ ] Type your own answer"));
        assert!(
            text[11].contains("↑↓ move · space toggle · enter next · tab/←→ switch · esc cancel")
        );
    }

    #[test]
    fn exit_confirm_defaults_to_stay() {
        let mut exit = ExitConfirm::new(ExitReason::AgentRunning);
        assert_eq!(exit.handle_key(key(KeyCode::Enter)), Effect::Close);
        assert_eq!(exit.handle_key(key(KeyCode::Char('y'))), Effect::Exit);
        exit.handle_key(key(KeyCode::Left));
        assert_eq!(exit.handle_key(key(KeyCode::Enter)), Effect::Exit);
    }
}
