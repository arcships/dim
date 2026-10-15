//! A filterable, scrollable list panel (port of `ListPanel` + the list
//! model of the scrollback panels): the sessions picker, the model picker.

use std::cell::Cell;

use crate::ui::composer::palette::{Columns, Kind, Row, footer, panel_row, title};
use crate::ui::i18n::{t, tf};
use crate::ui::render::panel::{bordered, follow_window};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::fg;
use crate::ui::transcript::blocks::truncate_cells;

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub detail: String,
    /// Extra text the filter matches besides the label.
    pub search: String,
    /// A group header: drawn, never selected, hidden while filtering.
    pub heading: bool,
    /// Counted in a `Total: N` footer (an action row is not).
    pub counted: bool,
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
            counted: true,
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
            counted: false,
            sticky: false,
        }
    }
}

/// What a key did.
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
    hint: &'static str,
    loading_text: &'static str,
    empty_text: &'static str,
    no_match_text: &'static str,
    /// `Total: N` instead of `i / n` in the footer (the model picker).
    total_count: bool,
    /// The detail follows the label on one string (`name  detail`).
    inline_detail: bool,
    /// Headings stay while filtering, and `i / n` counts every row.
    all_rows: bool,
    /// Typed characters filter the list.
    filterable: bool,
    items: Vec<Item>,
    filter: String,
    /// Index among the *selectable* rows currently shown.
    selected: usize,
    loading: bool,
    error: Option<String>,
    /// Rows the list may show; set from the screen height while rendering.
    page: Cell<usize>,
    /// First row of the visible window, kept between frames.
    window: Cell<usize>,
    refreshable: bool,
}

impl ListPanel {
    pub fn new(
        title: &'static str,
        hint: &'static str,
        loading_text: &'static str,
        empty_text: &'static str,
        no_match_text: &'static str,
        refreshable: bool,
    ) -> Self {
        Self {
            title,
            hint,
            loading_text,
            empty_text,
            no_match_text,
            total_count: false,
            inline_detail: false,
            all_rows: false,
            filterable: true,
            items: Vec::new(),
            filter: String::new(),
            selected: 0,
            loading: true,
            error: None,
            page: Cell::new(6),
            window: Cell::new(0),
            refreshable,
        }
    }

    pub fn with_total_count(mut self) -> Self {
        self.total_count = true;
        self
    }

    /// The provider list's layout: details inline, every row counted,
    /// headings kept while filtering.
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

    /// The id of the selected row.
    pub fn selected_id(&self) -> Option<String> {
        self.shown()
            .into_iter()
            .filter(|i| !i.heading)
            .nth(self.selected)
            .map(|i| i.id.clone())
    }

    pub fn set_loading(&mut self) {
        self.loading = true;
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
        let query = self.filter.trim().to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                if query.is_empty() {
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
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Event::None
            }
            KeyCode::Down => {
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
            KeyCode::Backspace if self.filterable => {
                self.filter.pop();
                self.selected = 0;
                Event::None
            }
            KeyCode::Char(c) if self.filterable && !ctrl && !key.mods.contains(Modifiers::ALT) => {
                self.filter.push(c);
                self.selected = 0;
                Event::None
            }
            _ => Event::None,
        }
    }

    /// `width` columns including borders; `rows` is the panel's row budget.
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        let inner = width.saturating_sub(4).max(1);
        let shown = self.shown();
        let filter_row = usize::from(!self.filter.is_empty());
        let page = rows.saturating_sub(4 + filter_row).max(1);
        self.page.set(page);
        let list: Vec<Row> = shown
            .iter()
            .map(|item| Row {
                kind: if item.heading {
                    Kind::Heading
                } else {
                    Kind::Item(item.id.clone())
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
        let mut content = vec![title(t(self.title), inner, p)];
        if filter_row == 1 {
            content.push(fg(
                &truncate_cells(&tf("filter: {value}", &[("value", &self.filter)]), inner),
                &p.muted,
            ));
        }
        let selectable: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect();
        if selectable.is_empty() {
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
                    // A section sits in the marker column's step, not left of it.
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
        let position = if self.all_rows && !selectable.is_empty() {
            format!(
                "{} / {}",
                selectable[self.selected.min(selectable.len() - 1)] + 1,
                list.len()
            )
        } else if self.total_count {
            let counted = shown.iter().filter(|i| !i.heading && i.counted).count();
            tf("Total: {count}", &[("count", &counted.to_string())])
        } else if selectable.is_empty() {
            "0 / 0".to_owned()
        } else {
            format!(
                "{} / {}",
                self.selected.min(selectable.len() - 1) + 1,
                selectable.len()
            )
        };
        content.push(footer(&position, t(self.hint), inner, p));
        bordered(&content, width, None, &p.border, None)
    }
}
