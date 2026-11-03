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

/// The panel's title.
enum Title {
    /// English until it is drawn.
    Fixed(&'static str),
    /// The caller's text, drawn as it is.
    Set(String),
}

pub struct ListPanel {
    /// A fixed title is English until it is drawn, so a language preview
    /// shows at once; a set title is drawn as it was given.
    title: Title,
    hint: String,
    /// A question with the key that answers it and the key that cancels it,
    /// drawn in the hint's place until the next [`ListPanel::set_hint`].
    confirmation: Option<(String, String, String)>,
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
    /// The row the last render drew the `> query` line on, `None` when it
    /// gave the line up.
    query_row: Cell<Option<usize>>,
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
            title: Title::Fixed(title),
            hint: hint.into(),
            confirmation: None,
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
            query_row: Cell::new(Some(2)),
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
    /// when the selection moves sets it here. It replaces a confirmation.
    pub fn set_hint(&mut self, hint: String) {
        self.hint = hint;
        self.confirmation = None;
    }

    /// A question with the key that answers it and the key that cancels it,
    /// drawn in the hint's place until the next [`ListPanel::set_hint`].
    pub fn set_confirmation(
        &mut self,
        question: impl Into<String>,
        confirm: impl Into<String>,
        cancel: impl Into<String>,
    ) {
        self.confirmation = Some((question.into(), confirm.into(), cancel.into()));
    }

    /// The title a caller draws in place of the panel's own.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = Title::Set(title.into());
    }

    /// A list whose empty text follows the view it shows replaces it here.
    pub fn set_empty_text(&mut self, empty_text: &'static str) {
        self.empty_text = empty_text;
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
        self.selected_item().map(|item| item.id.clone())
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.shown()
            .into_iter()
            .filter(|i| !i.heading)
            .nth(self.selected)
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

    /// Replaces one row's detail, as a value read after the list was built.
    pub fn set_detail(&mut self, id: &str, detail: impl Into<String>) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.detail = detail.into();
        }
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
    /// columns: after the filter text of the `> query` row, on the row the
    /// last render drew it on. A list without a filter has none.
    pub fn cursor(&self, width: usize) -> Option<(usize, usize)> {
        let inner = width.saturating_sub(4).max(1);
        let row = self.query_row.get()?;
        self.filterable
            .then(|| (row, 4 + self.filter.view(inner.saturating_sub(2)).1))
    }

    /// `width` columns including borders; `rows` is the panel's row budget.
    pub fn render(&self, width: usize, rows: usize, p: &Palette) -> Vec<String> {
        self.render_with_footer(width, rows, p, &[])
    }

    /// As [`ListPanel::render`], with `footer` lines above the bottom border:
    /// the caller colours them, and they take their rows from the list.
    ///
    /// The panel never draws more than `rows` rows, and under two rows it
    /// draws nothing: the two borders alone would take the budget. While the
    /// list cannot draw its first three entries, the notes give up lines
    /// from the end, the blank row going with the last line. When not even
    /// one list row fits beside the other parts, it gives them up in this
    /// order: the rest of the notes, the filter's padding (the row below the
    /// query first), the footer's first line first, the hint's rows above
    /// the border, the query row, and the list's last row. The selected
    /// entry and the hint's closing key stay the longest; a confirmation
    /// keeps its confirm key the longest instead.
    pub fn render_with_footer(
        &self,
        width: usize,
        rows: usize,
        p: &Palette,
        footer: &[String],
    ) -> Vec<String> {
        if rows < 2 {
            self.query_row.set(None);
            self.page.set(0);
            return Vec::new();
        }
        let inner = width.saturating_sub(4).max(1);
        let shown = self.shown();
        let hint = self.hint_text();
        let hint_above = hint_rows(&hint, width);
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
        let selectable: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect();
        let list_total = if selectable.is_empty() {
            shown.iter().filter(|item| item.sticky).count().max(1)
        } else {
            list.len()
        };
        let fitted = self.fit(
            rows,
            list_total,
            notes.len(),
            footer.len(),
            hint_above.as_ref().map_or(0, Vec::len),
        );
        self.query_row
            .set(match (self.filterable, fitted.query, fitted.above) {
                (true, true, true) => Some(2),
                (true, true, false) => Some(1),
                _ => None,
            });
        self.page.set(fitted.list);
        let mut content = Vec::new();
        if self.filterable {
            if fitted.above {
                content.push(String::new());
            }
            if fitted.query {
                content.push(format!("> {}", self.filter.view(inner.saturating_sub(2)).0));
            }
            if fitted.below {
                content.push(String::new());
            }
        }
        if selectable.is_empty() {
            if fitted.list > 0 {
                let sticky: Vec<&Item> = shown.iter().filter(|item| item.sticky).copied().collect();
                if sticky.is_empty() {
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
                    for item in sticky.into_iter().take(fitted.list) {
                        content.push(fg(&truncate_cells(&item.label, inner), &p.muted));
                    }
                }
            }
        } else if fitted.list > 0 {
            let current_at = selectable[self.selected.min(selectable.len() - 1)];
            let start = follow_window(self.window.get(), current_at, fitted.list, list.len());
            self.window.set(start);
            for (offset, row) in list.iter().skip(start).take(fitted.list).enumerate() {
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
        if fitted.notes > 0 {
            content.push(String::new());
            content.extend(notes.into_iter().take(fitted.notes));
        }
        content.extend_from_slice(&footer[footer.len() - fitted.footer..]);
        if fitted.hint_above
            && let Some(hint) = &hint_above
        {
            content.extend(hint.iter().map(|row| key_hints(row, p)));
        }
        let bottom_hint = if fitted.hint_above {
            None
        } else {
            Some(key_hints(&self.short_hint(&hint, width), p))
        };
        let title = match &self.title {
            Title::Fixed(text) => t(text),
            Title::Set(text) => text.as_str(),
        };
        bordered(
            &content,
            width,
            None,
            &p.border,
            Some(&title_plain(title, inner, p)),
            bottom_hint.as_deref(),
        )
    }

    /// The text the hint draws: a confirmation reads as its question, its
    /// confirm key and its cancel key.
    fn hint_text(&self) -> String {
        match &self.confirmation {
            Some((question, confirm, cancel)) => format!("{question} {confirm} · {cancel}"),
            None => self.hint.clone(),
        }
    }

    /// The hint shortened to the bottom border: a key hint keeps its closing
    /// key, a confirmation its confirm key.
    fn short_hint(&self, hint: &str, width: usize) -> String {
        match &self.confirmation {
            Some((question, confirm, cancel)) => {
                shorten_confirmation(question, confirm, cancel, width)
            }
            None => shorten_hint(hint, width),
        }
    }

    /// The parts that fit `rows`. The list takes the space the other parts
    /// leave; while it cannot draw its first three entries, the notes give up
    /// lines from the end, the blank row going with the last line. When not
    /// even one list row fits, the panel gives the rest up in order: the rest
    /// of the notes, the filter's padding (the row below the query first),
    /// the footer's first line first, the hint's rows above the border, the
    /// query row, and the list's last row.
    fn fit(
        &self,
        rows: usize,
        list_total: usize,
        notes: usize,
        footer: usize,
        hint_above: usize,
    ) -> Fitted {
        let filterable = self.filterable;
        let page = |parts: &Fitted| {
            let fixed = 2
                + usize::from(parts.above)
                + usize::from(parts.below)
                + usize::from(parts.query)
                + if parts.notes > 0 { parts.notes + 1 } else { 0 }
                + parts.footer
                + if parts.hint_above { hint_above } else { 0 };
            rows.checked_sub(fixed)
                .map(|left| left.min(list_total))
                .unwrap_or(0)
        };
        let mut parts = Fitted {
            above: filterable,
            below: filterable,
            query: filterable,
            notes,
            footer,
            hint_above: hint_above > 0,
            list: 0,
        };
        while parts.notes > 0 && page(&parts) < list_total.min(3) {
            parts.notes -= 1;
        }
        parts.list = page(&parts);
        if parts.list > 0 {
            return parts;
        }
        parts.notes = 0;
        parts.list = page(&parts);
        if parts.list > 0 {
            return parts;
        }
        parts.below = false;
        parts.list = page(&parts);
        if parts.list > 0 {
            return parts;
        }
        parts.above = false;
        parts.list = page(&parts);
        if parts.list > 0 {
            return parts;
        }
        while parts.footer > 0 {
            parts.footer -= 1;
            parts.list = page(&parts);
            if parts.list > 0 {
                return parts;
            }
        }
        parts.hint_above = false;
        parts.list = page(&parts);
        if parts.list > 0 {
            return parts;
        }
        parts.query = false;
        parts.list = page(&parts);
        parts
    }
}

/// The parts of a panel's layout that survived the row budget.
struct Fitted {
    /// The blank row above the `> query` line.
    above: bool,
    /// The blank row below the `> query` line.
    below: bool,
    query: bool,
    /// Note lines kept from the start; the blank row before them comes with
    /// the first line.
    notes: usize,
    /// Footer lines kept, taken from the end: the line nearest the hint
    /// stays the longest.
    footer: usize,
    /// The hint is drawn as rows above the bottom border.
    hint_above: bool,
    /// List rows to draw.
    list: usize,
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

/// `hint` shortened to fit the bottom border, keeping its closing key last:
/// whole parts go from the one before the closing key leftwards until the
/// rest fits, the first part and the closing key stay while they fit
/// together, and a closing key too wide on its own is truncated with `…`.
fn shorten_hint(hint: &str, width: usize) -> String {
    // `bordered` puts a hint on the bottom border only while a dash still
    // fits on either side of it.
    let room = width.saturating_sub(6).max(1);
    if visible_width(hint) <= room {
        return hint.to_owned();
    }
    let parts: Vec<&str> = hint.split(" · ").collect();
    let closing = parts[parts.len() - 1];
    for kept in (0..parts.len() - 1).rev() {
        let text = parts[..kept]
            .iter()
            .copied()
            .chain(std::iter::once(closing))
            .collect::<Vec<_>>()
            .join(" · ");
        if visible_width(&text) <= room {
            return text;
        }
    }
    truncate_cells(closing, room)
}

/// A confirmation shortened to fit the bottom border, keeping its confirm
/// key the longest: the full text, then the question cut with `…` before the
/// two keys, then the two keys, then the confirm key alone, and a confirm key
/// too wide on its own truncated with `…`.
fn shorten_confirmation(question: &str, confirm: &str, cancel: &str, width: usize) -> String {
    // `bordered` puts a hint on the bottom border only while a dash still
    // fits on either side of it.
    let room = width.saturating_sub(6).max(1);
    let keys = format!("{confirm} · {cancel}");
    let full = format!("{question} {keys}");
    if visible_width(&full) <= room {
        return full;
    }
    let keyed = visible_width(&keys) + 1;
    if room > keyed {
        let question = truncate_cells(question, room - keyed);
        return format!("{question} {keys}");
    }
    if visible_width(&keys) <= room {
        return keys;
    }
    if visible_width(confirm) <= room {
        return confirm.to_owned();
    }
    truncate_cells(confirm, room)
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

    fn palette() -> Palette {
        use crate::ui::theme::{Appearance, ColorMode, Theme};
        Theme::system(ColorMode::TrueColor, Appearance::Dark, None).palette
    }

    /// A panel draws no more rows than it is given, and nothing under two
    /// rows: the two borders alone would take that budget. A confirmation
    /// keeps its confirm key at every budget that draws, and its question
    /// whole while the full text fits.
    #[test]
    fn a_list_panel_draws_within_its_row_budget() {
        let question = "Remove a job whose name is a long one?";
        let confirm = "Enter confirm";
        let cancel = "Esc cancel";
        let confirming = || {
            let mut panel = three_rows();
            panel.set_confirmation(question, confirm, cancel);
            panel
        };
        let mut hinted = three_rows();
        hinted.set_hint("Enter open · Ctrl+R refresh · Ctrl+F filter · Esc close".to_owned());
        let configurations = [
            ("plain", three_rows().without_filter(), Vec::new()),
            ("filterable", three_rows(), Vec::new()),
            (
                "notes",
                three_rows().with_notes(vec![
                    "A note that takes its lines from the list beside it.".to_owned(),
                ]),
                Vec::new(),
            ),
            ("hint above the border", hinted, Vec::new()),
            (
                "footer",
                three_rows(),
                vec![
                    "first footer line".to_owned(),
                    "second footer line".to_owned(),
                ],
            ),
            ("confirmation", confirming(), Vec::new()),
        ];
        for (name, panel, footer) in configurations {
            for width in [20, 40, 80] {
                for budget in 0..=8 {
                    let rows = panel.render_with_footer(width, budget, &palette(), &footer);
                    assert!(
                        rows.len() <= budget,
                        "{name} at {width}x{budget}: {} rows drawn: {rows:#?}",
                        rows.len()
                    );
                    assert_eq!(
                        rows.is_empty(),
                        budget < 2,
                        "{name} at {width}x{budget}: {rows:#?}"
                    );
                }
            }
        }
        let full = format!("{question} {confirm} · {cancel}");
        let panel = confirming();
        for width in 20..=80 {
            for budget in 2..=30 {
                let rows = panel.render(width, budget, &palette());
                let text = crate::ui::render::panel::strip_escapes(&rows.join("\n"));
                assert!(
                    text.contains(confirm),
                    "confirmation at {width}x{budget}: the confirm key: {text}"
                );
                if visible_width(&full) + 6 <= width {
                    assert!(
                        text.contains(question),
                        "confirmation at {width}x{budget}: the whole question: {text}"
                    );
                }
            }
        }
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
