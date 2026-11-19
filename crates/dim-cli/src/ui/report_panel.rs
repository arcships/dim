use std::cell::Cell;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use crate::report::{
    Action, Draft, EMAIL, EMAIL_SUBJECT, EvidenceUnavailable, GeneratedReport, MailScope, Plan,
    Reply, ReportFiles, Target, mailto,
};
use crate::ui::i18n::{t, tf};
use crate::ui::line_input::LineInput;
use crate::ui::render::panel::{bordered, follow_window, wrap};
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::text::truncate_cells;
use crate::ui::theme::{Palette, ansi::fg};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Prepare,
    Plan,
    Generate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultControl {
    Save,
    Preview,
    Email,
    MailDetails,
    GitHub,
    Discord,
    Close,
}

#[derive(Clone, Copy)]
enum DraftControl {
    Preview,
    Redact,
    Conversation,
    Scope(MailScope),
    Email,
    Options,
    Generate,
    Cancel,
}

#[derive(Clone, Copy)]
enum View {
    Preview,
    MailDetails,
}

pub struct Panel {
    pub id: u64,
    pub draft: Option<Draft>,
    target: Option<Target>,
    model: Option<dim_agent_types::ModelId>,
    generated: Option<GeneratedReport>,
    plan: Option<Plan>,
    busy: Option<Busy>,
    note: String,
    redact: bool,
    include_conversation: bool,
    editing: bool,
    options: LineInput,
    instructions: String,
    focus: usize,
    caret: Cell<Option<(usize, usize)>>,
    saved: Option<PathBuf>,
    planned_files: Option<ReportFiles>,
    files_match: bool,
    email_pending: bool,
    scope: MailScope,
    view: Option<View>,
    scroll: Cell<usize>,
    page: Cell<usize>,
    mail_details: String,
}

pub enum Input {
    None,
    Send(Action),
    Close,
}

impl Panel {
    pub fn new(id: u64, target: Target) -> Self {
        Self {
            id,
            draft: None,
            target: Some(target),
            model: None,
            generated: None,
            plan: None,
            busy: Some(Busy::Prepare),
            note: String::new(),
            redact: true,
            include_conversation: false,
            editing: false,
            options: LineInput::new(""),
            instructions: String::new(),
            focus: 0,
            caret: Cell::new(None),
            saved: None,
            planned_files: None,
            files_match: false,
            email_pending: false,
            scope: MailScope::default(),
            view: None,
            scroll: Cell::new(0),
            page: Cell::new(1),
            mail_details: String::new(),
        }
    }

    pub fn reply(
        &mut self,
        reply: Reply,
        workspace: &Path,
        open: impl FnOnce(&str) -> bool,
    ) -> Option<Action> {
        let email = std::mem::take(&mut self.email_pending) && self.busy == Some(Busy::Generate);
        self.busy = None;
        self.note.clear();
        match reply {
            Reply::Prepared(session, prepared) => {
                self.model = prepared.default_model;
                self.draft = Some(Draft {
                    session,
                    report_id: prepared.report_id,
                    plan_id: prepared.plan.plan_id.clone(),
                });
                if !self.redact || self.include_conversation || !self.instructions.is_empty() {
                    return self.replan();
                }
                self.accept_plan(prepared.plan);
            }
            Reply::Plan(plan) => {
                if let Some(draft) = &mut self.draft {
                    draft.plan_id.clone_from(&plan.plan_id);
                }
                self.accept_plan(plan);
            }
            Reply::Generated(report) => {
                self.draft = None;
                let planned = self.planned_files.take();
                self.files_match = planned.is_some() && planned.as_ref() == report.files.as_ref();
                if planned.is_some() && !self.files_match {
                    self.note =
                        t("Generated files differ from the plan preview. Email is unavailable.")
                            .into();
                }
                self.generated = Some(report);
                self.focus = 0;
                if email && self.files_match {
                    self.email(workspace, |uri| Some(open(uri)));
                }
            }
            Reply::Failed {
                message,
                draft_missing,
            } => {
                if draft_missing {
                    self.draft = None;
                }
                self.clear_plan();
                self.note = crate::sanitize::safe_line(&message);
            }
        }
        None
    }

    pub fn disconnected(&mut self, message: &str) {
        self.target = None;
        self.draft = None;
        self.busy = None;
        self.editing = false;
        if self.generated.is_none() {
            self.clear_plan();
            self.focus = 0;
        }
        self.note = crate::sanitize::safe_line(message);
    }

    pub fn paste(&mut self, text: &str) {
        if self.editing {
            self.options.paste(text);
        }
    }

    fn replan(&mut self) -> Option<Action> {
        let (busy, action) = match self.draft.clone() {
            Some(draft) => (
                Busy::Plan,
                Action::Plan {
                    draft,
                    redact: self.redact,
                    include_conversation: self.include_conversation,
                    instructions: self.instructions.clone(),
                },
            ),
            None => (Busy::Prepare, Action::Prepare(self.target.clone()?)),
        };
        self.busy = Some(busy);
        self.clear_plan();
        self.note.clear();
        self.scroll.set(0);
        Some(action)
    }

    fn accept_plan(&mut self, plan: Plan) {
        if !self.include_conversation && !plan.evidence.conversation.is_empty() {
            self.note = t("Report plan included conversation without permission.").into();
        } else {
            self.plan = Some(plan);
        }
    }

    fn clear_plan(&mut self) {
        self.plan = None;
        self.planned_files = None;
        self.files_match = false;
        self.email_pending = false;
        self.view = None;
    }

    fn preview_files(&self) -> Option<&ReportFiles> {
        if let Some(report) = &self.generated {
            report.files.as_ref()
        } else {
            self.plan.as_ref()?.files.as_ref()
        }
    }

    fn show(&mut self, view: View) {
        self.scroll.set(0);
        self.page.set(1);
        self.view = Some(view);
    }

    fn preview(&mut self) {
        if self.preview_files().is_some() {
            self.show(View::Preview);
        }
    }

    pub fn key(&mut self, key: KeyEvent, workspace: &Path) -> Input {
        self.key_with_opener(key, workspace, crate::browser::open)
    }

    pub(super) fn key_with_opener(
        &mut self,
        key: KeyEvent,
        workspace: &Path,
        open: impl FnOnce(&str) -> bool,
    ) -> Input {
        if self.view.is_some() {
            let row = self.scroll.get();
            let page = self.page.get();
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.view = None;
                    self.scroll.set(0);
                }
                KeyCode::Up => self.scroll.set(row.saturating_sub(1)),
                KeyCode::Down => self.scroll.set(row.saturating_add(1)),
                KeyCode::PageUp => self.scroll.set(row.saturating_sub(page)),
                KeyCode::PageDown => self.scroll.set(row.saturating_add(page)),
                KeyCode::Home => self.scroll.set(0),
                KeyCode::End => self.scroll.set(usize::MAX),
                _ => {}
            }
            return Input::None;
        }
        if key.code == KeyCode::Esc {
            if self.editing {
                self.editing = false;
                return Input::None;
            }
            if self.busy == Some(Busy::Generate)
                && let Some(draft) = self.draft.clone()
            {
                self.email_pending = false;
                self.note = t("Cancelling generation…").into();
                return Input::Send(Action::Cancel {
                    draft,
                    generation: true,
                });
            }
            return Input::Close;
        }
        if self.editing {
            if key.code == KeyCode::Enter {
                if self.options.text().len() > 1024 {
                    self.note = t("Options must fit within 1 KiB").into();
                    return Input::None;
                }
                self.instructions = self.options.text().to_owned();
                self.editing = false;
                return self.replan().map_or(Input::None, Input::Send);
            }
            self.options.handle_key(key);
            return Input::None;
        }
        let (count, draft_control) = if self.generated.is_some() {
            (self.result_controls().len(), None)
        } else {
            let controls = self.draft_controls();
            (controls.len(), Some(controls[self.focus]))
        };
        match key.code {
            KeyCode::PageDown => self
                .scroll
                .set(self.scroll.get().saturating_add(self.page.get())),
            KeyCode::PageUp => self
                .scroll
                .set(self.scroll.get().saturating_sub(self.page.get())),
            KeyCode::Tab => self.focus = (self.focus + 1) % count,
            KeyCode::BackTab => self.focus = (self.focus + count - 1) % count,
            KeyCode::Up => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down => self.focus = (self.focus + 1).min(count - 1),
            KeyCode::Enter if self.generated.is_some() => {
                return self.generated_key(workspace, open);
            }
            KeyCode::Enter if matches!(draft_control, Some(DraftControl::Cancel)) => {
                return Input::Close;
            }
            KeyCode::Enter if self.busy.is_none() => {
                use DraftControl::*;
                let control = draft_control.expect("draft control");
                match control {
                    Preview => self.preview(),
                    Redact => {
                        self.redact.toggle();
                        return self.replan().map_or(Input::None, Input::Send);
                    }
                    Conversation => {
                        self.include_conversation.toggle();
                        return self.replan().map_or(Input::None, Input::Send);
                    }
                    Scope(scope) => self.scope = scope,
                    Options => {
                        self.options = LineInput::new(&self.instructions);
                        self.editing = true;
                    }
                    Generate if self.plan.is_none() => {
                        return self.replan().map_or(Input::None, Input::Send);
                    }
                    Email if self.preview_files().is_none() => {
                        self.note = t("Preview unavailable").into();
                    }
                    Email | Generate => {
                        if let Some(draft) = self.draft.clone() {
                            self.email_pending = matches!(control, Email);
                            self.busy = Some(Busy::Generate);
                            self.planned_files = self.plan.take().and_then(|plan| plan.files);
                            self.note.clear();
                            return Input::Send(Action::Generate(draft));
                        }
                    }
                    Cancel => unreachable!("handled even while busy"),
                }
            }
            _ => {}
        }
        Input::None
    }

    pub fn saved_path(&self) -> Option<&Path> {
        self.saved.as_deref()
    }

    fn result_controls(&self) -> Vec<ResultControl> {
        use ResultControl::*;
        let mut controls = vec![Save];
        if self.preview_files().is_some() {
            controls.push(Preview);
        }
        if self.files_match {
            controls.push(Email);
            if self.saved.is_some() {
                controls.push(MailDetails);
            }
        }
        if self.saved.is_some() {
            controls.extend([GitHub, Discord]);
        }
        controls.push(Close);
        controls
    }

    fn generated_key(&mut self, workspace: &Path, open: impl FnOnce(&str) -> bool) -> Input {
        use ResultControl::*;
        let control = self.result_controls()[self.focus];
        match control {
            Save => {
                if self.saved.is_none() {
                    self.save(workspace);
                }
            }
            Preview => self.preview(),
            Email => self.email(workspace, |uri| Some(open(uri))),
            MailDetails => self.email(workspace, |_| None),
            GitHub | Discord => {
                let report = self.generated.as_ref().expect("generated report");
                let (url, host, path) = if control == GitHub {
                    (
                        &report.sharing.github_issue_url,
                        "github.com",
                        "/arcships/dim/issues/new",
                    )
                } else {
                    (&report.sharing.discord_url, "discord.gg", "/ds35K3TadP")
                };
                let valid = url::Url::parse(url).ok().filter(|url| {
                    url.scheme() == "https"
                        && url.host_str() == Some(host)
                        && url.path() == path
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none()
                });
                self.note = if valid.is_some_and(|url| open(url.as_str())) {
                    if control == Discord {
                        crate::clipboard::copy_local(&report.sharing.description);
                        t("Opened Discord")
                    } else {
                        t("Opened GitHub issue draft")
                    }
                } else {
                    t("Could not open the share destination")
                }
                .into();
            }
            Close => return Input::Close,
        }
        Input::None
    }

    fn save(&mut self, workspace: &Path) {
        self.note = match self
            .generated
            .as_ref()
            .expect("generated report")
            .zip_bytes()
            .and_then(|bytes| save_zip(workspace, &bytes))
        {
            Ok(path) => {
                self.saved = Some(path);
                String::new()
            }
            Err(error) => format!("{} {error}", t("Could not save ZIP")),
        };
    }

    fn email(&mut self, workspace: &Path, open: impl FnOnce(&str) -> Option<bool>) {
        if self.saved.is_none() {
            self.save(workspace);
        }
        let Some(path) = &self.saved else {
            return;
        };
        let Ok(bytes) = self
            .generated
            .as_ref()
            .expect("generated report")
            .zip_bytes()
        else {
            self.note = t("Invalid report ZIP").into();
            return;
        };
        let verified = (|| -> std::io::Result<bool> {
            if !std::fs::symlink_metadata(path)?.is_file() {
                return Ok(false);
            }
            let file = std::fs::File::open(path)?;
            let mut saved = Vec::with_capacity(bytes.len());
            file.take(bytes.len() as u64 + 1).read_to_end(&mut saved)?;
            Ok(saved == bytes)
        })();
        if !matches!(verified, Ok(true)) {
            self.saved = None;
            self.focus = 0;
            self.note = t("Saved ZIP missing, unreadable, or changed. Save a new copy.").into();
            self.scroll.set(0);
            return;
        }
        self.note.clear();
        let body = self.scope.body(&bytes);
        let instruction = match open(&mailto(&body)) {
            Some(true) => t(
                "Attach the saved ZIP manually. If no draft opens, use these details. Nothing was sent.",
            ),
            Some(false) => t("Could not launch email. Compose manually and attach the saved ZIP."),
            None => t("Compose manually and attach the saved ZIP. Nothing was sent."),
        };
        self.mail_details = format!(
            "{instruction}\n{} {}\n{}\n{}\n{}",
            t("Saved ZIP"),
            crate::sanitize::safe_line(&path.display().to_string()),
            tf("To: {email}", &[("email", EMAIL)]),
            tf("Subject: {subject}", &[("subject", EMAIL_SUBJECT)]),
            body.replace("\r\n", "\n"),
        );
        self.show(View::MailDetails);
    }

    fn draft_controls(&self) -> Vec<DraftControl> {
        use DraftControl::*;
        if self.target.is_none() {
            return vec![Cancel];
        }
        let mut controls = vec![Preview, Redact];
        if matches!(&self.target, Some(Target::Session(_))) {
            controls.push(Conversation);
        }
        controls.extend([
            Scope(MailScope::ReportOnly),
            Scope(MailScope::InternalRegression),
            Email,
            Options,
            Generate,
            Cancel,
        ]);
        controls
    }

    fn controls(&self) -> Vec<&str> {
        if self.generated.is_some() {
            self.result_controls()
                .into_iter()
                .map(|control| match control {
                    ResultControl::Save => t("Save ZIP"),
                    ResultControl::Preview => t("Preview ZIP files"),
                    ResultControl::Email => t("Open email draft"),
                    ResultControl::MailDetails => t("Email details"),
                    ResultControl::GitHub => t("Draft a GitHub issue"),
                    ResultControl::Discord => t("Give feedback on Discord"),
                    ResultControl::Close => t("Close"),
                })
                .collect()
        } else {
            use DraftControl::*;
            self.draft_controls()
                .into_iter()
                .map(|control| match control {
                    Preview if self.preview_files().is_some() => t("Local attachment preview"),
                    Preview if self.busy.is_some() => t("Preview pending"),
                    Preview => t("Preview unavailable"),
                    Redact if self.redact => t("Redact [x]"),
                    Redact => t("Redact [ ]"),
                    Conversation if self.include_conversation => t("[x] Include conversation"),
                    Conversation => t("[ ] Include conversation"),
                    Scope(MailScope::ReportOnly) => match self.scope {
                        MailScope::ReportOnly => t("(o) Investigation only"),
                        MailScope::InternalRegression => t("( ) Investigation only"),
                    },
                    Scope(MailScope::InternalRegression) => match self.scope {
                        MailScope::ReportOnly => t("( ) Team-internal regression of this ZIP only"),
                        MailScope::InternalRegression => {
                            t("(o) Team-internal regression of this ZIP only")
                        }
                    },
                    Email => t("Open email draft"),
                    Options => t("Options"),
                    Generate if self.busy.is_some() || self.plan.is_some() => t("Generate report"),
                    Generate if self.draft.is_some() => t("Refresh report plan"),
                    Generate => t("Prepare report again"),
                    Cancel if self.target.is_none() => t("Close"),
                    Cancel => t("Cancel"),
                })
                .collect()
        }
    }

    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.caret.get()
    }

    pub fn render(&self, width: usize, budget: usize, p: &Palette) -> Vec<String> {
        self.caret.set(None);
        let title = if self.generated.is_some() {
            t("Report ready")
        } else {
            match &self.target {
                Some(Target::Session(_)) => t("Report this session"),
                Some(Target::Error(_)) => t("Report this error"),
                None => t("Report"),
            }
        };
        if budget < 3 || width < 8 {
            return vec![truncate_cells(title, width)];
        }
        let inner = width - 4;
        let height = budget - 2;
        let mut rows = Vec::new();
        if let Some(view) = self.view {
            let (title, text) = match view {
                View::Preview => (
                    t("Preview ZIP files"),
                    self.preview_files()
                        .map(ReportFiles::preview)
                        .unwrap_or_default(),
                ),
                View::MailDetails => (t("Email details"), self.mail_details.clone()),
            };
            let content: Vec<_> = text
                .split('\n')
                .flat_map(|line| wrap(line, inner))
                .collect();
            let start = self.scroll.get().min(content.len().saturating_sub(height));
            self.scroll.set(start);
            self.page.set(height);
            rows.extend(content.into_iter().skip(start).take(height));
            let hint = truncate_cells(t("↑↓/PgUp/PgDn scroll · Esc back"), inner.saturating_sub(2));
            return bordered(&rows, width, None, &p.border, Some(title), Some(&hint));
        }
        if self.editing {
            rows.push(truncate_cells(
                t("Options · for example: omit timeline, keep error codes"),
                inner,
            ));
            if height > 1 {
                let (text, column) = self.options.view(inner);
                rows.push(text);
                self.caret.set(Some((2, 2 + column)));
                if height > 2 && !self.note.is_empty() {
                    rows.push(truncate_cells(&self.note, inner));
                }
            }
        } else {
            let controls = self.controls();
            let visible = controls.len().min((height / 2).max(1));
            let first = follow_window(0, self.focus, visible, controls.len());
            let body_rows = height.saturating_sub(visible);
            if let Some(path) = &self.saved {
                let path = crate::sanitize::safe_line(&path.display().to_string());
                rows.extend(wrap(&format!("{} {path}", t("Saved ZIP")), inner));
            }
            if let Some(files) = self.plan.as_ref().and_then(|plan| plan.files.as_ref()) {
                for (name, content) in [
                    ("report.md", &files.report),
                    ("diagnostics.json", &files.diagnostics),
                ] {
                    let excerpt = content
                        .lines()
                        .find(|s| !s.trim().is_empty() && !matches!(s.trim(), "{" | "["))
                        .unwrap_or(content);
                    rows.push(truncate_cells(
                        &format!("{name}: {}", crate::sanitize::safe_line(excerpt)),
                        inner,
                    ));
                }
            }
            rows.extend(
                crate::sanitize::safe(&self.summary())
                    .lines()
                    .flat_map(|line| crate::ui::render::panel::wrap_words(line, inner)),
            );
            let start = self.scroll.get().min(rows.len().saturating_sub(body_rows));
            self.scroll.set(start);
            self.page.set(body_rows.max(1));
            rows = rows.into_iter().skip(start).take(body_rows).collect();
            for (index, label) in controls.into_iter().enumerate().skip(first).take(visible) {
                let selected = self.focus == index;
                let line = truncate_cells(
                    &format!("{} {label}", if selected { "›" } else { " " }),
                    inner,
                );
                rows.push(fg(&line, if selected { &p.accent } else { &p.text_soft }));
            }
        }
        let hint = if self.editing {
            t("Enter apply · Esc back")
        } else {
            t("PgUp/PgDn details · ↑↓ move · Enter select · Esc close")
        };
        bordered(&rows, width, None, &p.border, Some(title), Some(hint))
    }

    fn summary(&self) -> String {
        let mut body = if let Some(report) = &self.generated {
            let guidance = if !self.files_match {
                t("Email unavailable without matching plan and ZIP previews.")
            } else if matches!(self.scope, MailScope::InternalRegression) {
                t(
                    "Team-internal regression of this ZIP only. No external providers, training, or publication.",
                )
            } else {
                t("Email is for investigating this report only, not evaluation.")
            };
            format!(
                "{guidance}\n{}\n{}\n{}",
                t("Generate/save never uploads. Share manually after saving."),
                crate::sanitize::safe_line(&report.title),
                t("ZIP contents: report.md, diagnostics.json")
            )
        } else {
            let mut body = match &self.target {
                Some(Target::Session(_)) if !self.include_conversation => {
                    format!("{}\n", t("Diagnostics only; no conversation."))
                }
                Some(Target::Session(_)) => {
                    let status = match &self.plan {
                        Some(plan) if plan.evidence.conversation.is_empty() => {
                            t("No conversation excerpts in this plan.")
                        }
                        Some(_) if self.redact => t(
                            "Conversation included; redaction does not remove all sensitive text.",
                        ),
                        Some(_) => t("Conversation included without redaction."),
                        None => t("Conversation requested; check the updated preview."),
                    };
                    format!(
                        "{status}\n{}\n",
                        t("Check the preview for sensitive text before sharing.")
                    )
                }
                Some(Target::Error(_)) => {
                    format!(
                        "{}\n",
                        t("Error diagnostics only; no session text is available.")
                    )
                }
                None => String::new(),
            };
            if matches!(self.scope, MailScope::InternalRegression) {
                body.push_str(t("Team-internal regression of this ZIP only. No external providers, training, or publication."));
                body.push('\n');
            }
            body.push_str(t(
                "Generate/save never uploads. Share manually after saving.",
            ));
            body.push('\n');
            if let Some(Target::Error(error)) = &self.target {
                body.push_str(&crate::sanitize::safe_line(&error.message));
                body.push('\n');
            }
            if let Some(model) = &self.model {
                body.push_str(&format!(
                    "{} {}/{}\n",
                    t("Session model"),
                    model.provider_id,
                    model.model_id
                ));
            }
            body.push_str(t("Local generation makes zero model requests."));
            if let Some(plan) = &self.plan {
                body.push('\n');
                let session = matches!(&self.target, Some(Target::Session(_)));
                body.push_str(if let Some(timeline) = &plan.evidence.timeline {
                    if timeline.recent.is_empty() {
                        t("Evidence · No recent session activity")
                    } else {
                        t("Evidence · Recent session activity")
                    }
                } else {
                    match plan.evidence.unavailable.as_ref() {
                        Some(EvidenceUnavailable::Omitted) => t("Evidence · Timeline omitted"),
                        Some(EvidenceUnavailable::ReadFailed) => {
                            t("Evidence · Timeline read failed")
                        }
                        Some(EvidenceUnavailable::Unknown) => t("Evidence · Timeline unavailable"),
                        _ if session => t("Evidence · Timeline unavailable"),
                        Some(EvidenceUnavailable::NoReference) | None => {
                            t("Evidence · Observed error only")
                        }
                    }
                });
                if matches!(
                    plan.instructions_status.as_str(),
                    "partially_applied" | "unsupported"
                ) {
                    body.push('\n');
                    body.push_str(t("Some options were unsupported."));
                }
            }
            body
        };
        let note = match self.busy {
            Some(Busy::Prepare) => t("Preparing report…"),
            Some(Busy::Plan) => t("Updating report plan…"),
            Some(Busy::Generate) if self.note.is_empty() => t("Generating report…"),
            _ => &self.note,
        };
        if !note.is_empty() {
            body = format!("{note}\n{body}");
        }
        body
    }
}

fn save_zip(workspace: &Path, bytes: &[u8]) -> crate::Result<PathBuf> {
    let path = workspace.join(format!("dim-report-{}.zip", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error.into());
    }
    Ok(path)
}
