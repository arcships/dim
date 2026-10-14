//! The composer: the editable draft of the scrollback interface (design §6.7).
//!
//! Editing, wrapping, undo and kill/yank come from the vendored
//! `xai-ratatui-textarea`; this layer adds the dim contract on top: Enter
//! submits and Shift+Enter/Ctrl+J insert a newline, long pastes become atomic
//! tokens, Up/Down move by drawn row and fall through to the input history,
//! and a `/` draft opens the command palette.

pub mod commands;
pub mod history;
pub mod palette;

use crossterm::event::{KeyCode as CtCode, KeyEvent as CtKey, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidgetRef;
use xai_ratatui_textarea::{ElementId, ElementKind, TextArea, TextAreaState};

use crate::ui::render::markdown::{Cell, cells_to_ansi};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers};
use crate::ui::theme::Palette;
use palette::PaletteState;

/// Most rows the input shows before it scrolls.
pub const MAX_INPUT_ROWS: usize = 7;
const MULTILINE_PASTE_MIN_LINES: usize = 4;
const MULTILINE_PASTE_MIN_CHARS: usize = 201;
const LONG_TEXT_PASTE_MIN_CHARS: usize = 5000;
const TOKEN_KIND: ElementKind = ElementKind(1);

#[derive(Debug, Clone)]
struct Token {
    id: ElementId,
    label: String,
    content: String,
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Consumed (or ignored) with no effect on the app.
    Handled,
    /// The draft was submitted; the string is the expanded content.
    Submit(String),
    /// A palette row (or a typed `/name`) was chosen.
    Command { name: String, args: String },
    /// Not a composer key.
    Pass,
}

pub struct Composer {
    area: TextArea,
    state: TextAreaState,
    tokens: Vec<Token>,
    history: Vec<String>,
    history_index: Option<usize>,
    saved_draft: String,
    palette: PaletteState,
    session_active: bool,
    skills: Vec<palette::Skill>,
}

impl Composer {
    pub fn new(history: Vec<String>) -> Self {
        Self {
            area: TextArea::new(),
            state: TextAreaState::default(),
            tokens: Vec::new(),
            history,
            history_index: None,
            saved_draft: String::new(),
            palette: PaletteState::default(),
            session_active: false,
            skills: Vec::new(),
        }
    }

    pub fn set_session_active(&mut self, active: bool) {
        self.session_active = active;
    }

    /// Replaces the draft (for example with a message taken back from the
    /// queue); the cursor goes to the end.
    pub fn set_draft(&mut self, text: &str) {
        self.clear();
        self.area.set_text(text);
        self.area.set_cursor(text.len());
    }

    pub fn set_skills(&mut self, skills: Vec<palette::Skill>) {
        self.skills = skills;
    }

    pub fn text(&self) -> &str {
        self.area.text()
    }

    pub fn is_empty(&self) -> bool {
        self.area.text().is_empty()
    }

    pub fn clear(&mut self) {
        self.area.set_text("");
        self.tokens.clear();
        self.history_index = None;
        self.saved_draft.clear();
        self.palette = PaletteState::default();
        self.state = TextAreaState::default();
    }

    /// Replaces the history (after a session or directory change).
    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_index = None;
    }

    pub fn push_history(&mut self, entry: &str) {
        self.history.retain(|existing| existing != entry);
        self.history.push(entry.to_owned());
        let skip = self.history.len().saturating_sub(10);
        self.history.drain(..skip);
    }

    // ---- command palette -------------------------------------------------

    /// The query after the leading `/` when the draft is a command being
    /// typed: one line, starting with `/`, no whitespace yet.
    fn command_query(&self) -> Option<&str> {
        let text = self.area.text();
        let rest = text.strip_prefix('/')?;
        (!rest.contains(char::is_whitespace)).then_some(rest)
    }

    pub fn palette_open(&self) -> bool {
        self.command_query().is_some()
    }

    fn palette_rows(&self) -> Vec<palette::Row> {
        palette::rows_for(
            self.command_query().unwrap_or(""),
            self.session_active,
            &self.skills,
        )
    }

    // ---- keys ------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let plain = key.mods.0 & !Modifiers::SHIFT.0 == 0;
        let ctrl_only = key.mods.contains(Modifiers::CTRL) && key.mods.0 == Modifiers::CTRL.0;

        if self.palette_open()
            && let Some(outcome) = self.palette_key(key)
        {
            return outcome;
        }
        match key.code {
            KeyCode::Enter
                if key.mods.contains(Modifiers::SHIFT) || key.mods.contains(Modifiers::ALT) =>
            {
                self.insert_newline();
                return Outcome::Handled;
            }
            KeyCode::Char('j') if ctrl_only => {
                self.insert_newline();
                return Outcome::Handled;
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Up | KeyCode::Down
                if plain
                    && !key.mods.contains(Modifiers::SHIFT)
                    && self.browse_history(key.code == KeyCode::Up) =>
            {
                return Outcome::Handled;
            }
            _ => {}
        }
        match to_crossterm(key) {
            Some(event) => {
                self.history_index = None.filter(|_: &usize| false).or(self.history_index);
                self.area.input(event);
                self.prune_tokens();
                Outcome::Handled
            }
            None => Outcome::Pass,
        }
    }

    fn insert_newline(&mut self) {
        self.area.insert_str("\n");
    }

    fn palette_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        let query = self.command_query()?.to_owned();
        self.palette.set_query(&query);
        let rows = self.palette_rows();
        match key.code {
            KeyCode::Up if key.mods.0 == 0 => {
                self.palette.step(-1, &rows);
                Some(Outcome::Handled)
            }
            KeyCode::Down if key.mods.0 == 0 => {
                self.palette.step(1, &rows);
                Some(Outcome::Handled)
            }
            KeyCode::Esc => {
                // First Esc clears the query, the next one closes the palette.
                if query.is_empty() {
                    self.clear();
                } else {
                    self.area.set_text("/");
                    self.area.set_cursor(1);
                    self.palette.set_query("");
                }
                Some(Outcome::Handled)
            }
            KeyCode::Enter | KeyCode::Tab => {
                let row = rows.get(self.palette.current(&rows))?;
                // A command that needs a session is still run: the app says
                // why it cannot, as the full build does.
                if !row.selectable() {
                    return Some(Outcome::Handled);
                }
                match &row.kind {
                    palette::Kind::Command(command) => {
                        let name = command.name.to_owned();
                        self.clear();
                        Some(Outcome::Command {
                            name,
                            args: String::new(),
                        })
                    }
                    palette::Kind::Skill(name) => {
                        // A skill is inserted as a mention, not run.
                        let mention = format!("$skill: {name} ");
                        self.clear();
                        self.area.insert_str(&mention);
                        Some(Outcome::Handled)
                    }
                    palette::Kind::Heading | palette::Kind::Item(_) => Some(Outcome::Handled),
                }
            }
            _ => None,
        }
    }

    fn submit(&mut self) -> Outcome {
        let expanded = self.expanded_text();
        if expanded.trim().is_empty() {
            return Outcome::Handled;
        }
        // A typed `/name args` is a command, not a message.
        if let Some(rest) = expanded.trim().strip_prefix('/') {
            let (name, args) = match rest.split_once(char::is_whitespace) {
                Some((name, args)) => (name, args.trim()),
                None => (rest, ""),
            };
            // `suspend` is native: it is not in the generated registry.
            let native = name == "suspend";
            if let Some(command) = commands::COMMANDS
                .iter()
                .find(|c| c.name == name || c.aliases.contains(&name))
                .map(|c| c.name)
                .or(native.then_some("suspend"))
            {
                let outcome = Outcome::Command {
                    name: command.to_owned(),
                    args: args.to_owned(),
                };
                self.clear();
                return outcome;
            }
        }
        self.clear();
        Outcome::Submit(expanded)
    }

    /// The draft with every surviving token replaced by its content.
    pub fn expanded_text(&self) -> String {
        let text = self.area.text();
        let mut elements: Vec<_> = self.area.elements().to_vec();
        elements.sort_by_key(|e| e.range.start);
        let mut out = String::new();
        let mut at = 0;
        for element in elements {
            if element.range.start < at || element.range.end > text.len() {
                continue;
            }
            out.push_str(&text[at..element.range.start]);
            match self.tokens.iter().find(|t| t.id == element.id) {
                Some(token) => out.push_str(&token.content),
                None => out.push_str(&text[element.range.clone()]),
            }
            at = element.range.end;
        }
        out.push_str(&text[at..]);
        out
    }

    fn prune_tokens(&mut self) {
        let live: Vec<ElementId> = self.area.elements().iter().map(|e| e.id).collect();
        self.tokens.retain(|t| live.contains(&t.id));
    }

    // ---- history -----------------------------------------------------------

    /// Up on a single-line draft at the start walks older entries; Down while
    /// browsing, at either end, walks back toward the saved draft.
    fn browse_history(&mut self, up: bool) -> bool {
        if self.history.is_empty() || self.area.text().contains('\n') {
            return false;
        }
        let cursor = self.area.cursor();
        let len = self.area.text().len();
        let browsing = self.history_index.is_some_and(|i| i < self.history.len());
        if up {
            if cursor != 0 {
                return false;
            }
            if !browsing {
                self.saved_draft = self.area.text().to_owned();
                self.history_index = Some(self.history.len() - 1);
            } else if let Some(index) = self.history_index.filter(|i| *i > 0) {
                self.history_index = Some(index - 1);
            }
            self.load_history(0);
            return true;
        }
        if !browsing || (cursor != 0 && cursor != len) {
            return false;
        }
        match self.history_index {
            Some(index) if index + 1 < self.history.len() => {
                self.history_index = Some(index + 1);
                self.load_history(0);
            }
            _ => {
                self.history_index = None;
                let saved = std::mem::take(&mut self.saved_draft);
                self.area.set_text(&saved);
                self.area.set_cursor(saved.len());
                self.tokens.clear();
            }
        }
        true
    }

    fn load_history(&mut self, cursor: usize) {
        if let Some(entry) = self
            .history_index
            .and_then(|i| self.history.get(i))
            .cloned()
        {
            self.area.set_text(&entry);
            self.area.set_cursor(cursor.min(entry.len()));
            self.tokens.clear();
        }
    }

    // ---- paste -------------------------------------------------------------

    pub fn paste(&mut self, text: &str) {
        let text = sanitize(text);
        if text.is_empty() {
            return;
        }
        let chars = text.chars().count();
        let lines = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .count();
        let (label, content) = if chars > LONG_TEXT_PASTE_MIN_CHARS {
            (
                format!("[Text: {} ({chars} chars)]", preview(&text)),
                text.clone(),
            )
        } else if chars >= MULTILINE_PASTE_MIN_CHARS || lines >= MULTILINE_PASTE_MIN_LINES {
            (format!("[... {lines} lines]"), text.clone())
        } else {
            self.area
                .insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
            return;
        };
        let label = self.unique_label(label);
        // Keep a space between the token and the text around it.
        let cursor = self.area.cursor();
        let need_leading = {
            let before = &self.area.text()[..cursor];
            !before.is_empty() && !before.ends_with(char::is_whitespace)
        };
        let need_trailing = {
            let after = &self.area.text()[cursor..];
            !after.is_empty() && !after.starts_with(char::is_whitespace)
        };
        if need_leading {
            self.area.insert_str(" ");
        }
        let display = Line::from(Span::styled(
            label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        let id = self.area.insert_element(&label, TOKEN_KIND, Some(display));
        if need_trailing {
            self.area.insert_str(" ");
        }
        self.tokens.push(Token { id, label, content });
    }

    fn unique_label(&self, base: String) -> String {
        let existing: Vec<&str> = self.tokens.iter().map(|t| t.label.as_str()).collect();
        if !existing.contains(&base.as_str()) {
            return base;
        }
        let stem = &base[..base.len() - 1];
        let mut counter = 2;
        loop {
            let candidate = format!("{stem} #{counter}]");
            if !existing.contains(&candidate.as_str()) {
                return candidate;
            }
            counter += 1;
        }
    }

    // ---- drawing -----------------------------------------------------------

    /// Rows of the input at `text_width` columns (without prompt/border), the
    /// cursor (row, column) inside them, and how many rows the draft needs.
    pub fn render_rows(&mut self, text_width: u16, p: &Palette) -> (Vec<String>, (u16, u16)) {
        let width = text_width.max(1);
        let needed = self.area.desired_height(width).max(1);
        let height = needed.min(MAX_INPUT_ROWS as u16);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        (&self.area).render_ref(area, &mut buffer, &mut self.state);
        let cursor = self
            .area
            .cursor_pos_with_state(area, self.state)
            .unwrap_or((0, 0));

        let text = parse_hex_rgb(&p.text);
        let accent = parse_hex_rgb(&p.accent);
        let mut rows = Vec::with_capacity(height as usize);
        for y in 0..height {
            let mut cells: Vec<Cell> = Vec::new();
            let mut x = 0;
            while x < width {
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol().to_owned();
                let w = unicode_width::UnicodeWidthStr::width(symbol.as_str());
                let mut style = Style::default();
                if let Some(rgb) = text {
                    style = style.fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2));
                }
                if cell.modifier.contains(Modifier::BOLD) {
                    // A token: accent and bold.
                    style = style.add_modifier(Modifier::BOLD);
                    if let Some(rgb) = accent {
                        style = style.fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2));
                    }
                }
                if w > 0 {
                    cells.push((symbol, style, w, None));
                }
                x += w.max(1) as u16;
            }
            // Trailing blanks are dropped; the box pads to its width.
            while cells.last().is_some_and(|c| c.0 == " ") {
                cells.pop();
            }
            rows.push(cells_to_ansi(&cells, &[]));
        }
        (rows, cursor)
    }

    /// The palette panel when a command is being typed.
    /// The fullscreen palette box with `page` command rows, and the width
    /// of its query (the cursor goes after it).
    pub fn palette_overlay(
        &mut self,
        width: usize,
        page: usize,
        p: &Palette,
    ) -> Option<(Vec<String>, usize)> {
        let query = self.command_query()?.to_owned();
        self.palette.set_query(&query);
        let rows = self.palette_rows();
        let box_rows = palette::render_overlay(&self.palette, &rows, width, page, p);
        Some((
            box_rows,
            crate::ui::transcript::blocks::display_width(&query),
        ))
    }

    pub fn palette_panel(&mut self, width: usize, p: &Palette) -> Option<Vec<String>> {
        let query = self.command_query()?.to_owned();
        self.palette.set_query(&query);
        let rows = self.palette_rows();
        Some(palette::render(&self.palette, &rows, width, p))
    }
}

fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let [r, g, b] = crate::ui::theme::contrast::parse_hex(hex)?;
    Some((
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ))
}

/// Control characters other than tab and newline are dropped; carriage
/// returns become newlines.
fn sanitize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

fn preview(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned: String = collapsed
        .chars()
        .filter(|c| *c != '[' && *c != ']')
        .collect();
    let cleaned = cleaned.trim().to_owned();
    let head: String = cleaned.chars().take(32).collect();
    if head.is_empty() {
        return "pasted text".to_owned();
    }
    if collapsed.trim().chars().count() > head.chars().count() {
        format!("{head}...")
    } else {
        head
    }
}

fn to_crossterm(key: KeyEvent) -> Option<CtKey> {
    let code = match key.code {
        KeyCode::Char(c) => CtCode::Char(c),
        KeyCode::Enter => CtCode::Enter,
        KeyCode::Tab => CtCode::Tab,
        KeyCode::BackTab => CtCode::BackTab,
        KeyCode::Backspace => CtCode::Backspace,
        KeyCode::Delete => CtCode::Delete,
        KeyCode::Esc => CtCode::Esc,
        KeyCode::Up => CtCode::Up,
        KeyCode::Down => CtCode::Down,
        KeyCode::Left => CtCode::Left,
        KeyCode::Right => CtCode::Right,
        KeyCode::Home => CtCode::Home,
        KeyCode::End => CtCode::End,
        KeyCode::PageUp => CtCode::PageUp,
        KeyCode::PageDown => CtCode::PageDown,
        KeyCode::Insert => CtCode::Insert,
        KeyCode::F(n) => CtCode::F(n),
    };
    let mut modifiers = KeyModifiers::NONE;
    if key.mods.contains(Modifiers::SHIFT) {
        modifiers |= KeyModifiers::SHIFT;
    }
    if key.mods.contains(Modifiers::ALT) {
        modifiers |= KeyModifiers::ALT;
    }
    if key.mods.contains(Modifiers::CTRL) {
        modifiers |= KeyModifiers::CONTROL;
    }
    if key.mods.contains(Modifiers::SUPER) {
        modifiers |= KeyModifiers::SUPER;
    }
    Some(CtKey::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode, Theme};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn typed(c: &mut Composer, text: &str) {
        for ch in text.chars() {
            c.handle_key(key(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn enter_submits_and_shift_enter_adds_a_line() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "hello");
        c.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT));
        typed(&mut c, "world");
        assert_eq!(c.text(), "hello\nworld");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit("hello\nworld".into())
        );
        assert!(c.is_empty());
    }

    #[test]
    fn empty_enter_does_nothing() {
        let mut c = Composer::new(Vec::new());
        assert_eq!(c.handle_key(key(KeyCode::Enter)), Outcome::Handled);
    }

    #[test]
    fn long_pastes_become_one_atomic_token_that_expands_on_submit() {
        let mut c = Composer::new(Vec::new());
        let block = (1..=12)
            .map(|n| format!("pasted line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        c.paste(&block);
        assert_eq!(c.text(), "[... 12 lines]");
        // One backspace removes the whole token, not a character of it.
        let mut again = Composer::new(Vec::new());
        again.paste(&block);
        again.handle_key(key(KeyCode::Backspace));
        assert_eq!(again.text(), "");
        assert_eq!(c.handle_key(key(KeyCode::Enter)), Outcome::Submit(block));
    }

    #[test]
    fn short_pastes_are_inserted_verbatim_and_labels_stay_unique() {
        let mut c = Composer::new(Vec::new());
        c.paste("short\ntext");
        assert_eq!(c.text(), "short\ntext");
        c.clear();
        let block = "a\nb\nc\nd\ne";
        c.paste(block);
        c.paste(block);
        assert_eq!(c.text(), "[... 5 lines] [... 5 lines #2]");
    }

    #[test]
    fn history_walks_back_and_returns_the_saved_draft() {
        let mut c = Composer::new(vec!["first".into(), "second".into()]);
        typed(&mut c, "draft");
        // Up from the end first moves to the start (a single row).
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "draft");
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "second");
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "first");
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "second");
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "draft");
    }

    #[test]
    fn typed_suspend_is_a_native_command_and_ctrl_z_stays_undo() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "/suspend");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Command {
                name: "suspend".into(),
                args: String::new(),
            }
        );
        // Ctrl+Z reaches the editor: it is not a suspend key any more.
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "draft");
        c.handle_key(KeyEvent::new(KeyCode::Char('z'), Modifiers::CTRL));
        assert_ne!(c.text(), "draft", "ctrl+z undid the typing");
    }

    #[test]
    fn slash_opens_the_palette_and_enter_runs_the_selected_command() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "/");
        assert!(c.palette_open());
        typed(&mut c, "exit");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Command {
                name: "exit".into(),
                args: String::new()
            }
        );
        assert!(c.is_empty());
    }

    #[test]
    fn escape_clears_the_query_then_closes_the_palette() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "/hist");
        c.handle_key(key(KeyCode::Esc));
        assert_eq!(c.text(), "/");
        assert!(c.palette_open());
        c.handle_key(key(KeyCode::Esc));
        assert!(c.is_empty());
        assert!(!c.palette_open());
    }

    #[test]
    fn typed_commands_with_arguments_are_commands() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "/goal ship it");
        assert!(!c.palette_open(), "a space ends the command name");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Command {
                name: "goal".into(),
                args: "ship it".into()
            }
        );
        // Unknown slash text goes to the model.
        typed(&mut c, "/etc/hosts is odd");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit("/etc/hosts is odd".into())
        );
    }

    #[test]
    fn rows_scroll_after_seven_and_the_cursor_stays_visible() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let mut c = Composer::new(Vec::new());
        for n in 0..10 {
            typed(&mut c, &format!("line {n}"));
            c.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT));
        }
        let (rows, cursor) = c.render_rows(40, &theme.palette);
        assert_eq!(rows.len(), MAX_INPUT_ROWS);
        assert!((cursor.1 as usize) < rows.len());
    }
}
