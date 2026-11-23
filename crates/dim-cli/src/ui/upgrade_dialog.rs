//! The update dialog: a newer version on this build's channel. A recognised
//! installation whose server this client manages updates with `Update now`,
//! which leaves the interface first; any other names the command to run.
//!
//! ```text
//! ╭──────────────────────────────────────────────────────────────╮
//! │ Update available                                             │
//! │                                                              │
//! │ Version 0.1.0-alpha.2 is available (you have 0.1.0-alpha.1). │
//! │                                                              │
//! │ › Update now   Skip this version   Close                     │
//! │                                                              │
//! │ ←→ move · Enter confirm · Esc close                          │
//! ╰──────────────────────────────────────────────────────────────╯
//! ```

use std::cell::Cell;

use crate::ui::i18n::{t, tf};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{bordered, choices, confirm_panel, visible_width, wrap_words};
use crate::ui::terminal::{KeyCode, KeyEvent};
use crate::ui::text::truncate_cells;
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;
use crate::update::Offer;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Choice {
    Update,
    Skip,
    Close,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Self::Update => t("Update now"),
            Self::Skip => t("Skip this version"),
            Self::Close => t("Close"),
        }
    }
}

pub struct UpgradeDialog {
    pub offer: Offer,
    /// `Update now` was refused; the dialog says why for as long as it still
    /// cannot leave.
    blocked: bool,
    focused: usize,
    offset: Cell<usize>,
}

/// Why `Update now` cannot leave yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// Work in the shown Session, or a prompt waiting for an answer.
    Busy,
    /// A Session was just switched to; the daemon has not reported every
    /// live line's state yet.
    Session,
    /// A Session switch the user asked for has not resolved: the target
    /// Session's activity is not known yet.
    Switch,
}

impl UpgradeDialog {
    pub fn new(offer: Offer) -> Self {
        Self {
            offer,
            blocked: false,
            focused: 0,
            offset: Cell::new(0),
        }
    }

    /// Says why `Update now` cannot leave yet; the dialog stays open.
    pub fn block(&mut self) {
        self.blocked = true;
    }

    fn choices(&self) -> &'static [Choice] {
        if self.offer.direct {
            &[Choice::Update, Choice::Skip, Choice::Close]
        } else {
            &[Choice::Skip, Choice::Close]
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let count = self.choices().len();
        match key.code {
            KeyCode::Right | KeyCode::Tab => self.focused = (self.focused + 1) % count,
            KeyCode::Left | KeyCode::BackTab => self.focused = (self.focused + count - 1) % count,
            KeyCode::Up => self.offset.set(self.offset.get().saturating_sub(1)),
            KeyCode::Down => self.offset.set(self.offset.get().saturating_add(1)),
            KeyCode::Esc => return Effect::Close,
            KeyCode::Enter => {
                return match self.choices()[self.focused.min(count - 1)] {
                    Choice::Update => Effect::UpgradeNow,
                    Choice::Skip => Effect::UpgradeSkip(self.offer.version.clone()),
                    Choice::Close => Effect::Close,
                };
            }
            _ => {}
        }
        Effect::None
    }

    /// `blocked`: why `Update now` cannot leave yet, while that holds.
    pub fn render(
        &self,
        width: usize,
        height: usize,
        blocked: Option<Blocked>,
        p: &Palette,
    ) -> Vec<String> {
        let version = self.offer.version.as_str();
        let mut body = vec![tf(
            "Version {latest} is available (you have {current}).",
            &[("latest", version), ("current", &self.offer.current)],
        )];
        if self.blocked
            && let Some(reason) = blocked
        {
            body.push(
                match reason {
                    Blocked::Busy => t(
                        "Finish or stop the current work first (a running turn, shell command, or pending prompt).",
                    ),
                    Blocked::Session => {
                        t("Waiting for this Session's state after the switch.")
                    }
                    Blocked::Switch => t("Waiting for the Session switch to finish."),
                }
                .to_owned(),
            );
        } else if !self.offer.direct {
            body.push(tf(
                "To update, exit DimCode and run: {command}",
                &[("command", &self.offer.manual)],
            ));
        }
        let labels: Vec<String> = self
            .choices()
            .iter()
            .map(|choice| choice.label().to_owned())
            .collect();
        let inner = width.saturating_sub(4).max(1);
        let options = choices(&labels, self.focused, p);
        let hint = t("←→ move · Enter confirm · Esc close");
        let wrapped: Vec<_> = body
            .iter()
            .flat_map(|line| {
                crate::sanitize::safe(line)
                    .replace('\t', " ")
                    .lines()
                    .flat_map(|line| wrap_words(line, inner))
                    .collect::<Vec<_>>()
            })
            .collect();
        if wrapped == body
            && visible_width(&options) <= inner
            && visible_width(hint) <= inner
            && body.len() + 8 <= height
        {
            self.offset.set(0);
            return confirm_panel(
                t("Update available"),
                &body,
                &labels,
                self.focused,
                hint,
                width,
                p,
            );
        }
        let page = height.saturating_sub(4).max(1);
        let offset = self.offset.get().min(wrapped.len().saturating_sub(page));
        self.offset.set(offset);
        let mut content: Vec<_> = wrapped
            .iter()
            .skip(offset)
            .take(page)
            .map(|line| fg(line, &p.text))
            .collect();
        content.push(if visible_width(&options) <= inner {
            options
        } else {
            let count = format!(" {}/{}", self.focused + 1, labels.len());
            let label =
                truncate_cells(&labels[self.focused], inner.saturating_sub(2 + count.len()));
            choices(&[format!("{label}{count}")], 0, p)
        });
        let hint = if visible_width(hint) <= inner {
            hint
        } else {
            "←→ Tab Enter Esc"
        };
        content.push(fg(&truncate_cells(hint, inner), &p.muted));
        let scroll = (wrapped.len() > page).then(|| {
            format!(
                "↑↓ {}–{}/{}",
                offset + 1,
                (offset + page).min(wrapped.len()),
                wrapped.len()
            )
        });
        bordered(
            &content,
            width,
            None,
            &p.border,
            Some(&truncate_cells(
                t("Update available"),
                width.saturating_sub(6),
            )),
            scroll.as_deref(),
        )
    }
}
