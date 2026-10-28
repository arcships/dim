//! Runtime prompts of the scrollback interface: the exit confirmation, tool
//! permission requests and AskUser questions (ports of `prompts/model.ts`,
//! `prompts/view.ts` and the overlay state machines they sit on).
//!
//! Each prompt is a small state value with a key handler returning an
//! [`Effect`]; the app owns the side effects (RPCs, notices, exit).

use std::collections::BTreeMap;

use crate::question::{Answer, Call, Question};
use serde_json::Value;

use crate::ui::i18n::{t, tf};
use crate::ui::line_input::LineInput;
use crate::ui::render::panel::{
    IDLE_MARKER, SELECTED_MARKER, bordered, choices, confirm_panel, key_hints, visible_width,
};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::{display_width, truncate_cells, wrap_cells};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};

/// What a key did to a prompt.
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// Nothing for the app to do (the state may have changed).
    None,
    /// The prompt wants to leave without answering anything.
    Close,
    Exit,
    /// Leave and start the updated CLI on this session; `confirmed` once a
    /// busy session's confirmation said so.
    Restart {
        confirmed: bool,
    },
    /// Install the offered version.
    UpgradeInstall(crate::update::Install),
    /// Never offer this version again.
    UpgradeSkip(String),
    /// Answer an approval request.
    Approval {
        request_id: String,
        branch_id: String,
        allow: bool,
        remember: bool,
    },
    Answers {
        call: Call,
        answers: Vec<Answer>,
    },
    Cancel {
        call: Call,
    },
    Notice(String),
    /// Switch to the stored session with this id.
    Resume(String),
    /// Reload the session list.
    Refresh,
    /// Switch the session to full access and allow this call.
    FullAccess {
        request_id: String,
        branch_id: String,
    },
    /// Stop the background command on its originating branch.
    StopTask {
        session_id: String,
        projection_id: u64,
        target: crate::model::view::TaskTarget,
    },
    /// Interrupt this member Session's current turn.
    StopMember {
        session_id: String,
        projection_id: u64,
        member_id: String,
    },
    StopTasks {
        session_id: String,
        projection_id: u64,
    },
    /// Ask Workflow to stop this run.
    StopWorkflow {
        session_id: String,
        projection_id: u64,
        run_id: String,
    },
    /// Set the default tool-approval mode.
    SetApprovals(String),
    /// Paint with this theme without saving it.
    ThemePreview(String),
    /// Keep this theme and save it.
    ThemeApply(String),
    /// Leave the picker and paint with this theme again.
    ThemeRestore(String),
    /// Show the interface in this language (`None`: the system's) unsaved.
    LanguagePreview(Option<crate::ui::i18n::Locale>),
    /// Keep this language and save it.
    LanguageApply(Option<crate::ui::i18n::Locale>),
    /// Leave the picker and go back to this language.
    LanguageRestore(Option<crate::ui::i18n::Locale>),
    /// Flip or open a `/settings` row.
    Setting(crate::ui::settings_panel::Setting),
    /// Change a provider (`/connect`).
    Provider(crate::model::session::ProviderAction),
    /// Begin a sign-in on the worker.
    OauthStart(crate::model::session::OauthStart),
    /// Abandon the sign-in in progress.
    OauthCancel,
    /// Leave the OAuth step for the provider list (panel-internal).
    OauthBack,
    /// A callback URL the user pasted.
    OauthPaste(String),
    /// A signed-in provider: close the panel and offer its models.
    Connected(String),
    /// Open the `/models` picker in place of this panel.
    OpenModels,
    /// Sign this account provider out; the host reports the account again.
    Logout(String),
    /// Report this account provider's usage again.
    LoginUsage(String),
    /// Open this page in the browser.
    OpenUrl(String),
    /// Commit this text to the transcript as a status block.
    Print(String),
    /// Make this model the default and the session's model.
    ClearModelPreferences {
        provider_id: String,
        model_id: String,
        expected_version: u32,
    },
    SelectModel {
        provider_id: String,
        model_id: String,
    },
    /// Reread the subagent model policy before another save.
    ReadModelPolicy,
    SetModelPolicy {
        expected_revision: String,
        policy: crate::model::session::SubagentModelPolicy,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    AgentRunning,
    AskUserPending,
    PermissionPending,
}

impl ExitReason {
    fn text(self) -> &'static str {
        match self {
            Self::AgentRunning => t("The agent is still running."),
            Self::AskUserPending => t("AskUser is waiting for an answer."),
            Self::PermissionPending => t("A permission request is waiting."),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExitConfirm {
    pub reason: ExitReason,
    /// Leaving restarts onto the updated CLI instead of exiting.
    pub restart: bool,
    /// 0 = exit, 1 = stay (the default).
    pub focused: usize,
}

impl ExitConfirm {
    pub fn new(reason: ExitReason) -> Self {
        Self {
            reason,
            restart: false,
            focused: 1,
        }
    }

    pub fn restart(reason: ExitReason) -> Self {
        Self {
            restart: true,
            ..Self::new(reason)
        }
    }

    fn leave(&self) -> Effect {
        if self.restart {
            Effect::Restart { confirmed: true }
        } else {
            Effect::Exit
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') if plain(key) => self.leave(),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc if plain(key) => Effect::Close,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.focused = 1 - self.focused;
                Effect::None
            }
            KeyCode::Enter => {
                if self.focused == 0 {
                    self.leave()
                } else {
                    Effect::Close
                }
            }
            _ => Effect::None,
        }
    }

    pub fn render(&self, width: usize, p: &Palette) -> Vec<String> {
        let (title, leave, hint) = if self.restart {
            (
                t("Restart dimcode?"),
                t("Restart"),
                t("y restart · n stay · ←→ move · enter confirm"),
            )
        } else {
            (
                t("Exit dimcode?"),
                t("Exit"),
                t("y exit · n stay · ←→ move · enter confirm"),
            )
        };
        confirm_panel(
            title,
            &[self.reason.text().to_owned()],
            &[leave.to_owned(), t("Stay").to_owned()],
            self.focused,
            hint,
            width,
            p,
        )
    }
}

/// The confirmation full access takes, here and in the `/approvals` panel.
pub const CONFIRM_HINT: &str = "Tab switch  Enter confirm  Esc cancel";
pub const CONFIRM_BODY: &str = "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAction {
    AllowOnce,
    /// Allow and save an allow rule for the command, so it never asks again.
    AllowAlways,
    Reject,
    /// Switch the session to the full-access preset, which also becomes the
    /// default for later sessions, and allow this call. The loudest last.
    FullAccess,
}

impl PermissionAction {
    fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => t("Allow once"),
            Self::AllowAlways => t("Allow always"),
            Self::Reject => t("Reject"),
            Self::FullAccess => t("Full access"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PermissionPrompt {
    pub request_id: String,
    pub branch_id: String,
    pub reason: String,
    pub tool_name: String,
    pub input: Value,
    pub selected: usize,
    /// The decisions offered: "always" only for a simple command the
    /// daemon can keep a rule for.
    actions: Vec<PermissionAction>,
    /// `Some(focused button)` while full access awaits confirmation:
    /// 0 = Confirm, 1 = Cancel.
    confirming: Option<usize>,
}

impl PermissionPrompt {
    pub fn new(
        request_id: String,
        branch_id: String,
        reason: String,
        tool_name: String,
        input: Value,
        always: bool,
    ) -> Self {
        let mut actions = vec![PermissionAction::AllowOnce];
        if always {
            actions.push(PermissionAction::AllowAlways);
        }
        actions.push(PermissionAction::Reject);
        actions.push(PermissionAction::FullAccess);
        Self {
            request_id,
            branch_id,
            reason,
            tool_name,
            input,
            selected: 0,
            actions,
            confirming: None,
        }
    }

    fn always(&self) -> bool {
        self.actions.contains(&PermissionAction::AllowAlways)
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
            tf("Run {tool}", &[("tool", &self.tool_name)])
        } else {
            collapse(&self.reason)
        };
        vec![
            format!("# {description}"),
            format!("$ {}", self.command_preview()),
        ]
    }

    fn full_access(&self) -> Effect {
        Effect::FullAccess {
            request_id: self.request_id.clone(),
            branch_id: self.branch_id.clone(),
        }
    }

    fn decide(&self, allow: bool, remember: bool) -> Effect {
        Effect::Approval {
            request_id: self.request_id.clone(),
            branch_id: self.branch_id.clone(),
            allow,
            remember,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if let Some(focus) = &mut self.confirming {
            return match key.code {
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                    *focus = 1 - *focus;
                    Effect::None
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => self.full_access(),
                KeyCode::Enter if *focus == 0 => self.full_access(),
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.confirming = None;
                    Effect::None
                }
                _ => Effect::None,
            };
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') if plain(key) => self.decide(true, false),
            KeyCode::Char('n') | KeyCode::Char('N') if plain(key) => self.decide(false, false),
            KeyCode::Char('a') | KeyCode::Char('A') if plain(key) && self.always() => {
                self.decide(true, true)
            }
            KeyCode::Right | KeyCode::Tab => {
                self.selected = (self.selected + 1) % self.actions.len();
                Effect::None
            }
            KeyCode::Left | KeyCode::BackTab => {
                self.selected = (self.selected + self.actions.len() - 1) % self.actions.len();
                Effect::None
            }
            KeyCode::Enter => match self.actions[self.selected] {
                PermissionAction::AllowOnce => self.decide(true, false),
                PermissionAction::AllowAlways => self.decide(true, true),
                PermissionAction::Reject => self.decide(false, false),
                PermissionAction::FullAccess => {
                    self.confirming = Some(1);
                    Effect::None
                }
            },
            _ => Effect::None,
        }
    }

    pub fn render(&self, width: usize, p: &Palette) -> Vec<String> {
        if let Some(focus) = self.confirming {
            return confirm_panel(
                t("Confirm Full Access"),
                &[t(CONFIRM_BODY).to_owned()],
                &[t("Confirm").to_owned(), t("Cancel").to_owned()],
                focus,
                t(CONFIRM_HINT),
                width,
                p,
            );
        }
        let inner = width.saturating_sub(4).max(1);
        let title = paint(
            &truncate_cells(t("Permission required"), inner),
            Paint {
                fg: Some(&p.warning),
                bold: true,
                ..Paint::default()
            },
        );
        let mut content = vec![String::new()];
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
        let labels: Vec<String> = self.actions.iter().map(|a| a.label().to_owned()).collect();
        content.push(choices(&labels, self.selected, p));
        content.push(String::new());
        let hint = key_hints(
            if self.always() {
                t("y allow · a always · n reject · ←→/Tab move · Enter confirm")
            } else {
                t("y allow · n reject · ←→/Tab move · Enter confirm")
            },
            p,
        );
        bordered(&content, width, None, &p.border, Some(&title), Some(&hint))
    }
}

pub struct AskUserPrompt {
    pub call: Call,
    pub sending: bool,
    pub error: Option<String>,
    pub active_tab: usize,
    pub option_index: usize,
    pub selections: BTreeMap<String, Vec<String>>,
    pub custom_answers: BTreeMap<String, String>,
    pub typing: bool,
    /// The free-text answer being edited: the composer's line editor.
    pub draft: LineInput,
    pub missing_tab: Option<usize>,
}

impl AskUserPrompt {
    pub fn new(call: Call) -> Self {
        Self {
            call,
            sending: false,
            error: None,
            active_tab: 0,
            option_index: 0,
            selections: BTreeMap::new(),
            custom_answers: BTreeMap::new(),
            typing: false,
            draft: LineInput::new(""),
            missing_tab: None,
        }
    }

    fn on_confirm_tab(&self) -> bool {
        self.active_tab >= self.call.questions.len()
    }

    fn question(&self) -> Option<&Question> {
        self.call.questions.get(self.active_tab)
    }

    fn move_tab(&mut self, direction: isize) {
        if self.call.questions.is_empty() {
            return;
        }
        let next = (self.active_tab as isize + direction)
            .clamp(0, self.call.questions.len() as isize) as usize;
        self.set_tab(next);
    }

    fn set_tab(&mut self, next: usize) {
        if self.call.questions.is_empty() || next == self.active_tab {
            return;
        }
        let next = next.min(self.call.questions.len());
        self.active_tab = next;
        self.option_index = 0;
        self.typing = false;
        self.draft = LineInput::new(
            &self
                .call
                .questions
                .get(next)
                .and_then(|q| self.custom_answers.get(&q.id).cloned())
                .unwrap_or_default(),
        );
    }

    fn move_option(&mut self, direction: isize) {
        let Some(question) = self.question() else {
            return;
        };
        let rows = question.options.len() + usize::from(question.allow_custom);
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
        if question.multi_select {
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
        if !question.allow_custom || self.option_index != question.options.len() {
            return;
        }
        let draft = self
            .custom_answers
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        self.typing = true;
        self.draft = LineInput::new(&draft);
    }

    fn sync_draft(&mut self) {
        let Some(question) = self.question() else {
            return;
        };
        let id = question.id.clone();
        let multi = question.multi_select;
        let text = self.draft.text().trim().to_owned();
        if text.is_empty() {
            self.custom_answers.remove(&id);
            return;
        }
        self.custom_answers.insert(id.clone(), text);
        self.missing_tab = None;
        if !multi {
            self.selections.remove(&id);
        }
    }

    fn stop_typing(&mut self, advance: bool) {
        self.typing = false;
        self.draft.clear();
        if advance
            && let Some(question) = self.question()
            && self.custom_answers.contains_key(&question.id)
        {
            self.move_tab(1);
        }
    }

    fn collect(&self) -> Result<Vec<Answer>, usize> {
        let mut answers = Vec::new();
        for (index, question) in self.call.questions.iter().enumerate() {
            let selected = self
                .selections
                .get(&question.id)
                .cloned()
                .unwrap_or_default();
            let custom = self
                .custom_answers
                .get(&question.id)
                .filter(|text| !text.trim().is_empty())
                .cloned();
            if selected.is_empty() && custom.is_none() {
                return Err(index);
            }
            answers.push(Answer {
                question_id: question.id.clone(),
                selected,
                custom,
            });
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
            call: self.call.clone(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.sending {
            return Effect::None;
        }
        if self.typing {
            return self.handle_typing_key(key);
        }
        if let KeyCode::Char(c) = key.code
            && plain(key)
            && c != ' '
            && self
                .question()
                .is_some_and(|q| q.allow_custom && self.option_index == q.options.len())
        {
            self.enter_typing();
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
                    if question.multi_select {
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
                            call: self.call.clone(),
                            answers,
                        },
                        Err(missing) => {
                            self.active_tab = missing;
                            self.option_index = 0;
                            self.missing_tab = Some(missing);
                            self.typing = false;
                            self.draft.clear();
                            Effect::None
                        }
                    };
                }
                let Some(question) = self.question() else {
                    return Effect::None;
                };
                if self.option_index >= question.options.len() {
                    self.enter_typing();
                } else if question.multi_select {
                    self.move_tab(1);
                } else {
                    self.activate_preset();
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let digit = c as usize - '0' as usize;
                if self.on_confirm_tab() {
                    if digit <= self.call.questions.len() {
                        self.set_tab(digit - 1);
                    }
                } else if let Some(question) = self.question()
                    && digit <= question.options.len() + usize::from(question.allow_custom)
                {
                    let custom = digit > question.options.len();
                    self.option_index = digit - 1;
                    if custom {
                        self.enter_typing();
                    } else {
                        self.activate_preset();
                    }
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn handle_typing_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Esc => self.stop_typing(false),
            KeyCode::Enter => self.stop_typing(true),
            _ => {
                self.draft.handle_key(key);
                self.sync_draft();
            }
        }
        Effect::None
    }

    /// A paste goes into the free-text answer, opening it on its row.
    pub fn paste(&mut self, text: &str) {
        if self.sending {
            return;
        }
        if !self.typing {
            self.enter_typing();
        }
        if self.typing {
            self.draft.paste(text);
            self.sync_draft();
        }
    }

    /// The caret of the free-text answer being edited: its row and column
    /// in the rows [`Self::render`] draws.
    pub fn cursor(&self, width: usize, max_rows: usize) -> Option<(usize, usize)> {
        let (inner, lines) = self.layout(width, max_rows);
        let (index, caret) = lines
            .iter()
            .enumerate()
            .find_map(|(index, line)| Some((index, line.caret?)))?;
        let above = 3 + usize::from(self.error.is_some());
        Some((above + index, 2 + caret.min(inner)))
    }

    fn tab_row(&self, inner: usize, p: &Palette) -> String {
        let mut labels: Vec<String> = self
            .call
            .questions
            .iter()
            .map(|q| {
                let tick = if self.answer_text(q).is_some() {
                    "✓"
                } else {
                    " "
                };
                format!(
                    "{tick} {}",
                    crate::sanitize::safe(q.header.as_deref().unwrap_or(t("Question")))
                )
            })
            .collect();
        labels.push(t("Confirm").to_owned());
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
        let (inner, lines) = self.layout(width, max_rows);
        let origin = if self.call.branch_id == "main" {
            String::new()
        } else {
            line_text(
                &Line::text(tf(
                    "Question on branch {branch}",
                    &[("branch", &self.call.branch_id)],
                )),
                inner,
                p,
            )
        };
        let mut content = vec![self.tab_row(inner, p), origin];
        if let Some(error) = &self.error {
            content.push(line_text(&Line::warn(error), inner, p));
        }
        for line in lines {
            content.push(line_text(&line, inner, p));
        }
        let hint = key_hints(&self.hint(inner), p);
        bordered(&content, width, None, &p.border, None, Some(&hint))
    }

    /// The body lines drawn inside the border, roomy when they fit.
    fn layout(&self, width: usize, max_rows: usize) -> (usize, Vec<Line>) {
        const CHROME: usize = 4;
        let inner = width.saturating_sub(4).max(1);
        let limit = max_rows
            .saturating_sub(CHROME + usize::from(self.error.is_some()))
            .max(1);
        let roomy = self.body(inner, true);
        let body = if body_rows(&roomy) <= limit {
            roomy
        } else {
            self.body(inner, false)
        };
        (inner, fit_body(&body, limit))
    }

    fn hint(&self, width: usize) -> String {
        if self.sending {
            return t("Sending answer…").to_owned();
        }
        if self.typing {
            return t("Enter save · Esc back").to_owned();
        }
        let (full, compact) = if self.on_confirm_tab() {
            (
                "Enter submit · 1-9 edit · Tab/←→ back · Esc decline",
                "Enter submit · Tab edit · Esc decline",
            )
        } else if self
            .question()
            .is_some_and(|question| question.multi_select)
        {
            (
                "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline",
                "Space toggle · Tab next · Esc decline",
            )
        } else {
            (
                "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline",
                "1-9 select · Tab next · Esc decline",
            )
        };
        if visible_width(t(full)) <= width {
            t(full).to_owned()
        } else {
            t(compact).to_owned()
        }
    }

    fn body(&self, width: usize, roomy: bool) -> Body {
        if self.on_confirm_tab() {
            return self.review(roomy);
        }
        let Some(question) = self.question() else {
            return Body {
                head: vec![Line::warn(t(
                    "AskUserQuestion received an invalid payload.",
                ))],
                rows: Vec::new(),
            };
        };
        let mut head: Vec<Line> = wrap_cells(&question.question, width)
            .into_iter()
            .map(Line::text)
            .collect();
        if self.missing_tab == Some(self.active_tab) {
            head.push(Line::warn(tf(
                "Missing answer for Q{index}",
                &[("index", &(self.active_tab + 1).to_string())],
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
        let multi = question.multi_select;
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
        if !question.allow_custom {
            return Body { head, rows };
        }
        let index = question.options.len();
        let selected = self.option_index == index;
        let stored = self
            .custom_answers
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        let draft = if self.typing {
            self.draft.text().to_owned()
        } else {
            stored.clone()
        };
        let prefix = row_prefix(
            selected,
            index,
            index_width,
            mark(multi, !stored.is_empty()),
        );
        let label = format!("{prefix}{}", t("Type your own answer"));
        let lines = if !self.typing && draft.is_empty() {
            vec![Line::choice(label, selected)]
        } else {
            let field = |lead: &str, line: fn(String, bool) -> Line| {
                let lead_width = display_width(lead);
                if !self.typing {
                    return line(format!("{lead}{draft}"), selected);
                }
                let (window, column) = self.draft.view(width.saturating_sub(lead_width).max(1));
                let mut line = line(format!("{lead}{window}"), selected);
                line.caret = Some(lead_width + column);
                line
            };
            if roomy {
                vec![
                    Line::choice(label, selected),
                    field(&detail_indent, Line::detail),
                ]
            } else {
                vec![field(&format!("{label}: "), Line::choice)]
            }
        };
        rows.push(Row { lines, selected });
        Body { head, rows }
    }

    fn review(&self, roomy: bool) -> Body {
        let mut head = vec![Line {
            text: t("Review your answers and press Enter to submit.").to_owned(),
            tone: Tone::Detail,
            caret: None,
        }];
        if roomy {
            head.push(Line::text(String::new()));
        }
        let index_width = self.call.questions.len().to_string().len();
        let rows = self
            .call
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let answer = self.answer_text(question);
                let label = format!(
                    "{}{:>index_width$} {}",
                    " ".repeat(2),
                    index + 1,
                    question.header.as_deref().unwrap_or(t("Question"))
                );
                let shown = answer
                    .clone()
                    .unwrap_or_else(|| t("Not answered").to_owned());
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
                            caret: None,
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
                        caret: None,
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
    /// The cell column of the edit caret, on the line being typed into.
    caret: Option<usize>,
}

impl Line {
    fn text(text: String) -> Self {
        Self {
            text,
            tone: Tone::Text,
            caret: None,
        }
    }
    fn warn(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Warning,
            caret: None,
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
            caret: None,
        }
    }
    fn choice(text: String, selected: bool) -> Self {
        Self {
            text,
            tone: if selected { Tone::Selected } else { Tone::Text },
            caret: None,
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
        &truncate_cells(&crate::sanitize::safe(&line.text), width),
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
    use crate::question::Choice;
    use crate::ui::theme::{Appearance, ColorMode, Theme};

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

    fn dinner(multi_select: bool) -> AskUserPrompt {
        AskUserPrompt::new(Call {
            request_id: "ask-1".into(),
            branch_id: "main".into(),
            call_id: "call-1".into(),
            questions: vec![Question {
                id: "dinner".into(),
                header: Some("Dinner".into()),
                question: "What should we eat tonight?".into(),
                multi_select,
                allow_custom: true,
                options: vec![
                    Choice {
                        label: "Hotpot".into(),
                        description: Some("Takes longer".into()),
                    },
                    Choice {
                        label: "Ramen".into(),
                        description: None,
                    },
                    Choice {
                        label: "Fried rice".into(),
                        description: None,
                    },
                ],
            }],
        })
    }

    #[test]
    fn permission_keys_map_to_decisions() {
        let mut p = PermissionPrompt::new(
            "r1".into(),
            "main".into(),
            "source: tool_rule".into(),
            "exec".into(),
            serde_json::json!({"command": "npm test"}),
            true,
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
                allow, remember, ..
            } => assert!(allow && remember),
            other => panic!("{other:?}"),
        }
        let mut p = PermissionPrompt::new(
            "r2".into(),
            "main".into(),
            String::new(),
            "edit".into(),
            serde_json::json!({"path": "a"}),
            false,
        );
        assert!(matches!(
            p.handle_key(key(KeyCode::Char('a'))),
            Effect::None
        ));
        p.handle_key(key(KeyCode::Right));
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter)),
            Effect::Approval { allow: false, .. }
        ));
        p.handle_key(key(KeyCode::Right));
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), Effect::None));
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), Effect::None));
        assert!(matches!(p.handle_key(key(KeyCode::Enter)), Effect::None));
        assert!(matches!(
            p.handle_key(key(KeyCode::Char('y'))),
            Effect::FullAccess { .. }
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
            false,
        );
        let rows: Vec<String> = p
            .render(100, &theme.palette)
            .iter()
            .map(|r| strip(r))
            .collect();
        assert!(rows[0].starts_with("╭─ Permission required ─"));
        assert!(rows[2].contains("# source: tool_rule"));
        assert!(rows[3].contains("$ edit {\"path\":\"src/index.ts\"}"));
        assert!(rows[5].contains("› Allow once   "));
        assert!(!rows[5].contains("Allow always"));
        assert!(rows[5].contains("Reject   "));
        assert!(rows[5].contains("Full access"));
        assert!(rows[7].ends_with("─ y allow · n reject · ←→/Tab move · Enter confirm ─╯"));
    }

    #[test]
    fn multi_select_collects_two_answers_through_the_review_tab() {
        let mut ask = dinner(true);
        ask.handle_key(key(KeyCode::Char(' ')));
        ask.handle_key(key(KeyCode::Down));
        ask.handle_key(key(KeyCode::Down));
        ask.handle_key(key(KeyCode::Char(' ')));
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
                    answers,
                    vec![Answer {
                        question_id: "dinner".into(),
                        selected: vec!["Hotpot".into(), "Fried rice".into()],
                        custom: None
                    }]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn single_select_enter_advances_and_review_flags_missing_answers() {
        let mut ask = dinner(false);
        ask.set_tab(1);
        assert_eq!(ask.handle_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!(ask.active_tab, 0);
        assert_eq!(ask.missing_tab, Some(0));
        ask.handle_key(key(KeyCode::Char('2')));
        assert!(ask.on_confirm_tab());
        assert!(matches!(
            ask.handle_key(key(KeyCode::Enter)),
            Effect::Answers { .. }
        ));
    }

    #[test]
    fn custom_answer_is_typed_and_saved() {
        let mut ask = dinner(false);
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
                assert_eq!(
                    answers,
                    vec![Answer {
                        question_id: "dinner".into(),
                        selected: vec![],
                        custom: Some("pizza".into())
                    }]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ask_user_panel_rows_match_the_reference() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let ask = dinner(true);
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
            text.last().unwrap().ends_with(
                "─ ↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline ─╯"
            )
        );
        let narrow = ask.render(48, 18, &theme.palette);
        assert!(strip(narrow.last().unwrap()).contains("Space toggle · Tab next · Esc decline"));
        assert!(
            narrow
                .iter()
                .all(|row| crate::ui::text::display_width(&strip(row)) <= 48)
        );
    }

    #[test]
    fn the_typed_answer_edits_like_the_composer_and_carries_the_caret() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let mut ask = dinner(false);
        for _ in 0..3 {
            ask.handle_key(key(KeyCode::Down));
        }
        for c in "面条".chars() {
            ask.handle_key(key(KeyCode::Char(c)));
        }
        ask.handle_key(key(KeyCode::Left));
        ask.paste("牛肉\n");
        assert_eq!(ask.draft.text(), "面牛肉 条");
        let rows: Vec<String> = ask
            .render(100, 20, &theme.palette)
            .iter()
            .map(|r| strip(r))
            .collect();
        let (row, column) = ask.cursor(100, 20).unwrap();
        assert!(rows[row].contains("面牛肉 条"), "{rows:?}");
        let value = rows[row].find("面").unwrap();
        assert_eq!(column, display_width(&rows[row][..value]) + 7);
        assert!(!rows.concat().contains('▌'));
        ask.handle_key(key(KeyCode::Enter));
        assert!(ask.cursor(100, 20).is_none());
        assert_eq!(ask.custom_answers["dinner"], "面牛肉 条");

        let mut ask = dinner(true);
        ask.handle_key(key(KeyCode::Char(' ')));
        for _ in 0..3 {
            ask.handle_key(key(KeyCode::Down));
        }
        ask.handle_key(key(KeyCode::Char('饺')));
        let text = strip(&ask.render(100, 20, &theme.palette).concat());
        assert!(text.contains("[x] Hotpot") && text.contains("[x] Type your own answer"));
        ask.handle_key(key(KeyCode::Esc));
        assert!(!ask.typing);
        assert_eq!(ask.collect().unwrap()[0].custom.as_deref(), Some("饺"));
        ask.handle_key(key(KeyCode::Enter));
        ask.handle_key(key(KeyCode::Backspace));
        assert!(!ask.custom_answers.contains_key("dinner"));
        ask.handle_key(key(KeyCode::Char('面')));
        ask.handle_key(key(KeyCode::Enter));
        assert!(!ask.typing && ask.on_confirm_tab());

        let mut ask = dinner(false);
        ask.call.questions[0].allow_custom = false;
        for _ in 0..5 {
            ask.handle_key(key(KeyCode::Down));
        }
        ask.handle_key(key(KeyCode::Char('x')));
        assert!(!ask.typing);
        let text = ask.render(100, 20, &theme.palette).concat();
        assert!(!text.contains("Type your own answer"));
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
