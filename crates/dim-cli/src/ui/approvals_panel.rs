//! The `/approvals` panel: the default tool-approval mode, with a
//! confirmation before full access (port of `renderApprovals`).

use crate::ui::composer::palette::{footer, title};
use crate::ui::i18n::t;
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{IDLE_MARKER, SELECTED_MARKER, bordered, confirm_panel};
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};
use crate::ui::transcript::blocks::truncate_cells;

/// `(wire mode, title, status label, description)` in display order. The
/// texts are English keys: draw them through [`t`].
pub const MODES: [(&str, &str, &str, &str); 3] = [
    (
        "auto",
        "Full access",
        "full access",
        "All tools run automatically, bypass permissions",
    ),
    (
        "approve-for-me",
        "Approve for me",
        "approve for me",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient.",
    ),
    (
        "normal",
        "Default permissions",
        "default",
        "Read tools run automatically, write tools require confirmation",
    ),
];

const HINT: &str = "↑↓/tab move · enter apply · esc back";
const CONFIRM_HINT: &str = "Tab switch  Enter confirm  Esc cancel";
const CONFIRM_BODY: &str = "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task.";

/// The status-line label of a mode.
pub fn status_label(mode: &str) -> &'static str {
    MODES
        .iter()
        .find(|(m, ..)| *m == mode)
        .map_or(t("default"), |(_, _, status, _)| t(status))
}

pub struct ApprovalsPanel {
    current: String,
    selected: usize,
    /// `Some(focused button)` while full access awaits confirmation:
    /// 0 = Confirm, 1 = Cancel.
    confirming: Option<usize>,
}

impl ApprovalsPanel {
    pub fn new(current: &str) -> Self {
        let selected = MODES.iter().position(|(m, ..)| *m == current).unwrap_or(2);
        Self {
            current: current.to_owned(),
            selected,
            confirming: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if let Some(focus) = &mut self.confirming {
            return match key.code {
                KeyCode::Esc => {
                    self.confirming = None;
                    Effect::None
                }
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                    *focus = 1 - *focus;
                    Effect::None
                }
                KeyCode::Char('y') => Effect::SetApprovals("auto".into()),
                KeyCode::Char('n') => {
                    self.confirming = None;
                    Effect::None
                }
                KeyCode::Enter => {
                    if *focus == 0 {
                        Effect::SetApprovals("auto".into())
                    } else {
                        self.confirming = None;
                        Effect::None
                    }
                }
                _ => Effect::None,
            };
        }
        match key.code {
            KeyCode::Esc => Effect::Close,
            KeyCode::Up | KeyCode::BackTab => {
                self.selected = self.selected.saturating_sub(1);
                Effect::None
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1).min(MODES.len() - 1);
                Effect::None
            }
            KeyCode::Enter => {
                let mode = MODES[self.selected].0;
                if mode == self.current {
                    Effect::Close
                } else if mode == "auto" {
                    self.confirming = Some(1);
                    Effect::None
                } else {
                    Effect::SetApprovals(mode.to_owned())
                }
            }
            _ => Effect::None,
        }
    }

    pub fn render(&self, width: usize, max_rows: usize, p: &Palette) -> Vec<String> {
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
        let build = |roomy: bool, detail_lines: usize| -> Vec<String> {
            let mut content = vec![title(t("Tool Approvals"), inner, p)];
            if roomy {
                content.push(String::new());
            }
            for (index, (mode, name, _, description)) in MODES.iter().enumerate() {
                let selected = index == self.selected;
                let marker = if selected {
                    paint(
                        SELECTED_MARKER,
                        Paint {
                            fg: Some(&p.accent),
                            bold: true,
                            ..Paint::default()
                        },
                    )
                } else {
                    IDLE_MARKER.to_owned()
                };
                let label = paint(
                    &truncate_cells(t(name), inner.saturating_sub(2 + 9)),
                    Paint {
                        fg: Some(if selected { &p.accent } else { &p.text }),
                        bold: selected,
                        ..Paint::default()
                    },
                );
                let badge = if *mode == self.current {
                    format!("  {}", fg(t("current"), &p.success))
                } else {
                    String::new()
                };
                content.push(format!("{marker}{label}{badge}"));
                let color = if selected { &p.muted } else { &p.faint };
                let width = inner.saturating_sub(2).max(1);
                let wrapped = crate::ui::tui::wrap_words(t(description), width);
                let mut lines: Vec<String> = wrapped.iter().take(detail_lines).cloned().collect();
                if wrapped.len() > detail_lines
                    && let Some(last) = lines.last_mut()
                {
                    *last = format!("{last} {}", wrapped[detail_lines]);
                }
                for line in lines {
                    content.push(format!(
                        "{IDLE_MARKER}{}",
                        fg(&truncate_cells(&line, width), color)
                    ));
                }
            }
            if roomy {
                content.push(String::new());
            }
            content.push(footer(
                &format!("{} / {}", self.selected + 1, MODES.len()),
                t(HINT),
                inner,
                p,
            ));
            content
        };
        let content = [build(true, 2), build(true, 1), build(false, 1)]
            .into_iter()
            .find(|lines| lines.len() + 2 <= max_rows)
            .unwrap_or_else(|| build(false, 1));
        bordered(&content, width, None, &p.border, None)
    }
}
