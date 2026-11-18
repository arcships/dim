use std::cell::Cell;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use crate::feedback::{Sample, Submission};
use crate::feedback_transport::Intake;
use crate::model::view::ViewSnapshot;
use crate::ui::i18n::{t, tf};
use crate::ui::line_input::LineInput;
use crate::ui::render::panel::{bordered, wrap};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::truncate_cells;
use crate::ui::theme::{Palette, ansi::fg};

const NOTE_LIMIT: usize = 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct Feedback {
    send: Box<dyn Fn(Submission) -> Receiver<Option<uuid::Uuid>>>,
    host: String,
    sample: Sample,
    note: String,
    allow_eval: bool,
    editor: Option<LineInput>,
    too_long: bool,
    submission: Option<Submission>,
    state: State,
    focus: Choice,
    top: Cell<usize>,
    page: Cell<usize>,
    caret: Cell<Option<(usize, usize)>>,
}

enum State {
    Review,
    Sending(Receiver<Option<uuid::Uuid>>),
    Unconfirmed,
    Received(uuid::Uuid),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Back,
    Note,
    Eval,
    Send,
    Retry,
}

impl Feedback {
    pub(super) fn capture(snapshot: &ViewSnapshot, intake: Intake) -> Option<Self> {
        let destination = url::Url::parse(intake.destination()).ok()?;
        let host = format!(
            "{}:{}",
            destination.host_str()?,
            destination.port_or_known_default()?
        );
        let sample = Sample::capture(snapshot)?;
        Some(Self::new(
            sample,
            host,
            Box::new(move |submission| transmit(intake.clone(), submission)),
        ))
    }

    fn new(
        sample: Sample,
        host: String,
        send: Box<dyn Fn(Submission) -> Receiver<Option<uuid::Uuid>>>,
    ) -> Self {
        let submission = sample.prepare("", false).ok();
        Self {
            send,
            host,
            sample,
            note: String::new(),
            allow_eval: false,
            editor: None,
            too_long: false,
            submission,
            state: State::Review,
            focus: Choice::Back,
            top: Cell::new(0),
            page: Cell::new(0),
            caret: Cell::new(None),
        }
    }

    pub(super) fn open(&mut self) {
        self.editor = None;
        self.too_long = false;
        self.focus = Choice::Back;
        self.top.set(0);
        self.page.set(0);
    }

    pub(super) fn cursor(&self) -> Option<(usize, usize)> {
        self.caret.get()
    }

    pub(super) fn paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            // Don't copy unbounded clipboard text into the field.
            self.too_long = text.len() > NOTE_LIMIT;
            if !self.too_long {
                editor.paste(text);
            }
        }
    }

    fn prepare(&mut self) {
        self.submission = self.sample.prepare(&self.note, self.allow_eval).ok();
        self.open();
    }

    fn choices(&self) -> &[Choice] {
        use Choice::*;
        match self.state {
            State::Review if self.submission.is_some() => &[Back, Note, Eval, Send],
            State::Review => &[Back, Note, Eval],
            State::Unconfirmed => &[Back, Retry],
            State::Received(_) => &[Back],
            State::Sending(_) => &[],
        }
    }

    /// Returns true when this review should return to the existing report.
    pub(super) fn key(&mut self, key: KeyEvent) -> bool {
        if let Some(editor) = &mut self.editor {
            match key.code {
                KeyCode::Esc => self.editor = None,
                KeyCode::Enter => {
                    if editor.text().len() > NOTE_LIMIT {
                        self.too_long = true;
                    } else if !self.too_long {
                        let changed = self.note != editor.text();
                        self.note = editor.text().to_owned();
                        self.editor = None;
                        if changed {
                            self.prepare();
                        }
                    }
                }
                _ => {
                    editor.handle_key(key);
                    self.too_long = editor.text().len() > NOTE_LIMIT;
                }
            }
            return false;
        }
        let top = self.top.get();
        let page = self.page.get().max(1);
        match key.code {
            KeyCode::Up => self.top.set(top.saturating_sub(1)),
            KeyCode::Down => self.top.set(top.saturating_add(1)),
            KeyCode::PageUp => self.top.set(top.saturating_sub(page)),
            KeyCode::PageDown => self.top.set(top.saturating_add(page)),
            KeyCode::Home => self.top.set(0),
            KeyCode::End => self.top.set(usize::MAX),
            _ => {}
        }
        if key.code == KeyCode::Esc {
            return true;
        }
        let choices = self.choices();
        if choices.is_empty() {
            return false;
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.page.set(0);
                let at = choices.iter().position(|c| *c == self.focus).unwrap_or(0);
                self.focus = choices[if key.code == KeyCode::Tab {
                    (at + 1) % choices.len()
                } else {
                    (at + choices.len() - 1) % choices.len()
                }];
            }
            KeyCode::Enter if key.mods == Modifiers::NONE => match self.focus {
                Choice::Back => return true,
                Choice::Note => {
                    self.editor = Some(LineInput::new(&self.note));
                    self.too_long = false;
                }
                Choice::Eval => {
                    self.allow_eval = !self.allow_eval;
                    self.prepare();
                }
                Choice::Send | Choice::Retry if self.page.get() != 0 => {
                    if let Some(submission) = self.submission.clone() {
                        self.start(submission);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        false
    }

    fn start(&mut self, submission: Submission) {
        self.state = State::Sending((self.send)(submission));
        self.focus = Choice::Back;
        self.top.set(0);
    }

    pub(super) fn poll(&mut self) -> bool {
        let State::Sending(receive) = &self.state else {
            return false;
        };
        self.state = match receive.try_recv() {
            Ok(Some(receipt)) => State::Received(receipt),
            Ok(None) | Err(TryRecvError::Disconnected) => State::Unconfirmed,
            Err(TryRecvError::Empty) => return false,
        };
        self.open();
        true
    }

    pub(super) fn render(&self, width: usize, budget: usize, p: &Palette) -> Vec<String> {
        self.caret.set(None);
        self.page.set(0);
        let title = t("Private feedback");
        if width < 32 || budget < 8 {
            return wrap(
                t("Enlarge terminal to review/send · Esc back"),
                width.max(1),
            )
            .into_iter()
            .take(budget)
            .collect();
        }
        let inner = width - 4;
        let height = budget - 2;
        if let Some(editor) = &self.editor {
            let (text, column) = editor.view(inner);
            let mut rows = vec![truncate_cells(t("Optional note"), inner), text];
            self.caret.set(Some((2, 2 + column)));
            if self.too_long {
                rows.extend(wrap(t("Note must fit within 1 KiB"), inner));
            }
            return bordered(
                &rows,
                width,
                Some(budget),
                &p.border,
                Some(title),
                Some(t("Enter apply · Esc back")),
            );
        }
        let choices = self.choices();
        let (mut rows, mut footer) = (Vec::new(), Vec::new());
        for (index, choice) in choices.iter().enumerate() {
            let label = match choice {
                Choice::Back => t("Back to report"),
                Choice::Note => t("Optional note"),
                Choice::Eval if self.allow_eval => t("Reuse for private evaluation [x]"),
                Choice::Eval => t("Reuse for private evaluation [ ]"),
                Choice::Send => t("Confirm and send privately"),
                Choice::Retry => t("Retry the same submission"),
            };
            let selected = *choice == self.focus;
            // Reserve the whole selected control, never clip the active Send
            // behind earlier controls. Tab still reaches every action.
            if !selected {
                continue;
            }
            let line = format!("› {}/{} {label}", index + 1, choices.len());
            footer.extend(
                wrap(&line, inner)
                    .into_iter()
                    .map(|line| fg(&line, if selected { &p.accent } else { &p.text_soft })),
            );
        }
        let footer_rows = footer.len().min(height - 1);
        let page = height - footer_rows;
        let hint = t("↑↓/PgUp/PgDn scroll · Tab move · Enter select · Esc back");
        let mut content = Vec::new();
        match &self.state {
            State::Review => {}
            State::Sending(_) => content.push(t("Sending private feedback…").into()),
            State::Unconfirmed => content.push(
                t("No receipt confirmed. Retry sends the same bytes with the same ID.").into(),
            ),
            State::Received(receipt) => {
                content.push(tf(
                    "Private feedback received: {receipt}",
                    &[("receipt", &receipt.to_string())],
                ));
                content.push(t("Raw samples are kept for 30 days. Save your case ID and contact the pilot operator to withdraw; derived private tests are disabled within 48 hours.").into());
            }
        }
        if matches!(self.state, State::Review | State::Unconfirmed) {
            content.push(tf("To the Dim team via {host}", &[("host", &self.host)]));
            content.push(t("Sensitive: this session sample may contain code or private information. Review every field before sending.").into());
            content.push(t("Default: diagnosis only. Separately opt in to reuse as a private evaluation case. Never public or used for model training.").into());
            content.push(t("Raw samples are kept for 30 days. Save your case ID and contact the pilot operator to withdraw; derived private tests are disabled within 48 hours.").into());
            content.push(String::new());
            if let Some(submission) = &self.submission {
                content.push(t("Exact JSON to send:").into());
                content.extend(
                    display_json(&submission.body)
                        .split('\n')
                        .map(str::to_owned),
                );
            } else {
                content.push(t("Could not prepare private feedback. Nothing was sent.").into());
            }
        }
        let content: Vec<_> = content.iter().flat_map(|line| wrap(line, inner)).collect();
        let top = self.top.get().min(content.len().saturating_sub(page));
        self.top.set(top);
        self.page.set(page);
        rows.extend(content.into_iter().skip(top).take(page));
        rows.resize(page, String::new());
        rows.extend(footer.into_iter().take(footer_rows));
        bordered(
            &rows,
            width,
            Some(budget),
            &p.border,
            Some(title),
            Some(hint),
        )
    }
}

fn transmit(intake: Intake, submission: Submission) -> Receiver<Option<uuid::Uuid>> {
    let (tx, rx) = mpsc::channel();
    // Failure (including worker startup failure) never exposes transport errors,
    // credentials or response content. Retry retains the immutable submission.
    let _ = std::thread::Builder::new()
        .name("dim-private-feedback".into())
        .spawn(move || {
            let receipt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .and_then(|rt| {
                    rt.block_on(async {
                        tokio::time::timeout(SEND_TIMEOUT, intake.send(&submission))
                            .await
                            .ok()?
                            .ok()
                    })
                });
            let _ = tx.send(receipt);
        });
    rx
}

/// Keep all JSON content inspectable, including invisible terminal controls,
/// without interpreting it as ANSI or reserializing the submission.
fn display_json(body: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(body.len());
    for ch in body.chars() {
        if ch != '\n'
            && (ch.is_control()
                || matches!(ch, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'))
        {
            let _ = write!(out, "\\u{:04x}", u32::from(ch));
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::view::{RowKind, ViewRow};
    use crate::ui::theme::{Appearance, ColorMode, Theme};
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn fixture() -> (Feedback, Rc<RefCell<Vec<Submission>>>) {
        let snapshot = ViewSnapshot {
            focus: "main".into(),
            history_complete: true,
            rows: vec![Arc::new(ViewRow {
                row_id: "synthetic".into(),
                provisional_id: None,
                kind: RowKind::UserMessage,
                display: None,
                text: "Synthetic review sample".into(),
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
            })],
            ..ViewSnapshot::default()
        };
        let sent = Rc::new(RefCell::new(Vec::new()));
        let recorded = sent.clone();
        let feedback = Feedback::new(
            Sample::capture(&snapshot).unwrap(),
            "127.0.0.1:18787".into(),
            Box::new(move |submission| {
                recorded.borrow_mut().push(submission);
                let (tx, rx) = mpsc::channel();
                tx.send(None).unwrap();
                rx
            }),
        );
        (feedback, sent)
    }

    fn draw(feedback: &Feedback, width: usize, height: usize) -> String {
        let palette = Theme::system(ColorMode::Ansi16, Appearance::Dark, None).palette;
        crate::sanitize::safe(&feedback.render(width, height, &palette).join("\n"))
    }

    #[test]
    fn review_cancel_and_edits_never_send() {
        let (mut feedback, sent) = fixture();
        assert!(!feedback.allow_eval);
        assert!(feedback.key(key(KeyCode::Enter))); // Safe initial Back.
        feedback.open();
        draw(&feedback, 80, 24);
        feedback.key(key(KeyCode::Tab));
        feedback.key(key(KeyCode::Enter));
        feedback.paste("draft only");
        feedback.key(key(KeyCode::Esc)); // Discard the note.
        assert!(feedback.note.is_empty());
        feedback.key(key(KeyCode::Tab));
        feedback.key(key(KeyCode::Enter)); // Explicit evaluation opt-in.
        assert!(feedback.allow_eval);
        assert!(feedback.focus == Choice::Back);
        assert!(feedback.key(key(KeyCode::Esc)));
        assert!(sent.borrow().is_empty());
    }

    #[test]
    fn confirmation_requires_visible_controls_and_retry_is_identical() {
        let (mut feedback, sent) = fixture();
        for _ in 0..3 {
            feedback.key(key(KeyCode::Tab));
        }
        feedback.key(key(KeyCode::Enter)); // Not rendered yet.
        assert!(sent.borrow().is_empty());
        draw(&feedback, 20, 5);
        feedback.key(key(KeyCode::Enter)); // Too small to review.
        assert!(sent.borrow().is_empty());
        assert!(draw(&feedback, 32, 8).contains("4/4"));
        feedback.key(KeyEvent::new(KeyCode::Enter, Modifiers::CTRL));
        assert!(sent.borrow().is_empty());
        feedback.key(key(KeyCode::Enter));
        assert_eq!(sent.borrow().len(), 1);
        feedback.key(key(KeyCode::Enter)); // Sending cannot double-submit.
        assert_eq!(sent.borrow().len(), 1);
        assert!(feedback.key(key(KeyCode::Esc))); // Can leave during delivery.
        assert!(feedback.poll());
        assert!(feedback.focus == Choice::Back);
        feedback.key(key(KeyCode::Tab));
        draw(&feedback, 80, 24);
        feedback.key(key(KeyCode::Enter));
        let sent = sent.borrow();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].id, sent[1].id);
        assert_eq!(sent[0].body, sent[1].body);
    }

    #[test]
    fn preview_exposes_destination_fields_and_escapes_terminal_controls() {
        let (feedback, _) = fixture();
        let rendered = draw(&feedback, 2000, 80);
        assert!(rendered.contains("127.0.0.1:18787"));
        assert!(rendered.contains("allowEval"));
        assert!(rendered.contains("false"));
        assert!(rendered.contains("Synthetic review sample"));
        assert_eq!(
            display_json("a\u{1b}\u{202e}\t\nb"),
            "a\\u001b\\u202e\\u0009\nb"
        );
    }

    #[test]
    fn receipt_has_no_resend_and_note_limit_does_not_mutate_preview() {
        let (mut feedback, sent) = fixture();
        let original = feedback.submission.as_ref().unwrap().body.clone();
        feedback.key(key(KeyCode::Tab));
        feedback.key(key(KeyCode::Enter));
        feedback.paste(&"x".repeat(NOTE_LIMIT + 1));
        feedback.key(key(KeyCode::Enter));
        assert!(feedback.editor.is_some());
        assert_eq!(feedback.submission.as_ref().unwrap().body, original);
        feedback.key(key(KeyCode::Esc));
        feedback.state = State::Received(uuid::Uuid::nil());
        feedback.open();
        feedback.key(key(KeyCode::Tab));
        draw(&feedback, 80, 24);
        assert!(feedback.key(key(KeyCode::Enter)));
        assert!(sent.borrow().is_empty());
    }

    #[test]
    fn english_and_chinese_disclose_consent_retention_and_withdrawal() {
        use crate::ui::i18n::{Locale, t_in};
        for english in [
            "Review private feedback",
            "Back to report",
            "Optional note",
            "Reuse for private evaluation [x]",
            "Reuse for private evaluation [ ]",
            "Confirm and send privately",
            "Retry the same submission",
            "To the Dim team via {host}",
            "Sensitive: this session sample may contain code or private information. Review every field before sending.",
            "Default: diagnosis only. Separately opt in to reuse as a private evaluation case. Never public or used for model training.",
            "Raw samples are kept for 30 days. Save your case ID and contact the pilot operator to withdraw; derived private tests are disabled within 48 hours.",
            "Enlarge terminal to review/send · Esc back",
        ] {
            assert_eq!(t_in(Locale::En, english), english);
            assert_ne!(t_in(Locale::ZhCn, english), english);
        }
    }

    #[test]
    fn ordinary_report_and_private_cancel_do_not_send() {
        use crate::{
            report::Target,
            ui::report_panel::{Input, Panel},
        };
        let mut ordinary = Panel::new(1, Target::Session("synthetic".into()), None);
        assert!(matches!(
            ordinary.key(key(KeyCode::Esc), std::path::Path::new(".")),
            Input::Close
        ));
        let (feedback, sent) = fixture();
        let mut panel = Panel::new(2, Target::Session("synthetic".into()), Some(feedback));
        panel.key(key(KeyCode::BackTab), std::path::Path::new("."));
        assert!(matches!(
            panel.key(key(KeyCode::Enter), std::path::Path::new(".")),
            Input::None
        ));
        assert!(panel.reviewing());
        panel.key(key(KeyCode::Esc), std::path::Path::new("."));
        assert!(!panel.reviewing());
        assert!(matches!(
            panel.key(key(KeyCode::Esc), std::path::Path::new(".")),
            Input::Close
        ));
        assert!(sent.borrow().is_empty());
    }
}
