//! Cell-accurate text measurement shared by every drawing layer: display
//! width, truncation, tab expansion and wrapping by display columns. It
//! depends on nothing else in the interface.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Gutter columns for a terminal of `width` (narrow terminals get one).
pub fn gutter_width(width: usize) -> usize {
    if width < 40 { 1 } else { 2 }
}

/// The bar in front of a user message.
pub const USER_MARK: &str = "\u{2503}";
/// The mark in front of an assistant thought summary.
pub const ASSISTANT_MARK: &str = "\u{25c6}";
/// The bar in front of a thought shown whole.
pub const THINKING_BAR: &str = "\u{258e}";
/// The mark in front of an error row.
pub const ERROR_MARK: &str = "\u{25a0}";
/// The mark in front of a status or skipped row.
pub const STATUS_MARK: &str = "\u{25e6}";
/// The mark in front of a shell or command row.
pub const SHELL_MARK: &str = "$";
/// The mark in front of a running tool or compaction row.
pub const RUNNING_MARK: &str = "\u{00b7}";
/// The mark in front of a completed row.
pub const DONE_MARK: &str = "\u{2713}";
/// The mark in front of a failed row.
pub const FAILED_MARK: &str = "\u{2717}";
/// The mark in front of a load-earlier row.
pub const LOAD_MORE_MARK: &str = "\u{2191}";
/// The mark in front of a branch line.
pub const LINE_MARK: &str = "\u{2442}";

/// The gutter glyphs that count as chrome after a row's margin column. The
/// others (`$`, `·`, `✓`, `✗`, `↑`, `⑂`) can start assistant prose, which has
/// no gutter. Fullscreen copy retains these ambiguous marks on margin-prefixed
/// rows, including shell and status rows, until rows report their own chrome.
pub const MARGIN_MARKS: &[&str] = &[
    USER_MARK,
    ASSISTANT_MARK,
    THINKING_BAR,
    ERROR_MARK,
    STATUS_MARK,
];

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

/// Keeps the last `max_width` cells of `text` behind a leading ellipsis
/// when it does not fit (grapheme-aware, the budget includes the ellipsis).
pub fn truncate_cells_start(text: &str, max_width: usize) -> String {
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
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for cluster in text.graphemes(true).rev() {
        let cell = display_width(cluster);
        if used + cell > budget {
            break;
        }
        kept.push(cluster);
        used += cell;
    }
    let mut out = String::from("…");
    out.extend(kept.into_iter().rev());
    out
}

/// Columns between tab stops, CommonMark's.
const TAB_STOP: usize = 4;

/// `text` with each tab turned into the spaces up to the next tab stop of
/// its line. A tab never reaches the terminal (it would move the cursor
/// over cells the row does not paint, and rows are scrubbed of it), so
/// indentation made of tabs (Go, Makefiles) is kept this way.
pub fn expand_tabs(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut column = 0;
    for cluster in text.graphemes(true) {
        match cluster {
            "\t" => {
                let spaces = TAB_STOP - column % TAB_STOP;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            "\n" | "\r\n" | "\r" => {
                out.push_str(cluster);
                column = 0;
            }
            _ => {
                out.push_str(cluster);
                column += display_width(cluster);
            }
        }
    }
    out.into()
}

/// Wraps `text` into rows of at most `width` cells, breaking only between
/// grapheme clusters. A cluster wider than the row stays alone on its row.
pub fn wrap_cells(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let normalized = expand_tabs(text).replace("\r\n", "\n").replace('\r', "\n");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_cells_start_keeps_the_end() {
        assert_eq!(truncate_cells_start("abcdef", 10), "abcdef");
        assert_eq!(truncate_cells_start("abcdef", 4), "…def");
        assert_eq!(truncate_cells_start("中文混合text", 5), "…text");
        assert_eq!(truncate_cells_start("abc", 1), "…");
        assert_eq!(truncate_cells_start("abc", 0), "");
    }

    #[test]
    fn tabs_expand_to_the_next_stop_of_their_line() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("ab\tc\n\td"), "ab  c\n    d");
        assert_eq!(expand_tabs("中\tx"), "中  x");
        assert!(matches!(
            expand_tabs("plain"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn wrap_breaks_on_cells_not_chars() {
        assert_eq!(wrap_cells("中文中文", 4), vec!["中文", "中文"]);
        assert_eq!(wrap_cells("ab\n\ncd", 5), vec!["ab", "", "cd"]);
        assert_eq!(wrap_cells("中", 1), vec!["中"]);
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(wrap_cells(&format!("a{family}"), 2), vec!["a", family]);
    }

    #[test]
    fn measuring_at_any_width_never_panics() {
        let samples = [
            "",
            "a",
            "中文中文",
            "👨\u{200d}👩\u{200d}👧x",
            "\ttab\tbed",
            "e\u{301}",
            "a\nb\n\n",
            "\u{200b}",
        ];
        for text in samples {
            for width in 0..6 {
                let _ = wrap_cells(text, width);
                let _ = truncate_cells(text, width);
            }
            let _ = expand_tabs(text);
            let _ = display_width(text);
        }
    }
}
