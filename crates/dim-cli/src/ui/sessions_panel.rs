//! The `/resume` picker: a filterable list of stored sessions (port of
//! `sessionsPanelItems` / `SessionsPanel`).

use crate::model::session::SessionItem;
use crate::ui::list_panel::{Event, Item, ListPanel};
use crate::ui::prompts::Effect;
use crate::ui::terminal::KeyEvent;
use crate::ui::theme::Palette;

const HINT: &str = "enter open · ^R refresh";

pub struct SessionsPanel {
    list: ListPanel,
}

impl SessionsPanel {
    pub fn new() -> Self {
        Self {
            list: ListPanel::new(
                "Sessions",
                HINT,
                "Loading sessions...",
                "No saved sessions yet.",
                "No matching sessions.",
                true,
            ),
        }
    }

    pub fn set_items(&mut self, items: Result<Vec<SessionItem>, String>) {
        self.list.set_items(
            items.map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let mut row = Item::new(
                            item.id.clone(),
                            label(item),
                            relative_time(item.updated_at, now_ms()),
                        );
                        row.search = search_text(item);
                        row
                    })
                    .collect()
            }),
            None,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        match self.list.handle_key(key) {
            Event::None => Effect::None,
            Event::Close => Effect::Close,
            Event::Pick(id) => Effect::Resume(id),
            Event::Refresh => Effect::Refresh,
        }
    }

    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.list.render(width, rows, p)
    }
}

fn search_text(item: &SessionItem) -> String {
    [
        item.title.as_deref(),
        Some(item.id.as_str()),
        Some(item.cwd.as_str()),
        Some(item.model.as_str()),
        item.preview.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

/// The name unless it is the placeholder, else the first words, else the id.
pub fn label(item: &SessionItem) -> String {
    let name = item.title.as_deref().map(str::trim).unwrap_or("");
    if !name.is_empty() && !name.eq_ignore_ascii_case("new session") {
        return name.to_owned();
    }
    let preview = item.preview.as_deref().map(str::trim).unwrap_or("");
    if !preview.is_empty() {
        return preview.replace('\n', " ");
    }
    if name.is_empty() {
        item.id.clone()
    } else {
        name.to_owned()
    }
}

pub fn relative_time(updated_at: u64, now: u64) -> String {
    let elapsed = now.saturating_sub(updated_at);
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if elapsed < MINUTE {
        "now".to_owned()
    } else if elapsed < HOUR {
        format!("{}m ago", elapsed / MINUTE)
    } else if elapsed < DAY {
        format!("{}h ago", elapsed / HOUR)
    } else {
        format!("{}d ago", elapsed / DAY)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::terminal::{KeyCode, Modifiers};

    fn item(id: &str, title: Option<&str>, preview: Option<&str>) -> SessionItem {
        SessionItem {
            id: id.into(),
            title: title.map(str::to_owned),
            preview: preview.map(str::to_owned),
            cwd: "/w".into(),
            updated_at: 0,
            model: "p/m".into(),
        }
    }

    #[test]
    fn label_prefers_a_real_name_then_the_first_message() {
        assert_eq!(label(&item("a", Some("Fix bug"), Some("hi"))), "Fix bug");
        assert_eq!(
            label(&item("a", Some("New session"), Some("hello there"))),
            "hello there"
        );
        assert_eq!(label(&item("abc", None, None)), "abc");
    }

    #[test]
    fn moving_up_inside_the_window_moves_the_highlight_not_the_list() {
        use crate::ui::theme::{Appearance, ColorMode, palette::palette_for};
        let p = palette_for(ColorMode::TrueColor, Appearance::Dark);
        let mut panel = SessionsPanel::new();
        let items = (0..30)
            .map(|i| item(&format!("s{i}"), Some(&format!("session {i:02}")), None))
            .collect();
        panel.set_items(Ok(items));
        let key = |code| KeyEvent::new(code, Modifiers::NONE);
        let shown = |panel: &SessionsPanel| -> Vec<String> {
            panel
                .render(60, 9, &p)
                .iter()
                .map(|r| crate::ui::render::panel::strip_escapes(r))
                .filter(|r| r.contains("session "))
                .collect()
        };
        // Page of 5 rows: going down past it scrolls the list...
        shown(&panel);
        for _ in 0..7 {
            panel.handle_key(key(KeyCode::Down));
        }
        let rows = shown(&panel);
        assert!(
            rows[0].contains("session 03") && rows[4].contains("› session 07"),
            "{rows:?}"
        );
        // ...going back up stays in that window until its top is passed.
        panel.handle_key(key(KeyCode::Up));
        panel.handle_key(key(KeyCode::Up));
        let rows = shown(&panel);
        assert!(
            rows[0].contains("session 03") && rows[2].contains("› session 05"),
            "{rows:?}"
        );
        for _ in 0..3 {
            panel.handle_key(key(KeyCode::Up));
        }
        let rows = shown(&panel);
        assert!(rows[0].contains("› session 02"), "{rows:?}");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(1_000, 30_000), "now");
        assert_eq!(relative_time(0, 5 * 60_000), "5m ago");
        assert_eq!(relative_time(0, 3 * 3_600_000), "3h ago");
        assert_eq!(relative_time(0, 2 * 86_400_000), "2d ago");
    }

    #[test]
    fn filter_narrows_and_enter_resumes_the_selected_row() {
        let mut panel = SessionsPanel::new();
        panel.set_items(Ok(vec![
            item("one", None, Some("alpha topic")),
            item("two", None, Some("beta topic")),
        ]));
        for c in "beta".chars() {
            panel.handle_key(KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
        }
        match panel.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE)) {
            Effect::Resume(id) => assert_eq!(id, "two"),
            _ => panic!("expected resume"),
        }
    }
}
