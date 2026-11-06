//! `/timeline`: the user messages of the shown line, a jump to any of them,
//! and the actions a picked message offers (timeline UI §4.6).

use crate::model::view::{RowKind, ViewRow, ViewSnapshot};
use crate::sanitize::safe;
use crate::ui::i18n::t;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::render::panel::{visible_width, wrap_words};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::truncate_cells;
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

const HINT: &str = "Enter actions · Esc close";

/// What a key on a timeline row asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineAction {
    Jump,
    Edit,
    Quote,
    Branch,
    Fork,
}

/// The user messages of `rows`, oldest first: `(row index, its reply's
/// first text)`. Line-opened markers are not messages.
pub fn messages(rows: &[std::sync::Arc<ViewRow>]) -> Vec<(usize, Option<&str>)> {
    let mut out: Vec<(usize, Option<&str>)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.line.is_some() {
            continue;
        }
        match row.kind {
            RowKind::UserMessage => out.push((index, None)),
            RowKind::AssistantMessage if !row.text.trim().is_empty() => {
                if let Some((_, reply @ None)) = out.last_mut() {
                    *reply = Some(row.text.as_str());
                }
            }
            _ => {}
        }
    }
    out
}

/// One line of `text`, whitespace collapsed.
fn preview(text: &str) -> String {
    safe(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub struct TimelinePanel {
    session_id: String,
    projection_id: u64,
    focus: String,
    list: ListPanel,
    actions: Option<ListPanel>,
}

impl TimelinePanel {
    pub fn new(snapshot: &ViewSnapshot) -> Self {
        let mut panel = Self {
            session_id: snapshot.session_id.clone(),
            projection_id: snapshot.projection_id,
            focus: snapshot.focus.clone(),
            // Letters act on the selected message, so typing does not filter.
            list: ListPanel::new(
                "Timeline",
                t(HINT),
                "",
                "No messages yet.",
                "No messages yet.",
                false,
            )
            .without_filter(),
            actions: None,
        };
        panel.set_rows(snapshot);
        panel
    }

    /// Whether the panel still describes the shown line.
    pub fn current(&self, snapshot: &ViewSnapshot) -> bool {
        self.session_id == snapshot.session_id
            && self.projection_id == snapshot.projection_id
            && self.focus == snapshot.focus
    }

    pub fn set_rows(&mut self, snapshot: &ViewSnapshot) {
        let selected = self.list.selected_id();
        let messages = messages(&snapshot.rows);
        let newest = messages
            .last()
            .map(|(index, _)| snapshot.rows[*index].row_id.clone());
        let items = messages
            .into_iter()
            .enumerate()
            .map(|(number, (index, reply))| {
                let row = &snapshot.rows[index];
                Item::new(
                    row.row_id.clone(),
                    format!(
                        "#{}  {}",
                        number + 1,
                        preview(row.display.as_deref().unwrap_or(&row.text))
                    ),
                    reply.map(preview).unwrap_or_default(),
                )
            })
            .collect();
        let preselect = selected.clone().or(newest);
        self.list.set_items(Ok(items), preselect.as_deref());
        if self.list.selected_id() != selected {
            self.actions = None;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let action = match key.code {
            KeyCode::Char('1') if self.actions.is_some() && key.mods == Modifiers::NONE => {
                Some(TimelineAction::Jump)
            }
            KeyCode::Char('2') if self.actions.is_some() && key.mods == Modifiers::NONE => {
                Some(TimelineAction::Edit)
            }
            KeyCode::Char('3') if self.actions.is_some() && key.mods == Modifiers::NONE => {
                Some(TimelineAction::Quote)
            }
            KeyCode::Char('4') if self.actions.is_some() && key.mods == Modifiers::NONE => {
                Some(TimelineAction::Branch)
            }
            KeyCode::Char('5') if self.actions.is_some() && key.mods == Modifiers::NONE => {
                Some(TimelineAction::Fork)
            }
            KeyCode::Char('e') if key.mods == Modifiers::NONE => Some(TimelineAction::Edit),
            KeyCode::Char('q') if key.mods == Modifiers::NONE => Some(TimelineAction::Quote),
            KeyCode::Char('b') if key.mods == Modifiers::NONE => Some(TimelineAction::Branch),
            KeyCode::Char('f') if key.mods == Modifiers::NONE => Some(TimelineAction::Fork),
            _ => None,
        };
        if let Some(action) = action {
            return match self.list.selected_id() {
                Some(row_id) => Effect::Timeline { row_id, action },
                None => Effect::None,
            };
        }
        if let Some(actions) = &mut self.actions {
            return match actions.handle_key(key) {
                Event::Close => {
                    self.actions = None;
                    Effect::None
                }
                Event::Pick(action) => Effect::Timeline {
                    row_id: self.list.selected_id().unwrap(),
                    action: match action.as_str() {
                        "jump" => TimelineAction::Jump,
                        "edit" => TimelineAction::Edit,
                        "quote" => TimelineAction::Quote,
                        "branch" => TimelineAction::Branch,
                        "fork" => TimelineAction::Fork,
                        _ => unreachable!(),
                    },
                },
                Event::None | Event::Refresh => Effect::None,
            };
        }
        match self.list.handle_key(key) {
            Event::None | Event::Refresh => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick(_) => {
                let mut actions = ListPanel::new(
                    "Timeline actions",
                    format!("↑↓/Enter · {}", t("Esc back")),
                    "",
                    "",
                    "",
                    false,
                )
                .without_filter();
                actions.set_items(
                    Ok(vec![
                        Item::new("jump", format!("1. {}", t("Jump to message")), ""),
                        Item::new("edit", format!("2. {}", t("Edit as a new draft")), ""),
                        Item::new("quote", format!("3. {}", t("Quote message")), ""),
                        Item::new("branch", format!("4. {}", t("Create branch from here")), ""),
                        Item::new("fork", format!("5. {}", t("Fork session")), ""),
                    ]),
                    Some("jump"),
                );
                self.actions = Some(actions);
                Effect::None
            }
        }
    }

    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        self.actions.as_ref().unwrap_or(&self.list).cursor(width)
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        if let Some(actions) = &self.actions {
            let inner = width.saturating_sub(4).max(1);
            let mut footer = Vec::new();
            if let Some(action) = actions.selected_item() {
                if visible_width(&action.label) > inner.saturating_sub(2) {
                    footer.extend(
                        wrap_words(&action.label, inner)
                            .iter()
                            .map(|line| fg(line, &p.text)),
                    );
                }
                if action.id == "fork" {
                    footer.extend(
                        wrap_words(t("Before this message"), inner)
                            .iter()
                            .map(|line| fg(line, &p.muted)),
                    );
                }
            }
            if let Some(message) = self.list.selected_item() {
                footer.push(fg(&truncate_cells(&message.label, inner), &p.text));
            }
            return actions.render_with_footer(width, rows, p, &footer);
        }
        self.list.render(width, rows, p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn row(id: &str, kind: RowKind, text: &str) -> Arc<ViewRow> {
        Arc::new(ViewRow {
            row_id: id.into(),
            kind,
            text: text.into(),
            display: None,
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
            cut: Some(1),
            line: None,
            compaction: None,
        })
    }

    fn snapshot() -> ViewSnapshot {
        ViewSnapshot {
            session_id: "s1".into(),
            projection_id: 2,
            focus: "main".into(),
            rows: vec![
                row("u1", RowKind::UserMessage, "weather in\nShanghai"),
                row("t1", RowKind::Thinking, "let me look"),
                row("a1", RowKind::AssistantMessage, "Cloudy, 20°C"),
                row("a2", RowKind::AssistantMessage, "and dry"),
                row("u2", RowKind::UserMessage, "adsa"),
            ],
            ..ViewSnapshot::default()
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    #[test]
    fn lists_each_user_message_with_its_first_reply() {
        let snapshot = snapshot();
        let found = messages(&snapshot.rows);
        assert_eq!(found, [(0, Some("Cloudy, 20°C")), (4, None)]);
        let p = {
            use crate::ui::theme::{Appearance, ColorMode, Theme};
            Theme::system(ColorMode::TrueColor, Appearance::Dark, None).palette
        };
        let text = TimelinePanel::new(&snapshot).render(80, 12, &p).join("\n");
        assert!(text.contains("#1  weather in Shanghai"), "{text}");
        assert!(text.contains("Cloudy, 20°C"), "{text}");
        assert!(text.contains("#2  adsa"), "{text}");
    }

    #[test]
    fn opens_on_the_newest_message_and_letters_act_on_the_selection() {
        let mut panel = TimelinePanel::new(&snapshot());
        assert_eq!(panel.handle_key(key(KeyCode::Enter)), Effect::None);
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter)),
            Effect::Timeline {
                row_id: "u2".into(),
                action: TimelineAction::Jump,
            }
        );
        panel.handle_key(key(KeyCode::Esc));
        panel.handle_key(key(KeyCode::Up));
        for (letter, action) in [
            ('e', TimelineAction::Edit),
            ('q', TimelineAction::Quote),
            ('b', TimelineAction::Branch),
            ('f', TimelineAction::Fork),
        ] {
            assert_eq!(
                panel.handle_key(key(KeyCode::Char(letter))),
                Effect::Timeline {
                    row_id: "u1".into(),
                    action,
                }
            );
        }
        assert_eq!(panel.handle_key(key(KeyCode::Esc)), Effect::Close);
    }
}
