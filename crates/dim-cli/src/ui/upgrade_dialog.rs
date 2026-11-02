//! The update dialog (port of the TS `upgrade-dialog`): a newer version on
//! this build's channel, installed in place with the package manager this
//! copy came from, then a restart onto the same session.
//!
//! ```text
//! ╭──────────────────────────────────────────────────────────────╮
//! │ Update available                                             │
//! │                                                              │
//! │ Version 0.1.0-alpha.2 is available (you have 0.1.0-alpha.1). │
//! │ Will run: npm install -g --prefer-online … dimcode@0.1.0-…   │
//! │                                                              │
//! │ › Update now   Skip this version   Later                     │
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

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Offer,
    Installing,
    Installed,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Choice {
    Install,
    Skip,
    Later,
    Close,
    Restart,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Self::Install => t("Update now"),
            Self::Skip => t("Skip this version"),
            Self::Later => t("Later"),
            Self::Close => t("Close"),
            Self::Restart => t("Restart now"),
        }
    }
}

pub struct UpgradeDialog {
    pub offer: Offer,
    pub stage: Stage,
    focused: usize,
    offset: Cell<usize>,
}

impl UpgradeDialog {
    pub fn new(offer: Offer) -> Self {
        Self {
            offer,
            stage: Stage::Offer,
            focused: 0,
            offset: Cell::new(0),
        }
    }

    /// The install ended; the dialog moves on to restarting or the error.
    pub fn installed(&mut self, result: Result<(), String>) {
        self.stage = match result {
            Ok(()) => Stage::Installed,
            Err(error) => Stage::Failed(error),
        };
        self.focused = 0;
        self.offset.set(0);
    }

    /// How an install that ended after the dialog closed is told.
    pub fn outcome(version: &str, result: &Result<(), String>) -> String {
        match result {
            Ok(()) => tf(
                "Updated to {version}. Restart DimCode to use it.",
                &[("version", version)],
            ),
            Err(error) => tf("Update failed: {error}", &[("error", error)]),
        }
    }

    fn choices(&self) -> &'static [Choice] {
        match self.stage {
            Stage::Offer if self.offer.install.is_some() => {
                &[Choice::Install, Choice::Skip, Choice::Later]
            }
            Stage::Offer => &[Choice::Skip, Choice::Close],
            Stage::Installing | Stage::Failed(_) => &[Choice::Close],
            Stage::Installed => &[Choice::Restart, Choice::Close],
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
                    Choice::Install => {
                        self.stage = Stage::Installing;
                        self.focused = 0;
                        self.offset.set(0);
                        match &self.offer.install {
                            Some(install) => Effect::UpgradeInstall(install.clone()),
                            None => Effect::None,
                        }
                    }
                    Choice::Skip => Effect::UpgradeSkip(self.offer.version.clone()),
                    Choice::Later | Choice::Close => Effect::Close,
                    Choice::Restart => Effect::Restart { confirmed: false },
                };
            }
            _ => {}
        }
        Effect::None
    }

    pub fn render(&self, width: usize, height: usize, p: &Palette) -> Vec<String> {
        let version = self.offer.version.as_str();
        let body = match &self.stage {
            Stage::Offer => vec![
                tf(
                    "Version {latest} is available (you have {current}).",
                    &[("latest", version), ("current", &self.offer.current)],
                ),
                match &self.offer.install {
                    Some(install) => tf(
                        "Will run: {command}",
                        &[("command", &install.command_line())],
                    ),
                    None => tf(
                        "To update, run: {command}",
                        &[("command", &self.offer.manual)],
                    ),
                },
            ],
            Stage::Installing => vec![tf("Updating to {version}...", &[("version", version)])],
            Stage::Installed => vec![Self::outcome(version, &Ok(()))],
            Stage::Failed(error) => vec![Self::outcome(version, &Err(error.clone()))],
        };
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
