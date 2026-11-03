//! Markdown for the transcript: the vendored `xai-grok-markdown` renderer
//! (pulldown-cmark + syntect, output as ratatui `Line`s) styled with the
//! scrollback palette, wrapped to the content width and converted to ANSI
//! strings the way the rest of the write path expects them.

use anstyle::{Color, RgbColor, Style};
use ratatui::style::{Color as TuiColor, Modifier, Style as TuiStyle};
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use xai_grok_markdown::{
    MarkdownBuffers, MarkdownStyle, Syntect, render_markdown_ratatui_with_buffers_width,
};

use crate::ui::theme::ansi::{Spec, spec, tui_color};
use crate::ui::theme::{CodePalette, Palette, Theme};

/// Columns fenced code is drawn in from its fence.
const CODE_INDENT: usize = 4;

pub struct MarkdownRenderer {
    style: MarkdownStyle,
    syntect: Syntect,
    /// The body text colour; code and table rows draw uncoloured text so it
    /// is lifted off those rows after rendering (the renderer applies one
    /// default colour to every text span).
    text_soft: TuiColor,
    /// Colour of horizontal rules (the `faint` role).
    rule: TuiColor,
}

/// `color` as a foreground style; `dim` is the faint attribute.
fn fg(color: &str) -> Style {
    match spec(color) {
        Some(Spec::Rgb(r, g, b)) => Style::new().fg_color(Some(Color::Rgb(RgbColor(r, g, b)))),
        Some(Spec::Slot(n)) => Style::new().fg_color(Some(Color::Ansi256(n.into()))),
        Some(Spec::Dim) => Style::new().dimmed(),
        Some(Spec::Default) | None => Style::new(),
    }
}

/// The role mapping onto the renderer's styles: a
/// blue (keybinding hue) first-level heading, highlighted second-level ones,
/// bold accent inline code, plain bullets, faint table rules with a divider
/// under every row, accent underlined links.
fn markdown_style(p: &Palette) -> MarkdownStyle {
    let heading_top = fg(&p.keybind).bold();
    let heading_accent = fg(&p.highlight).bold();
    let heading_plain = fg(&p.text).bold();
    let hidden = Style::new().dimmed().hidden();
    let marker = fg(&p.muted);
    MarkdownStyle {
        heading_inner: [
            heading_top,
            heading_accent,
            heading_plain,
            heading_plain,
            heading_plain,
            heading_plain,
        ],
        heading_outer: [hidden, hidden, marker, marker, marker, marker],
        strong_inner: Style::new().bold(),
        strong_outer: hidden,
        emphasis_inner: Style::new().italic(),
        emphasis_outer: hidden,
        strikethrough_inner: Style::new().strikethrough(),
        strikethrough_outer: hidden,
        inline_code_inner: fg(&p.accent).bold(),
        inline_code_outer: hidden,
        blockquote_outer: fg(&p.muted),
        task_checked: fg(&p.success),
        task_unchecked: fg(&p.muted),
        list_item: fg(&p.text),
        rule: fg(&p.faint),
        link_outer: fg(&p.muted),
        link_text: fg(&p.accent).underline(),
        link_url: fg(&p.muted),
        link_title: fg(&p.muted),
        code_outer: fg(&p.muted),
        code_language: fg(&p.faint),
        code_untagged: fg(&p.text_soft),
        code_background: Style::new(),
        table_outer: fg(&p.faint),
        text: fg(&p.text_soft),
        math: fg(&p.text_soft).italic(),
    }
}

/// xterm's default colours for the 16 palette slots.
const XTERM_SLOTS: [&str; 16] = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
    "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
];

/// A syntax colour as the highlighter takes it. Palette slots go in as
/// xterm's colour for the slot, which the terminal-native syntax mode maps
/// back to the slot's hue (greys to the default foreground).
fn syntax_hex(color: &str) -> String {
    match spec(color) {
        Some(Spec::Slot(n)) if n < 16 => XTERM_SLOTS[n as usize].to_owned(),
        Some(Spec::Rgb(..)) => color.to_owned(),
        _ => XTERM_SLOTS[7].to_owned(),
    }
}

/// A minimal TextMate theme carrying the syntax roles of the code palette.
fn tm_theme(code: &CodePalette, text: &str) -> String {
    let text = &syntax_hex(text);
    let rule = |scope: &str, color: &str| {
        let color = syntax_hex(color);
        format!(
            "<dict><key>scope</key><string>{scope}</string><key>settings</key>\
             <dict><key>foreground</key><string>{color}</string></dict></dict>"
        )
    };
    let rules = [
        rule("comment, punctuation.definition.comment", &code.comment),
        rule(
            "keyword, storage, storage.type, keyword.control",
            &code.keyword,
        ),
        rule("keyword.operator", &code.operator),
        rule("string, punctuation.definition.string", &code.string),
        rule("constant.numeric", &code.number),
        rule(
            "constant, support.constant, variable.language",
            &code.constant,
        ),
        rule(
            "entity.name.function, support.function, meta.function-call",
            &code.func,
        ),
        rule(
            "entity.name.type, entity.name.class, support.type, support.class",
            &code.type_,
        ),
        rule("punctuation", &code.punctuation),
    ]
    .concat();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
         <key>name</key><string>dim</string><key>settings</key><array>\
         <dict><key>settings</key><dict><key>foreground</key><string>{text}</string>\
         </dict></dict>{rules}</array></dict></plist>"
    )
}

impl MarkdownRenderer {
    pub fn new(theme: &Theme) -> Self {
        let color = |spec: &str| tui_color(spec).unwrap_or(TuiColor::Reset);
        xai_grok_markdown::set_polarity_safe_syntax(theme.code.keyword.starts_with("ansi:"));
        Self {
            style: markdown_style(&theme.palette),
            syntect: Syntect::new(tm_theme(&theme.code, &theme.palette.text_soft).as_bytes()),
            text_soft: color(&theme.palette.text_soft),
            rule: color(&theme.palette.faint),
        }
    }

    /// Renders `source` into rows of at most `content_width` cells, each
    /// prefixed with the reserved gutter blanks. Trailing blank rows are
    /// dropped (a committed chunk brings its own spacing).
    pub fn render(&self, source: &str, content_width: usize, gutter: usize) -> Vec<String> {
        let width = content_width.max(1);
        let source = crate::ui::text::expand_tabs(source);
        let text = source.trim_end_matches('\n');
        let mut buffers = MarkdownBuffers::new();
        let (output, _) = render_markdown_ratatui_with_buffers_width(
            text,
            self.style,
            true,
            &mut buffers,
            Some(&self.syntect),
            Some(width),
        );
        let in_code = |index: usize| {
            output
                .code_blocks
                .iter()
                .any(|b| b.output_line_range.contains(&index))
        };
        let mut urls: Vec<String> = Vec::new();
        let pad = " ".repeat(gutter);
        let mut rows: Vec<String> = Vec::new();
        let mut fence: Option<usize> = None;
        for (index, line) in output.lines.iter().enumerate() {
            let code = in_code(index);
            let text_of: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let indent = text_of.len() - text_of.trim_start_matches(' ').len();
            if !code
                && ["```", "~~~"]
                    .iter()
                    .any(|f| text_of[indent..].starts_with(f))
            {
                fence = match fence {
                    Some(_) => None,
                    None => Some(indent),
                };
                continue;
            }
            let trimmed = text_of.trim_start();
            let table = trimmed.starts_with(['┌', '├', '└'])
                || (trimmed.starts_with('│') && trimmed.matches('│').count() >= 2);
            let is_rule = !code
                && !table
                && text_of.trim().chars().count() >= 3
                && text_of.trim().chars().all(|c| c == '─');

            if is_rule {
                let rule: Vec<Cell> = (0..width)
                    .map(|_| ("─".to_owned(), TuiStyle::default().fg(self.rule), 1, None))
                    .collect();
                rows.push(format!("{pad}{}", cells_to_ansi(&rule, &urls)));
                continue;
            }

            let mut cells = cells_of(line);
            attach_links(&mut cells, index, &output.hyperlinks, &mut urls);
            if code || table {
                for cell in &mut cells {
                    if cell.1.fg == Some(self.text_soft) || cell.1.fg == Some(TuiColor::Reset) {
                        cell.1.fg = None;
                    }
                    if table {
                        cell.1.add_modifier.remove(Modifier::DIM);
                    }
                }
                merge_neutral_gaps(&mut cells);
            } else {
                for cell in &mut cells {
                    if matches!(cell.1.fg, None | Some(TuiColor::Reset)) && cell.0 != " " {
                        cell.1.fg = Some(self.text_soft);
                    }
                }
            }
            if code {
                let indent = fence.unwrap_or(0) + CODE_INDENT;
                let lead = " ".repeat(indent);
                let skip = cells
                    .iter()
                    .take(indent - CODE_INDENT)
                    .take_while(|cell| cell.0 == " ")
                    .count();
                let body_width = width.saturating_sub(indent).max(1);
                for wrapped in wrap_cells_row(&cells[skip..], body_width, true) {
                    rows.push(format!("{pad}{lead}{}", cells_to_ansi(&wrapped, &urls)));
                }
                continue;
            }
            if table {
                for cell in &mut cells {
                    if let Some(round) = match cell.0.as_str() {
                        "┌" => Some("╭"),
                        "┐" => Some("╮"),
                        "└" => Some("╰"),
                        "┘" => Some("╯"),
                        _ => None,
                    } {
                        cell.0 = round.to_owned();
                    }
                }
            } else if let Some(marker) = cells.iter().position(|cell| cell.0 != " ")
                && cells[marker].0 == "•"
                && cells.get(marker + 1).is_some_and(|cell| cell.0 == " ")
            {
                cells[marker].0 = "-".to_owned();
            }
            for wrapped in wrap_cells_row(&cells, width, false) {
                rows.push(format!("{pad}{}", cells_to_ansi(&wrapped, &urls)));
            }
        }
        while rows.last().is_some_and(|row| row.trim().is_empty()) {
            rows.pop();
        }
        rows
    }
}

/// One cell of a wrapped line: a grapheme cluster, its style, its width and
/// the index of the hyperlink target it belongs to.
pub type Cell = (String, TuiStyle, usize, Option<usize>);

fn cells_of(line: &Line<'_>) -> Vec<Cell> {
    let mut cells = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for cluster in span.content.graphemes(true) {
            let width = UnicodeWidthStr::width(cluster);
            cells.push((cluster.to_owned(), style, width, None));
        }
    }
    cells
}

/// Only these schemes become OSC 8 links; anything else keeps the visible
/// ` (destination)` so it is not silently dropped.
pub(crate) fn linkable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

fn attach_links(
    cells: &mut Vec<Cell>,
    line_index: usize,
    targets: &[xai_grok_markdown::HyperlinkTarget],
    urls: &mut Vec<String>,
) {
    let mut starts: Vec<usize> = Vec::new();
    let mut column = 0;
    for cell in cells.iter() {
        starts.push(column);
        column += cell.2;
    }
    let mut linked: Vec<(usize, usize)> = Vec::new();
    for target in targets.iter().filter(|t| t.line_index == line_index) {
        if !linkable(&target.url) {
            continue;
        }
        let slot = match urls.iter().position(|u| *u == target.url) {
            Some(i) => i,
            None => {
                urls.push(target.url.clone());
                urls.len() - 1
            }
        };
        let mut last_end: Option<usize> = None;
        for (i, cell) in cells.iter_mut().enumerate() {
            if cell.2 > 0 && target.column_range.contains(&starts[i]) {
                cell.3 = Some(slot);
                last_end = Some(i + 1);
            }
        }
        if let Some(end) = last_end {
            linked.push((slot, end));
        }
    }
    linked.sort_by_key(|(_, end)| std::cmp::Reverse(*end));
    for (slot, end) in linked {
        let suffix = format!(" ({})", urls[slot]);
        let tail: String = cells[end.min(cells.len())..]
            .iter()
            .map(|c| c.0.as_str())
            .collect();
        if tail.starts_with(&suffix) {
            let drop = suffix.graphemes(true).count();
            cells.drain(end..end + drop);
        }
    }
}

/// A run of plain spaces between two cells of the same colour takes that
/// colour, so `export const` reads as one styled run.
fn merge_neutral_gaps(cells: &mut [Cell]) {
    for i in 1..cells.len().saturating_sub(1) {
        let blank = cells[i].0 == " " && cells[i].1.fg.is_none();
        if blank && cells[i - 1].1.fg.is_some() && cells[i - 1].1.fg == cells[i + 1].1.fg {
            cells[i].1.fg = cells[i - 1].1.fg;
        }
    }
}

/// Hanging indent of a wrapped row: under a numbered list marker or a quote
/// bar. A `-` item continues at its margin.
fn hanging_indent(cells: &[Cell]) -> usize {
    let text: String = cells.iter().map(|c| c.0.as_str()).collect();
    let leading = text.len() - text.trim_start_matches(' ').len();
    let mut indent = leading;
    let mut remaining = &text[leading..];
    while let Some(after) = remaining.strip_prefix("│ ") {
        indent += 2;
        remaining = after;
    }
    let digits = remaining.bytes().take_while(u8::is_ascii_digit).count();
    let number =
        (digits > 0 && digits <= 9 && remaining[digits..].starts_with(". ")).then_some(digits + 2);
    indent + number.unwrap_or(0)
}

/// Word wrap of one logical row (port of `wrapAnsiWords` + `layoutBlock`):
/// runs of spaces are kept except at a break, a word longer than the row is
/// cut by display width, and every row after the first starts with the hang.
fn wrap_cells_row(cells: &[Cell], width: usize, code: bool) -> Vec<Vec<Cell>> {
    let total: usize = cells.iter().map(|c| c.2).sum();
    if total <= width {
        return vec![cells.to_vec()];
    }
    let hang = if code { 0 } else { hanging_indent(cells) };
    let hang = hang.min(width.saturating_sub(1));
    let first_width = width.max(1);
    let cont_width = width.saturating_sub(hang).max(1);

    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut row_width = 0usize;
    let mut word: Vec<Cell> = Vec::new();
    let mut word_width = 0usize;
    let mut gap: Vec<Cell> = Vec::new();

    macro_rules! budget {
        () => {
            if rows.is_empty() {
                first_width
            } else {
                cont_width
            }
        };
    }
    macro_rules! flush_row {
        () => {{
            rows.push(std::mem::take(&mut row));
            row_width = 0;
            gap.clear();
        }};
    }
    macro_rules! flush_word {
        () => {{
            if !word.is_empty() {
                if row_width > 0 && row_width + gap.len() + word_width > budget!() {
                    flush_row!();
                }
                if !gap.is_empty() && (row_width > 0 || rows.is_empty()) {
                    row_width += gap.len();
                    row.append(&mut gap);
                }
                gap.clear();
                while word_width > budget!() {
                    let room = budget!().saturating_sub(row_width);
                    let mut used = 0;
                    let mut take = 0;
                    while take < word.len() && used + word[take].2 <= room {
                        used += word[take].2;
                        take += 1;
                    }
                    if take == 0 {
                        break;
                    }
                    row.extend(word.drain(..take));
                    word_width -= used;
                    flush_row!();
                }
                row_width += word_width;
                row.append(&mut word);
                word_width = 0;
            }
        }};
    }

    for cell in cells.iter().map(Some).chain([None]) {
        match cell {
            Some(cell) if cell.0 != " " => {
                word_width += cell.2;
                word.push(cell.clone());
            }
            _ => {
                flush_word!();
                gap.extend(cell.cloned());
            }
        }
    }
    rows.push(row);

    for (index, row) in rows.iter_mut().enumerate() {
        if index == 0 || hang == 0 {
            continue;
        }
        let style = row.first().map(|c| c.1).unwrap_or_default();
        let mut prefixed: Vec<Cell> = (0..hang)
            .map(|_| (" ".to_owned(), style, 1, None))
            .collect();
        prefixed.append(row);
        *row = prefixed;
    }
    rows
}

fn color_code(color: TuiColor, background: bool) -> Option<String> {
    let base = if background { 40 } else { 30 };
    Some(match color {
        TuiColor::Reset => return None,
        TuiColor::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", if background { 48 } else { 38 }),
        TuiColor::Indexed(n @ 0..=7) => format!("{}", base + n as u32),
        TuiColor::Indexed(n @ 8..=15) => format!("{}", base + 60 + n as u32 - 8),
        TuiColor::Indexed(n) => format!("{};5;{n}", if background { 48 } else { 38 }),
        TuiColor::Black => format!("{base}"),
        TuiColor::Red => format!("{}", base + 1),
        TuiColor::Green => format!("{}", base + 2),
        TuiColor::Yellow => format!("{}", base + 3),
        TuiColor::Blue => format!("{}", base + 4),
        TuiColor::Magenta => format!("{}", base + 5),
        TuiColor::Cyan => format!("{}", base + 6),
        TuiColor::Gray => format!("{}", base + 7),
        TuiColor::DarkGray => format!("{}", base + 60),
        TuiColor::LightRed => format!("{}", base + 61),
        TuiColor::LightGreen => format!("{}", base + 62),
        TuiColor::LightYellow => format!("{}", base + 63),
        TuiColor::LightBlue => format!("{}", base + 64),
        TuiColor::LightMagenta => format!("{}", base + 65),
        TuiColor::LightCyan => format!("{}", base + 66),
        TuiColor::White => format!("{}", base + 67),
    })
}

fn sgr(style: &TuiStyle) -> Option<String> {
    let mut codes: Vec<String> = Vec::new();
    let m = style.add_modifier;
    for (flag, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if m.contains(flag) {
            codes.push(code.into());
        }
    }
    if let Some(code) = style.fg.and_then(|c| color_code(c, false)) {
        codes.push(code);
    }
    if let Some(code) = style.bg.and_then(|c| color_code(c, true)) {
        codes.push(code);
    }
    (!codes.is_empty()).then(|| codes.join(";"))
}

/// One ANSI string for a row: every styled run is `SGR text SGR 0`, with the
/// codes in the order the rest of the transcript uses (bold, dim, italic,
/// underline, strikethrough, then colours), and link text wrapped in OSC 8.
pub fn cells_to_ansi(cells: &[Cell], urls: &[String]) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < cells.len() {
        let link = cells[index].3;
        let mut end = index;
        while end < cells.len() && cells[end].3 == link {
            end += 1;
        }
        if let Some(url) = link.and_then(|i| urls.get(i)) {
            out.push_str(&format!(
                "\x1b]8;;{}\x1b\\",
                url.replace(char::is_control, "")
            ));
        }
        let mut run_start = index;
        while run_start < end {
            let style = cells[run_start].1;
            let mut run_end = run_start;
            while run_end < end && cells[run_end].1 == style {
                run_end += 1;
            }
            let text: String = cells[run_start..run_end]
                .iter()
                .map(|c| c.0.as_str())
                .collect();
            match sgr(&style) {
                Some(codes) => out.push_str(&format!("\x1b[{codes}m{text}\x1b[0m")),
                None => out.push_str(&text),
            }
            run_start = run_end;
        }
        if link.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
        index = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode};

    fn renderer() -> MarkdownRenderer {
        MarkdownRenderer::new(&Theme::system(ColorMode::TrueColor, Appearance::Dark, None))
    }

    /// The visible text of rows: CSI sequences and OSC strings (BEL- or
    /// ST-terminated, as OSC 8 links use) are dropped.
    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                let mut out = String::new();
                let mut chars = r.chars().peekable();
                while let Some(c) = chars.next() {
                    if c != '\x1b' {
                        out.push(c);
                        continue;
                    }
                    match chars.peek() {
                        Some(']') => {
                            chars.next();
                            while let Some(n) = chars.next() {
                                if n == '\x07' {
                                    break;
                                }
                                if n == '\x1b' {
                                    chars.next();
                                    break;
                                }
                            }
                        }
                        _ => {
                            for n in chars.by_ref() {
                                if n.is_ascii_alphabetic() {
                                    break;
                                }
                            }
                        }
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn long_paragraphs_wrap_on_words_within_the_width() {
        let rows =
            plain(&renderer().render("one two three four five six seven eight nine ten", 20, 0));
        assert!(rows.len() > 1);
        assert!(
            rows.iter()
                .all(|r| UnicodeWidthStr::width(r.as_str()) <= 20),
            "{rows:?}"
        );
        assert_eq!(
            rows.join(" "),
            "one two three four five six seven eight nine ten"
        );
    }

    #[test]
    fn quoted_text_keeps_the_body_colour_and_is_not_mistaken_for_a_table() {
        let rows = renderer().render("> quoted line\n> more", 40, 0);
        assert!(rows[0].contains("quoted line"));
        assert!(rows[0].contains(" quoted line"), "{:?}", rows[0]);
        assert!(!rows[0].contains("\x1b[2mquoted"), "{:?}", rows[0]);
    }

    #[test]
    fn list_items_keep_amps_dash_and_wrap_to_the_margin() {
        let rows = plain(&renderer().render(
            "- alpha beta gamma delta epsilon zeta eta theta iota",
            20,
            0,
        ));
        assert!(rows[0].starts_with("- alpha"));
        assert!(!rows[1].starts_with(' '), "{rows:?}");
    }

    #[test]
    fn tab_indented_code_keeps_its_indentation() {
        let rows = plain(&renderer().render(
            "```go\nfunc fib(n int) int {\n\ta, b := 0, 1\n\tfor i := 0; i < n; i++ {\n\t\ta, b = b, a+b\n\t}\n}\n```",
            60,
            0,
        ));
        assert_eq!(rows[1], "        a, b := 0, 1", "{rows:?}");
        assert_eq!(rows[3], "            a, b = b, a+b", "{rows:?}");
    }

    #[test]
    fn code_rows_wrap_hard_under_their_indent() {
        let rows = plain(&renderer().render("```\nabcdefghijklmnopqrstuvwxyz\n```", 10, 0));
        assert_eq!(rows[0], "    abcdef");
        assert!(rows[1].starts_with("    ghi"), "{rows:?}");
    }

    #[test]
    fn styled_runs_are_closed_and_do_not_leak() {
        let rows = renderer().render("a **bold** word", 40, 0);
        assert!(rows[0].contains("\x1b[1m"), "{:?}", rows[0]);
        assert!(rows[0].matches("\x1b[0m").count() >= 1);
        let tail = rows[0].rsplit("\x1b[0m").next().unwrap();
        assert!(!tail.contains('\x1b'), "{:?}", rows[0]);
    }

    #[test]
    fn fenced_code_is_indented_without_its_fences() {
        let rows = plain(&renderer().render("```rust\nlet x = 1;\n```", 40, 0));
        assert_eq!(rows, vec!["    let x = 1;"]);
    }

    #[test]
    fn inline_code_drops_its_backticks() {
        let rows = plain(&renderer().render("use `code` here", 40, 0));
        assert_eq!(rows, vec!["use code here"]);
    }

    /// Link targets are positioned in the rendered line before the visible
    /// ` (url)` suffixes are removed, so two links with leading text keep
    /// their own OSC 8 spans.
    #[test]
    fn links_show_their_text_and_carry_the_url_as_an_osc8_link() {
        let raw = renderer().render("[site](https://example.com)", 40, 0);
        assert_eq!(plain(&raw), vec!["site"]);
        assert!(
            raw[0].starts_with("\x1b]8;;https://example.com\x1b\\"),
            "{:?}",
            raw[0]
        );
        let raw = renderer().render(
            "see [a](https://a.example) and [b](https://b.example)",
            80,
            0,
        );
        assert_eq!(plain(&raw), vec!["see a and b"]);
        let linked_text = |url: &str| {
            let open = format!("\x1b]8;;{url}\x1b\\");
            let rest = &raw[0][raw[0].find(&open).expect(url) + open.len()..];
            let close = rest.find("\x1b]8;;\x1b\\").expect("link close");
            crate::ui::render::panel::strip_escapes(&rest[..close])
        };
        assert_eq!(linked_text("https://a.example"), "a");
        assert_eq!(linked_text("https://b.example"), "b");
    }

    #[test]
    fn task_items_keep_the_marker_and_the_box() {
        let rows = plain(&renderer().render("- [x] done\n- [ ] todo", 40, 0));
        assert_eq!(rows, vec!["- [x] done", "- [ ] todo"]);
    }

    #[test]
    fn strikethrough_drops_its_markers() {
        let rows = plain(&renderer().render("~~gone~~", 40, 0));
        assert_eq!(rows, vec!["gone"]);
    }

    #[test]
    fn any_markdown_at_any_width_lays_out_without_panicking() {
        let r = renderer();
        let pieces = [
            "# Heading",
            "plain words that wrap",
            "- item\n  - nested",
            "1. one\n2. two",
            "```rust\nfn main() {}\n\tlet x = 1;\n```",
            "```mermaid\ngraph TD; A-->B\n```",
            "| a | b |\n|---|---|\n| 中文 | 👨\u{200d}👩\u{200d}👧 |",
            "> quote",
            "[link](https://e.x)",
            "`code`",
            "**bold** _it_ ~~s~~",
            "---",
            "\t\ttabs",
            "",
            "  ",
            "a\u{301}e\u{301}",
            "```\nunclosed fence",
            "https://example.com/very/long/path/that/does/not/break",
        ];
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let count = 1 + (state % 4) as usize;
            let source: Vec<&str> = (0..count)
                .map(|k| pieces[((state >> (k * 8)) as usize) % pieces.len()])
                .collect();
            let source = source.join("\n\n");
            for width in [0, 1, 2, 3, 5, 8, 13, 40] {
                for gutter in [0, 2] {
                    let _ = r.render(&source, width, gutter);
                }
            }
        }
    }
}
