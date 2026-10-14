//! Block-level layout shared by the commit path and the live region: the
//! gutter, cell-accurate wrapping and the row forms of user and assistant
//! text. Markdown styling (2.5) plugs in through [`render_streamed`]'s
//! renderer argument; until then answers lay out as wrapped plain text.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const ASSISTANT_MARK: &str = "\u{25c6}"; // ◆
pub const USER_MARK: &str = "\u{203a}"; // ›

/// Gutter columns for a terminal of `width` (narrow terminals get one).
pub fn gutter_width(width: usize) -> usize {
    if width < 40 { 1 } else { 2 }
}

/// The mark cell(s) of a gutter.
pub fn gutter_mark(mark: &str, gutter: usize) -> String {
    let first: String = mark.chars().take(1).collect();
    if gutter <= 1 {
        first
    } else {
        format!("{first} ")
    }
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Cuts `text` to `max_width` cells with a trailing ellipsis when it does
/// not fit (grapheme-aware, the budget includes the ellipsis).
pub fn truncate_cells(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 1 {
        return "…".to_owned();
    }
    let budget = max_width - 1;
    let mut out = String::new();
    let mut used = 0;
    for cluster in text.graphemes(true) {
        let cell = display_width(cluster);
        if used + cell > budget {
            break;
        }
        out.push_str(cluster);
        used += cell;
    }
    out.push('…');
    out
}

/// Wraps `text` into rows of at most `width` cells, breaking only between
/// grapheme clusters. A cluster wider than the row stays alone on its row.
pub fn wrap_cells(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut rows = Vec::new();
    for line in normalized.split('\n') {
        if line.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut row = String::new();
        let mut row_width = 0;
        for cluster in line.graphemes(true) {
            let cell = display_width(cluster);
            if !row.is_empty() && row_width + cell > width {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            row.push_str(cluster);
            row_width += cell;
        }
        rows.push(row);
    }
    rows
}

/// Replaces the gutter of the first non-blank row with `mark` (`style`
/// paints it). The gutter is reserved as blanks by the renderer, so the mark
/// costs no width; the row's own text starts after the gutter cells.
pub fn apply_gutter_mark(
    lines: Vec<String>,
    mark: &str,
    gutter: usize,
    style: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let mut lines = lines;
    if let Some(index) = lines.iter().position(|line| !line.trim().is_empty()) {
        let rest: String = lines[index].chars().skip(gutter).collect();
        lines[index] = format!("{}{rest}", style(&gutter_mark(mark, gutter)));
    }
    lines
}

/// Plain stand-in for the Markdown renderer: every row carries the reserved
/// gutter blanks.
pub fn render_plain(source: &str, content_width: usize, gutter: usize) -> Vec<String> {
    let pad = " ".repeat(gutter);
    wrap_cells(source.trim_end_matches('\n'), content_width)
        .into_iter()
        .map(|row| format!("{pad}{row}"))
        .collect()
}

/// How the user band is painted: the mark, the text, and the surface tint
/// behind both (none on terminals with no subtle step).
pub struct UserStyle<'a> {
    pub accent: &'a str,
    pub text: &'a str,
    pub surface: Option<&'a str>,
}

/// A user message: mark on the first row, blank gutter after. With a surface
/// tint every row is padded to the content width so it reads as a band.
pub fn render_user(text: &str, width: usize, style: &UserStyle<'_>) -> Vec<String> {
    use crate::ui::theme::ansi::{Paint, paint};
    let gutter = gutter_width(width);
    let content = width.saturating_sub(gutter).max(1);
    let mut rows = wrap_cells(text, content);
    rows.retain(|_| true);
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let lead = if i == 0 {
                gutter_mark(USER_MARK, gutter)
            } else {
                " ".repeat(gutter)
            };
            let body = match style.surface {
                Some(_) => {
                    let used = display_width(&row);
                    format!("{row}{}", " ".repeat(content.saturating_sub(used)))
                }
                None if row.is_empty() => " ".to_owned(),
                None => row,
            };
            format!(
                "{}{}",
                paint(
                    &lead,
                    Paint {
                        fg: Some(style.accent),
                        bg: style.surface,
                        ..Paint::default()
                    }
                ),
                paint(
                    &body,
                    Paint {
                        fg: Some(style.text),
                        bg: style.surface,
                        ..Paint::default()
                    }
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gutter_narrows_below_forty_columns() {
        assert_eq!(gutter_width(39), 1);
        assert_eq!(gutter_width(40), 2);
        assert_eq!(gutter_mark("◆", 1), "◆");
        assert_eq!(gutter_mark("◆", 2), "◆ ");
    }

    #[test]
    fn wrap_breaks_on_cells_not_chars() {
        assert_eq!(wrap_cells("中文中文", 4), vec!["中文", "中文"]);
        assert_eq!(wrap_cells("ab\n\ncd", 5), vec!["ab", "", "cd"]);
        // A wide cluster larger than the row stays whole.
        assert_eq!(wrap_cells("中", 1), vec!["中"]);
        // ZWJ emoji stays one cluster.
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(wrap_cells(&format!("a{family}"), 2), vec!["a", family]);
    }

    #[test]
    fn mark_goes_on_the_first_visible_row() {
        let rows = vec!["  ".to_owned(), "  hello".to_owned()];
        assert_eq!(
            apply_gutter_mark(rows, ASSISTANT_MARK, 2, &|m: &str| m.to_owned()),
            vec!["  ", "◆ hello"]
        );
    }

    #[test]
    fn user_rows_keep_the_gutter() {
        let style = UserStyle {
            accent: "#a9dc76",
            text: "#e4e4e7",
            surface: None,
        };
        let strip = |s: &str| {
            let mut out = String::new();
            let mut skip = false;
            for c in s.chars() {
                match (skip, c) {
                    (false, '\x1b') => skip = true,
                    (true, 'm') => skip = false,
                    (false, c) => out.push(c),
                    _ => {}
                }
            }
            out
        };
        // Wide terminal: two-column gutter.
        let rows: Vec<String> = render_user(&"word ".repeat(10), 40, &style)
            .iter()
            .map(|r| strip(r))
            .collect();
        assert!(rows[0].starts_with("› word"));
        assert!(rows[1].starts_with("  "));
        // Narrow terminal: one-column gutter.
        let narrow: Vec<String> = render_user("one two three", 12, &style)
            .iter()
            .map(|r| strip(r))
            .collect();
        assert_eq!(narrow[0], "›one two thr");
        assert_eq!(narrow[1], " ee");
        // A surface tint pads every row to the content width.
        let banded = UserStyle {
            surface: Some("#2a2a31"),
            ..style
        };
        let rows = render_user("hi", 40, &banded);
        assert_eq!(display_width(&strip(&rows[0])), 40);
    }
}
