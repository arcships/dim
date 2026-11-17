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
    wrap_with_source(text, width)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

/// One row of a wrapped text and, for each character it draws, the byte of the
/// source it came from. Wrapping expands a tab, normalises a CRLF and drops the
/// break after a row, so the row's characters no longer line up with the
/// source: a caller that styles what it draws keeps its source spans here.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Wrapped {
    pub text: String,
    /// One source byte per character of `text`, in order.
    pub source: Vec<usize>,
}

impl Wrapped {
    /// One flag per character of `text`: whether the byte it came from is
    /// inside one of `spans`. A tab's spaces all report the tab, so they are
    /// covered together with it.
    pub fn covered(&self, spans: &[(usize, usize)]) -> Vec<bool> {
        self.source
            .iter()
            .map(|at| spans.iter().any(|(start, end)| start <= at && at < end))
            .collect()
    }
}
/// `wrap_cells` with each row's source bytes kept: a tab becomes the spaces to
/// the next tab stop of its line, a CRLF or CR is one break, and every drawn
/// character remembers where it came from. Each of a tab's spaces is a cluster
/// of its own, so it wraps like any other cell.
pub fn wrap_with_source(text: &str, width: usize) -> Vec<Wrapped> {
    if width == 0 {
        return vec![Wrapped::default()];
    }
    let mut expanded = String::with_capacity(text.len() + 8);
    let mut source = Vec::with_capacity(text.len() + 8);
    let mut column = 0;
    for (at, cluster) in text.grapheme_indices(true) {
        match cluster {
            "\t" => {
                let spaces = TAB_STOP - column % TAB_STOP;
                expanded.extend(std::iter::repeat_n(' ', spaces));
                source.extend(std::iter::repeat_n(at, spaces));
                column += spaces;
            }
            "\n" | "\r\n" | "\r" => {
                expanded.push('\n');
                source.push(at);
                column = 0;
            }
            _ => {
                for (offset, ch) in cluster.char_indices() {
                    expanded.push(ch);
                    source.push(at + offset);
                }
                column += display_width(cluster);
            }
        }
    }
    let mut rows = Vec::new();
    let mut row = Wrapped::default();
    let mut used = 0;
    let mut at = 0;
    for cluster in expanded.graphemes(true) {
        if cluster == "\n" {
            rows.push(std::mem::take(&mut row));
            used = 0;
            at += 1;
            continue;
        }
        let cell = display_width(cluster);
        if !row.text.is_empty() && used + cell > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        for ch in cluster.chars() {
            row.text.push(ch);
            row.source.push(source[at]);
            at += 1;
        }
        used += cell;
    }
    rows.push(row);
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

    /// No width from zero up makes the four measuring functions panic.
    #[test]
    fn wrap_breaks_on_cells_not_chars() {
        assert_eq!(wrap_cells("中文中文", 4), vec!["中文", "中文"]);
        assert_eq!(wrap_cells("ab\n\ncd", 5), vec!["ab", "", "cd"]);
        assert_eq!(wrap_cells("中", 1), vec!["中"]);
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(wrap_cells(&format!("a{family}"), 2), vec!["a", family]);
        for text in [
            "",
            "a",
            "中文中文",
            "👨\u{200d}👩\u{200d}👧x",
            "\ttab\tbed",
            "e\u{301}",
            "a\nb\n\n",
            "\u{200b}",
        ] {
            for width in 0..6 {
                let _ = wrap_cells(text, width);
                let _ = truncate_cells(text, width);
            }
            let _ = expand_tabs(text);
            let _ = display_width(text);
        }
    }

    /// The rows this wrap drew before it carried a tab's byte: an expanded
    /// tab's spaces were ordinary cells and broke at the row edge.
    fn wrap_cells_expanding_tabs_first(text: &str, width: usize) -> Vec<String> {
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

    /// A tab's spaces break at the row edge like any other cells.
    #[test]
    fn wrap_cells_splits_a_tabs_spaces_at_the_row_edge() {
        assert_eq!(wrap_cells("\tX", 2), vec!["  ", "  ", "X"]);
        assert_eq!(wrap_cells("abcde\tX", 6), vec!["abcde ", "  X"]);
        let corpus = [
            "",
            "a",
            "\tX",
            "\t",
            "ab\tX",
            "abc\tX",
            "abcd\tX",
            "abcde\tX",
            "中\tx",
            "\t\tX",
            "a\t\tb",
            "a\r\nb",
            "a\rb",
            "\ta\r\n\tb",
            "a\r\tb",
            "a\n\nb\n",
            "\n",
            "中文中文",
            "中文中",
            "a   b",
            "   ",
            "e\u{301}",
            "e\u{301}\tX",
            "\t/skill:commit now",
        ];
        for text in corpus {
            for width in 0..=12 {
                assert_eq!(
                    wrap_cells(text, width),
                    wrap_cells_expanding_tabs_first(text, width),
                    "{text:?} at width {width}"
                );
            }
        }
    }

    /// Every space a tab expands to reports that tab's byte, on the row it was
    /// drawn on, past a row break too.
    #[test]
    fn a_tabs_spaces_report_its_byte_on_the_next_row() {
        let drawn: Vec<(String, Vec<usize>)> = wrap_with_source("abcde\tX", 6)
            .into_iter()
            .map(|row| (row.text, row.source))
            .collect();
        assert_eq!(
            drawn,
            vec![
                ("abcde ".to_owned(), vec![0, 1, 2, 3, 4, 5]),
                ("  X".to_owned(), vec![5, 5, 6]),
            ],
            "one of the tab's three spaces stays on the first row"
        );
    }
}
