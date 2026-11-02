//! The rows of the palette and the list panels:
//! category, label, detail and key in columns shared by every row, under a
//! panel title. Panels and the palette draw through these.

use crate::ui::commands::Command;
use crate::ui::i18n::t;
use crate::ui::render::panel::IDLE_MARKER;
use crate::ui::text::{display_width, truncate_cells};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};

const ROW_INDENT: usize = 2;
const DETAIL_GAP: usize = 2;
const ESC_CLOSE: &str = "esc close";

#[derive(Debug, Clone)]
pub enum Kind {
    Heading,
    /// A row of a plain list panel (sessions).
    Item,
    Command(&'static Command),
    Skill(String),
}

/// One palette row: category, label (bold), detail,
/// and the key that runs it at the right edge.
#[derive(Debug, Clone)]
pub struct Row {
    pub kind: Kind,
    /// Right-aligned in a column of its own; empty in plain lists.
    pub category: String,
    pub label: String,
    pub detail: String,
    pub key: Option<String>,
    /// Shown faint and marked `(unavailable)`; still runs, and the app says
    /// why it cannot.
    pub unavailable: bool,
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, Kind::Heading)
    }
}

/// A panel title without the `esc close` hint (forms say how to go back in
/// their own hint row).
pub fn title_plain(text: &str, inner: usize, p: &Palette) -> String {
    paint(
        &truncate_cells(text, inner),
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    )
}

pub fn title(text: &str, inner: usize, p: &Palette) -> String {
    let close = t(ESC_CLOSE);
    let head = truncate_cells(text, inner.saturating_sub(display_width(close) + 2).max(1));
    let styled = paint(
        &head,
        Paint {
            fg: Some(&p.accent),
            bold: true,
            ..Paint::default()
        },
    );
    let gap = inner.saturating_sub(display_width(&head) + display_width(close));
    if gap < 2 {
        return styled;
    }
    format!("{styled}{}{}", " ".repeat(gap), fg(close, &p.faint))
}

/// Column widths shared by every row, not just the visible window, so
/// nothing shifts while scrolling.
#[derive(Debug, Clone, Copy)]
pub struct Columns {
    /// The right-aligned category column; 0 in plain lists.
    category: usize,
    /// Where details start, counted from the label.
    label: usize,
}

impl Columns {
    pub fn of(rows: &[Row], inner: usize) -> Self {
        let widest = |f: &dyn Fn(&Row) -> Option<usize>| {
            rows.iter()
                .filter(|r| r.selectable())
                .filter_map(f)
                .max()
                .unwrap_or(0)
        };
        let category = widest(&|r| Some(display_width(&r.category)));
        let label = widest(&|r| (!r.detail.is_empty()).then(|| display_width(&r.label)));
        let command = widest(&|r| match r.kind {
            Kind::Command(command) => Some(display_width(command.name) + 1 + DETAIL_GAP),
            _ => None,
        });
        let room = inner.saturating_sub(ROW_INDENT + category + DETAIL_GAP);
        Self {
            category,
            label: (label + DETAIL_GAP).min((room / 2).max(command).min(room).max(1)),
        }
    }

    /// Where a row's detail starts, counted from the panel's first column.
    pub fn detail(&self) -> usize {
        ROW_INDENT + self.label
    }
}

const UNAVAILABLE: &str = " (unavailable)";

/// One row in the shared columns: category (faint, right-aligned), label (bold),
/// detail (faint), key (keybind colour, at the right edge). An unavailable
/// row is faint throughout; the selected row sits on an accent bar.
pub fn panel_row(row: &Row, selected: bool, inner: usize, columns: Columns, p: &Palette) -> String {
    if !row.selectable() {
        return paint(
            &truncate_cells(&row.label, inner),
            Paint {
                fg: Some(&p.muted),
                bold: true,
                ..Paint::default()
            },
        );
    }
    let category = if columns.category == 0 {
        String::new()
    } else {
        let text = truncate_cells(&row.category, columns.category);
        format!(
            "{}{text}{}",
            " ".repeat(columns.category - display_width(&text)),
            " ".repeat(DETAIL_GAP)
        )
    };
    let key = row
        .key
        .as_deref()
        .map(|key| format!("{}{key}", " ".repeat(DETAIL_GAP)))
        .filter(|key| {
            ROW_INDENT + display_width(&category) + display_width(&row.label) + display_width(key)
                <= inner
        })
        .unwrap_or_default();
    let room = inner
        .saturating_sub(ROW_INDENT + display_width(&category) + display_width(&key))
        .max(1);
    let mut label = row.label.clone();
    let mut detail = row.detail.clone();
    if row.unavailable {
        if detail.is_empty() {
            label.push_str(t(UNAVAILABLE));
        } else {
            detail.push_str(t(UNAVAILABLE));
        }
    }
    let label = if detail.is_empty() {
        truncate_cells(&label, room)
    } else {
        truncate_cells(
            &label,
            room.min(columns.label.saturating_sub(DETAIL_GAP).max(1)),
        )
    };
    let detail = if detail.is_empty() {
        String::new()
    } else {
        let gap = columns
            .label
            .saturating_sub(display_width(&label))
            .max(DETAIL_GAP);
        let available = room.saturating_sub(display_width(&label));
        if available <= gap {
            String::new()
        } else {
            truncate_cells(&format!("{}{detail}", " ".repeat(gap)), available)
        }
    };
    let used = ROW_INDENT
        + display_width(&category)
        + display_width(&label)
        + display_width(&detail)
        + display_width(&key);
    let fill = " ".repeat(inner.saturating_sub(used));
    let part = |text: &str, fg: &str, bold: bool| {
        paint(
            text,
            Paint {
                fg: Some(fg),
                bg: selected.then_some(p.accent.as_str()),
                bold,
                ..Paint::default()
            },
        )
    };
    let (text, faint, keyed) = if selected {
        (&p.on_accent, &p.on_accent, &p.keybind)
    } else if row.unavailable {
        (&p.muted, &p.muted, &p.muted)
    } else {
        (&p.text, &p.muted, &p.keybind)
    };
    format!(
        "{}{}{}{}{}",
        part(IDLE_MARKER, text, false),
        part(&category, faint, false),
        part(&label, text, true),
        part(&format!("{detail}{fill}"), faint, false),
        part(&key, keyed, true),
    )
}

pub fn footer(count: &str, hint: &str, inner: usize, p: &Palette) -> String {
    let count = truncate_cells(count, inner);
    let rest = truncate_cells(
        &format!(" · {hint}"),
        inner.saturating_sub(display_width(&count)),
    );
    let mut out = fg(&count, &p.faint);
    if !rest.is_empty() {
        out.push_str(&fg(&rest, &p.muted));
    }
    out
}
