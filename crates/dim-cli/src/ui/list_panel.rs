//! A filterable, scrollable list panel (port of `ListPanel` + the list
//! model of the scrollback panels): the sessions picker, the model picker.

use std::cell::Cell;

use crate::ui::i18n::t;
use crate::ui::line_input::LineInput;
use crate::ui::render::panel::{bordered, follow_window, key_hints, visible_width, wrap_words};
use crate::ui::rows::{Columns, Kind, Row, panel_row, title_plain};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::text::truncate_cells;
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub detail: String,
    /// Extra text the filter matches besides the label.
    pub search: String,
    /// A group header: drawn, never selected, hidden while filtering.
    pub heading: bool,
    /// Stays in the list whatever the filter says.
    pub sticky: bool,
}

impl Item {
    pub fn new(id: impl Into<String>, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: detail.into(),
            search: String::new(),
            heading: false,
            sticky: false,
        }
    }

    pub fn heading(label: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            label: label.into(),
            detail: String::new(),
            search: String::new(),
            heading: true,
            sticky: false,
        }
    }
}

/// What a key did.
#[derive(Debug, PartialEq)]
pub enum Event {
    None,
    Close,
    Pick(String),
    Refresh,
}

pub struct ListPanel {
    /// The fixed texts are English, translated as they are drawn so a
    /// language preview shows at once.
    title: &'static str,
    hint: String,
    loading_text: &'static str,
    empty_text: &'static str,
    no_match_text: &'static str,
    /// The detail follows the label on one string (`name  detail`).
    inline_detail: bool,
    /// Headings stay while filtering.
    all_rows: bool,
    /// Typed characters filter the list.
    filterable: bool,
    remote_filter: bool,
    items: Vec<Item>,
    filter: LineInput,
    /// Index among the *selectable* rows currently shown.
    selected: usize,
    loading: bool,
    error: Option<String>,
    /// Rows the list may show; set from the screen height while rendering.
    page: Cell<usize>,
    /// First row of the visible window, kept between frames.
    window: Cell<usize>,
    refreshable: bool,
    notes: Vec<String>,
}

impl ListPanel {
    pub fn new(
        title: &'static str,
        hint: impl Into<String>,
        loading_text: &'static str,
        empty_text: &'static str,
        no_match_text: &'static str,
        refreshable: bool,
    ) -> Self {
        Self {
            title,
            hint: hint.into(),
            loading_text,
            empty_text,
            no_match_text,
            inline_detail: false,
            all_rows: false,
            filterable: true,
            remote_filter: false,
            items: Vec::new(),
            filter: LineInput::new(""),
            selected: 0,
            loading: true,
            error: None,
            page: Cell::new(6),
            window: Cell::new(0),
            refreshable,
            notes: Vec::new(),
        }
    }

    /// The provider list's layout: details inline, headings kept while
    /// filtering.
    pub fn with_inline_rows(mut self) -> Self {
        self.inline_detail = true;
        self.all_rows = true;
        self
    }

    /// A list that is only moved through (the theme picker).
    pub fn without_filter(mut self) -> Self {
        self.filterable = false;
        self
    }

    /// The hint names the keys the selection offers; a panel that redraws it
    /// when the selection moves sets it here.
    pub fn set_hint(&mut self, hint: String) {
        self.hint = hint;
    }

    /// Explanatory paragraphs below the choices, separate from keyboard hints.
    pub fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }

    pub fn filter_is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    pub fn query(&self) -> &str {
        self.filter.text()
    }

    pub fn remote_filter(&mut self, enabled: bool) {
        self.remote_filter = enabled;
    }

    pub fn near_end(&self) -> bool {
        !self.items.is_empty() && self.selected + 4 >= self.items.len()
    }

    pub fn loading(&mut self) {
        self.loading = true;
        self.items.clear();
        self.selected = 0;
        self.window.set(0);
        self.error = None;
    }

    pub fn append_items(&mut self, items: Vec<Item>) {
        self.items.extend(items);
        self.error = None;
    }

    /// A paste goes into the filter, on one line.
    pub fn paste(&mut self, text: &str) {
        if self.filterable {
            self.filter.paste(text);
            self.selected = 0;
        }
    }

    /// The id of the selected row.
    pub fn selected_id(&self) -> Option<String> {
        self.shown()
            .into_iter()
            .filter(|i| !i.heading)
            .nth(self.selected)
            .map(|i| i.id.clone())
    }

    pub fn set_items(&mut self, items: Result<Vec<Item>, String>, preselect: Option<&str>) {
        self.loading = false;
        match items {
            Ok(items) => {
                self.items = items;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        if let Some(id) = preselect
            && let Some(at) = self
                .shown()
                .iter()
                .filter(|i| !i.heading)
                .position(|i| i.id == id)
        {
            self.selected = at;
        }
        let count = self.shown().iter().filter(|i| !i.heading).count();
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    /// Rows for the current filter: headings only while nothing is typed.
    fn shown(&self) -> Vec<&Item> {
        let query = self.filter.text().trim().to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                if self.remote_filter || query.is_empty() {
                    return true;
                }
                if item.sticky {
                    return true;
                }
                if item.heading {
                    return self.all_rows;
                }
                item.label.to_lowercase().contains(&query)
                    || item.search.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Event {
        let ctrl = key.mods.contains(Modifiers::CTRL);
        let count = self.shown().iter().filter(|i| !i.heading).count();
        let page = self.page.get();
        match key.code {
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    Event::Close
                } else {
                    self.filter.clear();
                    self.selected = 0;
                    Event::None
                }
            }
            KeyCode::Char('r') if ctrl && self.refreshable => {
                self.loading = true;
                Event::Refresh
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.selected = self.selected.saturating_sub(1);
                Event::None
            }
            KeyCode::Down | KeyCode::Tab => {
                if self.selected + 1 < count {
                    self.selected += 1;
                }
                Event::None
            }
            KeyCode::Home => {
                self.selected = 0;
                Event::None
            }
            KeyCode::End => {
                self.selected = count.saturating_sub(1);
                Event::None
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(page);
                Event::None
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + page).min(count.saturating_sub(1));
                Event::None
            }
            KeyCode::Enter => self
                .shown()
                .into_iter()
                .filter(|i| !i.heading)
                .nth(self.selected)
                .map_or(Event::None, |item| Event::Pick(item.id.clone())),
            _ if self.filterable && self.filter.handle_key(key) => {
                self.selected = 0;
                Event::None
            }
            _ => Event::None,
        }
    }

    /// Where the terminal cursor goes in the rendered rows of `width`
    /// columns: after the filter text of the `> query` row, under the top
    /// border and a blank row. A list without a filter has none.
    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        let inner = width.saturating_sub(4).max(1);
        self.filterable
            .then(|| (2, 4 + self.filter.view(inner.saturating_sub(2)).1))
    }

    /// `width` columns including borders; `rows` is the panel's row budget.
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.render_with_footer(width, rows, p, &[])
    }

    /// As [`ListPanel::render`], with `footer` lines above the bottom border:
    /// the caller fits and colours them, and they take their rows from the
    /// list.
    pub fn render_with_footer(
        &self,
        width: usize,
        rows: usize,
        p: &Palette,
        footer: &[String],
    ) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let shown = self.shown();
        let query_rows = if self.filterable { 3 } else { 0 };
        // A hint the bottom border cannot hold is drawn as rows above a plain
        // bottom border, so no key is ever lost to the width. The rows take
        // the list's space like the footer's, but the list keeps one row of
        // its own: a panel too short for both shows no hint rather than no
        // list.
        let hint = hint_rows(&self.hint, width)
            .filter(|hint| hint.len() <= rows.saturating_sub(2 + query_rows + footer.len() + 1));
        let (bottom_hint, hint_rows): (Option<String>, Vec<String>) = match &hint {
            Some(hint) => (None, hint.iter().map(|row| key_hints(row, p)).collect()),
            None => (Some(key_hints(&self.hint, p)), Vec::new()),
        };
        let mut notes = Vec::new();
        for note in &self.notes {
            for line in wrap_words(note, inner.saturating_sub(6).max(1)) {
                let prefix = if notes.is_empty() { "Hint: " } else { "      " };
                notes.push(fg(
                    &truncate_cells(&format!("{prefix}{line}"), inner),
                    &p.muted,
                ));
            }
        }
        notes.truncate(rows.saturating_sub(3 + query_rows + shown.len().min(3) + hint_rows.len()));
        let note_rows = notes.len() + usize::from(!notes.is_empty());
        let page = rows
            .saturating_sub(2 + query_rows + note_rows + footer.len() + hint_rows.len())
            .max(1);
        self.page.set(page);
        let list: Vec<Row> = shown
            .iter()
            .map(|item| Row {
                kind: if item.heading {
                    Kind::Heading
                } else {
                    Kind::Item
                },
                label: if self.inline_detail && !item.detail.is_empty() {
                    format!("{}  {}", item.label, item.detail)
                } else {
                    item.label.clone()
                },
                detail: if self.inline_detail {
                    String::new()
                } else {
                    item.detail.clone()
                },
                category: String::new(),
                key: None,
                unavailable: false,
            })
            .collect();
        let columns = Columns::of(&list, inner);
        let mut content = Vec::new();
        if self.filterable {
            content.push(String::new());
            content.push(format!("> {}", self.filter.view(inner.saturating_sub(2)).0));
            content.push(String::new());
        }
        let selectable: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect();
        if selectable.is_empty() && shown.iter().any(|item| item.sticky) {
            for item in shown.iter().filter(|item| item.sticky).take(page) {
                content.push(fg(&truncate_cells(&item.label, inner), &p.muted));
            }
        } else if selectable.is_empty() {
            let text = if self.loading {
                t(self.loading_text)
            } else if let Some(error) = &self.error {
                error.as_str()
            } else if self.filter.is_empty() {
                t(self.empty_text)
            } else {
                t(self.no_match_text)
            };
            content.push(fg(&truncate_cells(text, inner), &p.muted));
        } else {
            let current_at = selectable[self.selected.min(selectable.len() - 1)];
            let start = follow_window(self.window.get(), current_at, page, list.len());
            self.window.set(start);
            for (offset, row) in list.iter().skip(start).take(page).enumerate() {
                if matches!(row.kind, Kind::Heading) {
                    content.push(fg(
                        &truncate_cells(&format!("  {}", row.label), inner),
                        &p.muted,
                    ));
                } else {
                    content.push(panel_row(
                        row,
                        start + offset == current_at,
                        inner,
                        columns,
                        p,
                    ));
                }
            }
        }
        if !notes.is_empty() {
            content.push(String::new());
            content.extend(notes);
        }
        content.extend_from_slice(footer);
        content.extend(hint_rows);
        bordered(
            &content,
            width,
            None,
            &p.border,
            Some(&title_plain(t(self.title), inner, p)),
            bottom_hint.as_deref(),
        )
    }
}

/// The hint as rows above the bottom border, when the border cannot hold it:
/// its ` · ` parts joined into rows of the panel's content width. A part
/// wider than a row wraps across rows, so nothing is cut.
fn hint_rows(hint: &str, width: usize) -> Option<Vec<String>> {
    // `bordered` puts a hint on the bottom border only while a dash still
    // fits on either side of it.
    if visible_width(hint) + 6 <= width {
        return None;
    }
    let room = width.saturating_sub(4).max(1);
    let mut rows: Vec<String> = Vec::new();
    for part in hint.split(" · ") {
        let mut lines = wrapped_part(part, room).into_iter();
        let first = lines.next().unwrap_or_default();
        match rows.last_mut() {
            Some(row) if visible_width(row) + 3 + visible_width(&first) <= room => {
                row.push_str(" · ");
                row.push_str(&first);
            }
            _ => rows.push(first),
        }
        rows.extend(lines);
    }
    Some(rows)
}

/// `part` as rows of `room` columns, so no key or action is cut to fit. Its
/// last two words are the key the part asks for and that key's action, and
/// they stay on one row.
fn wrapped_part(part: &str, room: usize) -> Vec<String> {
    let words: Vec<&str> = part.split(' ').collect();
    if visible_width(part) <= room || words.len() < 3 {
        return wrap_words(part, room);
    }
    let split = words.len() - 2;
    let mut rows = wrap_words(&words[..split].join(" "), room);
    let last = words[split..].join(" ");
    match rows.last_mut() {
        Some(row) if visible_width(row) + 1 + visible_width(&last) <= room => {
            row.push(' ');
            row.push_str(&last);
        }
        _ => rows.push(last),
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_rows() -> ListPanel {
        let mut panel = ListPanel::new("T", "h", "loading", "empty", "none", false);
        panel.set_items(
            Ok(vec![
                Item::new("a", "alpha", ""),
                Item::new("b", "beta", ""),
                Item::new("c", "custom", ""),
            ]),
            None,
        );
        panel
    }

    /// A part wider than a row wraps instead of being cut: the remove
    /// confirmation keeps its keys and actions at 80 columns, with a long
    /// name.
    #[test]
    fn a_hint_part_wider_than_a_row_wraps_whole() {
        let name = "a job whose name is exactly sixty cells long to wrap a hint!";
        let hint = format!("Remove {name}? Enter confirm · Esc cancel");
        let rows = hint_rows(&hint, 80).unwrap();
        assert!(rows.iter().all(|row| visible_width(row) <= 76), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("Enter confirm")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("Esc cancel")),
            "{rows:?}"
        );
        assert!(!rows.iter().any(|row| row.contains('…')), "{rows:?}");
    }

    #[test]
    fn the_filter_edits_like_a_line_and_takes_pastes() {
        let mut panel = three_rows();
        let key = |code| KeyEvent::new(code, Modifiers::NONE);
        panel.handle_key(key(KeyCode::Down));
        for c in "cus".chars() {
            assert_eq!(panel.handle_key(key(KeyCode::Char(c))), Event::None);
        }
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.shown().len(), 1);
        assert_eq!(panel.cursor(40), Some((2, 7)));
        for _ in 0..3 {
            panel.handle_key(key(KeyCode::Left));
        }
        assert_eq!(panel.cursor(40), Some((2, 4)));
        panel.handle_key(key(KeyCode::Delete));
        assert_eq!(panel.filter.text(), "us");
        panel.paste("t\nom");
        assert_eq!(panel.filter.text(), "t omus");
        assert!(
            panel
                .render(
                    40,
                    10,
                    &crate::ui::theme::palette::terminal_palette(
                        crate::ui::theme::ColorMode::TrueColor,
                        crate::ui::theme::Appearance::Dark,
                    )
                )
                .join("\n")
                .contains("> t omus")
        );
        assert_eq!(panel.handle_key(key(KeyCode::Esc)), Event::None);
        assert!(panel.filter_is_empty());
        assert_eq!(panel.shown().len(), 3);
        assert_eq!(panel.handle_key(key(KeyCode::Esc)), Event::Close);
        let mut plain = three_rows().without_filter();
        assert_eq!(plain.handle_key(key(KeyCode::Char('x'))), Event::None);
        assert!(plain.filter_is_empty());
        assert_eq!(plain.cursor(40), None);
    }
}
