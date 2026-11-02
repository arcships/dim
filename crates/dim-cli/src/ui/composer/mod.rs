//! The composer: the editable draft of the scrollback interface (design §6.7).
//!
//! Editing, wrapping, undo and kill/yank come from the vendored
//! `xai-ratatui-textarea`; this layer adds the dim contract on top: Enter
//! submits and Shift+Enter/Ctrl+J insert a newline, long pastes become atomic
//! tokens, Up/Down move by drawn row and fall through to the input history,
//! a `/` draft opens the command palette, `@` opens the file picker, files
//! dropped or pasted as paths become `[name]` tokens, and `!` / `!!` typed
//! into an empty draft switch it to shell mode.

pub mod history;
pub mod mentions;
pub mod palette;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidgetRef;
use xai_ratatui_textarea::{ElementId, ElementKind, TextArea, TextAreaState};

use crate::attachments::{MAX_IMAGES, file_link};
use crate::model::session::DraftPart;
use crate::ui::i18n::{t, tf};
use crate::ui::render::markdown::{Cell, cells_to_ansi};
use crate::ui::terminal::{KeyCode, KeyEvent, Modifiers, to_crossterm};
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
    /// The marker the draft draws for the token.
    label: String,
    /// The token's text in the buffer: what the cursor moves over and what
    /// select-and-copy takes. An image's marker is never this text.
    buffer: String,
    /// What the token submits and the input history keeps: a file's link, a
    /// paste's text, or nothing for a clipboard image.
    content: String,
    /// A clipboard image, sent as its own content block.
    block: Option<dim_agent_types::ContentBlock>,
    /// A local file or directory, dropped, pasted or mentioned with `@`:
    /// sent as a link to its path, or an image the model accepts as the
    /// image itself.
    file: Option<std::path::PathBuf>,
    /// What an image's sent form changed, after its marker's index.
    details: String,
}

/// Shell mode: the draft is a command to run, not a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// `!`: the result is sent to the model as context.
    Recorded,
    /// `!!`: the result stays in the terminal.
    Private,
}

impl Shell {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Recorded => "!",
            Self::Private => "!!",
        }
    }

    /// The mode and command of a history entry such as `!!git status`.
    pub fn split(entry: &str) -> (Option<Self>, &str) {
        if let Some(rest) = entry.strip_prefix("!!") {
            (Some(Self::Private), rest)
        } else if let Some(rest) = entry.strip_prefix('!') {
            (Some(Self::Recorded), rest)
        } else {
            (None, entry)
        }
    }
}

/// What a key did.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Consumed (or ignored) with no effect on the app.
    Handled,
    /// The draft was submitted; the string is the expanded content.
    Submit(String),
    /// A draft holding inserted files or a paste to store: `text` is what
    /// the input history keeps.
    SubmitDraft { text: String, parts: Vec<DraftPart> },
    /// A shell-mode draft was submitted: the command to run.
    Shell { command: String, shell: Shell },
    /// A palette row (or a typed `/name`) was chosen.
    Command { name: String, args: String },
    /// The draft cannot be sent as it is; it stays for editing.
    Rejected(String),
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
    shell: Option<Shell>,
    saved_shell: Option<Shell>,
    palette: PaletteState,
    context: crate::ui::commands::Context,
    skills: Vec<palette::Skill>,
    /// Whether the model takes images; unknown counts as yes.
    accepts_images: bool,
    /// The files `@` searches, once the working directory is known.
    mentions: Option<mentions::Mentions>,
    picker: mentions::Picker,
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
            shell: None,
            saved_shell: None,
            palette: PaletteState::default(),
            context: crate::ui::commands::Context::default(),
            skills: Vec::new(),
            accepts_images: true,
            mentions: None,
            picker: mentions::Picker::default(),
        }
    }

    /// The directory `@` mentions are relative to.
    pub fn set_root(&mut self, root: std::path::PathBuf) {
        if self.mentions.as_ref().is_none_or(|m| m.root() != root) {
            self.mentions = Some(mentions::Mentions::new(root));
        }
    }

    /// Whether the current model takes image input: an image file the
    /// draft names is then sent as the image, otherwise as its path.
    pub fn set_accepts_images(&mut self, accepts: bool) {
        if self.accepts_images == accepts {
            return;
        }
        self.accepts_images = accepts;
        self.renumber();
    }

    /// What the palette weighs availability against.
    pub fn set_context(&mut self, context: crate::ui::commands::Context) {
        self.context = context;
    }

    /// Replaces the draft (for example with a message taken back from the
    /// queue); the cursor goes to the end.
    pub fn set_draft(&mut self, text: &str) {
        self.clear();
        self.area.set_text(text);
        self.area.set_cursor(text.len());
    }

    /// Appends `text` to the draft as a Markdown quote and a blank line,
    /// keeping what is already there; the cursor goes to the end. The
    /// format is the one the Web composer writes (member-conversation
    /// design, cases in `web/src/features/chat/quote-cases.json`). A
    /// shell draft takes no quote: it would run as part of the command.
    /// Returns whether the draft changed.
    pub fn quote(&mut self, text: &str) -> bool {
        let text = text.replace("\r\n", "\n");
        let text = text.strip_suffix('\n').unwrap_or(&text);
        if self.shell.is_some() || text.is_empty() {
            return false;
        }
        let quote: String = text
            .split('\n')
            .map(|line| match line {
                "" => ">\n".to_owned(),
                line => format!("> {line}\n"),
            })
            .collect();
        let draft = self.area.text();
        let gap = if draft.is_empty() || draft.ends_with("\n\n") {
            ""
        } else if draft.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let end = draft.len();
        self.area.set_cursor(end);
        self.area.insert_str(&format!("{gap}{quote}\n"));
        true
    }

    /// Skills the palette offers as `$skill:` mentions.
    #[allow(dead_code)]
    pub fn set_skills(&mut self, skills: Vec<palette::Skill>) {
        self.skills = skills;
    }

    /// The draft as the buffer holds it: a token's marker is not this text
    /// when it differs from what the token stands for.
    pub fn text(&self) -> &str {
        self.area.text()
    }

    pub fn is_empty(&self) -> bool {
        self.area.text().is_empty()
    }

    pub fn shell(&self) -> Option<Shell> {
        self.shell
    }

    pub fn clear(&mut self) {
        self.area.set_text("");
        self.tokens.clear();
        self.history_index = None;
        self.saved_draft.clear();
        self.shell = None;
        self.saved_shell = None;
        self.palette = PaletteState::default();
        self.picker = mentions::Picker::default();
        self.state = TextAreaState::default();
    }

    /// Replaces the history (after a session or directory change).
    pub fn push_history(&mut self, entry: &str) {
        if entry.is_empty() {
            return;
        }
        self.history.retain(|existing| existing != entry);
        self.history.push(entry.to_owned());
        let skip = self.history.len().saturating_sub(10);
        self.history.drain(..skip);
    }

    /// The query after the leading `/` when the draft is a command being
    /// typed: one line, starting with `/`, no whitespace yet.
    fn command_query(&self) -> Option<&str> {
        if self.shell.is_some() {
            return None;
        }
        let text = self.area.text();
        let rest = text.strip_prefix('/')?;
        (!rest.contains(char::is_whitespace)).then_some(rest)
    }

    pub fn palette_open(&self) -> bool {
        self.command_query().is_some()
    }

    fn palette_rows(&self) -> Vec<crate::ui::rows::Row> {
        palette::rows_for(
            self.command_query().unwrap_or(""),
            self.context,
            &self.skills,
        )
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let plain = key.mods.0 & !Modifiers::SHIFT.0 == 0;
        let ctrl_only = key.mods.contains(Modifiers::CTRL) && key.mods.0 == Modifiers::CTRL.0;

        if self.palette_open()
            && let Some(outcome) = self.palette_key(key)
        {
            return outcome;
        }
        if let Some(outcome) = self.mention_key(key) {
            return outcome;
        }
        if self.area.text().is_empty() {
            match key.code {
                KeyCode::Char('!') if plain && self.shell != Some(Shell::Private) => {
                    self.shell = Some(match self.shell {
                        None => Shell::Recorded,
                        Some(_) => Shell::Private,
                    });
                    return Outcome::Handled;
                }
                KeyCode::Backspace if key.mods.0 == 0 && self.shell.is_some() => {
                    self.shell = match self.shell {
                        Some(Shell::Private) => Some(Shell::Recorded),
                        _ => None,
                    };
                    return Outcome::Handled;
                }
                _ => {}
            }
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
        if key.code == KeyCode::Enter
            && crate::ui::commands::COMMANDS.iter().any(|command| {
                command.slash
                    && !command.listed(self.context)
                    && (command.name == query || command.aliases.contains(&query.as_str()))
            })
        {
            return Some(self.submit());
        }
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
                if !row.selectable() {
                    return Some(Outcome::Handled);
                }
                match &row.kind {
                    crate::ui::rows::Kind::Command(command)
                        if command.requires_exact_name
                            && query != command.name
                            && !command.aliases.contains(&query.as_str()) =>
                    {
                        let filled = format!("/{}", command.name);
                        self.clear();
                        self.area.insert_str(&filled);
                        Some(Outcome::Handled)
                    }
                    crate::ui::rows::Kind::Command(command) if command.argument.is_some() => {
                        let filled = format!("/{} ", command.name);
                        self.clear();
                        self.area.insert_str(&filled);
                        Some(Outcome::Handled)
                    }
                    crate::ui::rows::Kind::Command(command) => {
                        let name = command.name.to_owned();
                        self.clear();
                        Some(Outcome::Command {
                            name,
                            args: String::new(),
                        })
                    }
                    crate::ui::rows::Kind::Skill(name) => {
                        let mention = format!("$skill: {name} ");
                        self.clear();
                        self.area.insert_str(&mention);
                        Some(Outcome::Handled)
                    }
                    crate::ui::rows::Kind::Heading | crate::ui::rows::Kind::Item => {
                        Some(Outcome::Handled)
                    }
                }
            }
            _ => None,
        }
    }

    /// The `@` mention being typed, unless Esc closed it.
    fn mention(&self) -> Option<mentions::Trigger> {
        if self.shell.is_some() || self.palette_open() || self.mentions.is_none() {
            return None;
        }
        let trigger = mentions::trigger(self.area.text(), self.area.cursor())?;
        (self.picker.dismissed.as_ref() != Some(&trigger)).then_some(trigger)
    }

    /// Whether the file picker owns Up, Down, Enter, Tab and Esc.
    pub fn mention_open(&self) -> bool {
        self.mention().is_some()
    }

    /// True when the picker needs a frame: more files were found.
    pub fn mention_tick(&mut self) -> bool {
        self.mention_open() && self.mentions.as_mut().is_some_and(|m| m.changed())
    }

    fn mention_candidates(&mut self, query: &str) -> Vec<mentions::Candidate> {
        self.picker.follow(query);
        self.mentions
            .as_mut()
            .map(|m| m.candidates(query))
            .unwrap_or_default()
    }

    pub fn mention_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        let trigger = self.mention()?;
        if key.mods.0 != 0 {
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                self.picker.dismissed = Some(trigger);
                Some(Outcome::Handled)
            }
            KeyCode::Up | KeyCode::Down => {
                let len = self.mention_candidates(&trigger.query).len();
                self.picker
                    .step(if key.code == KeyCode::Up { -1 } else { 1 }, len);
                Some(Outcome::Handled)
            }
            KeyCode::Enter | KeyCode::Tab => {
                let mut candidates = self.mention_candidates(&trigger.query);
                if candidates.is_empty() {
                    return None;
                }
                let candidate =
                    candidates.swap_remove(self.picker.selected.min(candidates.len() - 1));
                self.accept_mention(&trigger, candidate);
                Some(Outcome::Handled)
            }
            _ => None,
        }
    }

    /// A directory is typed in to look inside it; a file becomes an atomic
    /// `@path` token followed by a space.
    fn accept_mention(&mut self, trigger: &mentions::Trigger, candidate: mentions::Candidate) {
        let range = trigger.start..self.area.cursor();
        let label = format!("@{}", candidate.text);
        if candidate.dir {
            self.area.replace_range(range.clone(), &label);
            self.area.set_cursor(range.start + label.len());
            return;
        }
        let path = std::path::absolute(&candidate.path).unwrap_or(candidate.path);
        let display = Line::from(Span::styled(
            label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        let id = self
            .area
            .replace_range_with_element(range, &label, TOKEN_KIND, Some(display));
        let cursor = self.area.cursor();
        if !self.area.text()[cursor..].starts_with(char::is_whitespace) {
            self.area.insert_str(" ");
        } else {
            self.area.set_cursor(cursor + 1);
        }
        self.tokens.push(Token {
            id,
            content: file_link(&label, &path),
            buffer: label.clone(),
            label,
            block: None,
            file: Some(path),
            details: String::new(),
        });
        self.renumber();
    }

    /// The file picker panel while an `@` mention is typed.
    pub fn mention_panel(&mut self, width: usize, p: &Palette) -> Option<Vec<String>> {
        let trigger = self.mention()?;
        let candidates = self.mention_candidates(&trigger.query);
        let searching = self.mentions.as_ref().is_some_and(|m| m.searching());
        Some(mentions::render(
            &self.picker,
            &candidates,
            searching,
            width,
            p,
        ))
    }

    fn submit(&mut self) -> Outcome {
        let expanded = self.expanded_text();
        // An image submits no text of its own, so an image-only message draft
        // is not empty. A shell command is text only, so there an image is
        // not a command.
        let image_only =
            self.shell.is_none() && self.tokens.iter().any(|token| self.is_image(token));
        if expanded.trim().is_empty() && !image_only {
            return Outcome::Handled;
        }
        if let Some(shell) = self.shell {
            self.clear();
            return Outcome::Shell {
                command: expanded.trim().to_owned(),
                shell,
            };
        }
        if let Some(rest) = expanded.trim().strip_prefix('/') {
            let (name, args) = match rest.split_once(char::is_whitespace) {
                Some((name, args)) => (name, args.trim()),
                None => (rest, ""),
            };
            if let Some(command) = crate::ui::commands::COMMANDS
                .iter()
                .find(|c| c.slash && (c.name == name || c.aliases.contains(&name)))
            {
                if let Some(argument) = command.argument
                    && args.is_empty()
                {
                    return Outcome::Rejected(tf(
                        "/{name} needs {argument}",
                        &[("name", command.name), ("argument", argument)],
                    ));
                }
                if command.argument.is_some()
                    && let Some(image) = self.tokens.iter().find(|token| token.block.is_some())
                {
                    return Outcome::Rejected(tf(
                        "/{name} takes text only; remove {label} to send",
                        &[("name", command.name), ("label", &image.label)],
                    ));
                }
                let outcome = Outcome::Command {
                    name: command.name.to_owned(),
                    args: args.to_owned(),
                };
                self.clear();
                return outcome;
            }
        }
        let parts = match self.parts() {
            Ok(parts) => parts,
            Err(reason) => return Outcome::Rejected(reason),
        };
        self.clear();
        if parts
            .iter()
            .any(|part| matches!(part, DraftPart::Block(_) | DraftPart::Paste(_)))
        {
            return Outcome::SubmitDraft {
                text: expanded,
                parts,
            };
        }
        Outcome::Submit(expanded)
    }

    /// The draft in order: typed text, pastes and inserted blocks. A file
    /// is its link, or the image itself when the model takes images.
    fn parts(&self) -> Result<Vec<DraftPart>, String> {
        let text = self.area.text();
        let mut elements: Vec<_> = self.area.elements().to_vec();
        elements.sort_by_key(|e| e.range.start);
        let mut parts = Vec::new();
        let mut typed = String::new();
        let mut at = 0;
        for element in elements {
            if element.range.start < at || element.range.end > text.len() {
                continue;
            }
            typed.push_str(&text[at..element.range.start]);
            match self.tokens.iter().find(|t| t.id == element.id) {
                Some(token) => {
                    let part = match (&token.block, &token.file) {
                        (Some(_), None) if !self.accepts_images => {
                            return Err(tf(
                                "The current model does not accept images; remove {label} to send",
                                &[("label", &token.label)],
                            ));
                        }
                        (Some(block), None) => DraftPart::Block(block.clone()),
                        (Some(block), Some(_)) if self.accepts_images => {
                            DraftPart::Block(block.clone())
                        }
                        (Some(_), Some(_)) => {
                            typed.push_str(&token.content);
                            at = element.range.end;
                            continue;
                        }
                        (None, Some(path))
                            if self.accepts_images && crate::images::is_image(path) =>
                        {
                            match crate::images::file(path) {
                                Ok(crate::images::Item::Image(image)) => {
                                    DraftPart::Block(image.block())
                                }
                                Ok(crate::images::Item::Text(_)) => {
                                    DraftPart::Text(token.content.clone())
                                }
                                Err(error) => return Err(format!("{}: {error}", token.label)),
                            }
                        }
                        (None, Some(_)) => {
                            typed.push_str(&token.content);
                            at = element.range.end;
                            continue;
                        }
                        (None, None) => DraftPart::Paste(token.content.clone()),
                    };
                    if !typed.is_empty() {
                        parts.push(DraftPart::Text(std::mem::take(&mut typed)));
                    }
                    parts.push(part);
                }
                None => typed.push_str(&text[element.range.clone()]),
            }
            at = element.range.end;
        }
        typed.push_str(&text[at..]);
        if !typed.is_empty() {
            parts.push(DraftPart::Text(typed));
        }
        Ok(parts)
    }

    /// Decides what a group of pasted paths becomes without reading image
    /// bytes: absolute paths, token names, and the 20-image count.
    pub fn plan_files(&self, paths: &[std::path::PathBuf]) -> Result<Vec<PlannedFile>, String> {
        let mut images = self.image_count();
        let mut plan = Vec::new();
        for path in paths {
            let path = std::path::absolute(path).map_err(|error| error.to_string())?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let is_image = crate::images::is_image(&path);
            if is_image {
                images += 1;
                if images > MAX_IMAGES {
                    return Err(t("A submission accepts at most 20 images").to_owned());
                }
            }
            plan.push(PlannedFile {
                name,
                path,
                read: is_image && self.accepts_images,
            });
        }
        Ok(plan)
    }

    /// Inserts a planned group with the image blocks read for it. The group
    /// is taken whole or not at all.
    pub fn insert_files(
        &mut self,
        plan: Vec<PlannedFile>,
        images: Vec<Option<crate::images::Attached>>,
    ) {
        for (planned, image) in plan.into_iter().zip(images) {
            let content = file_link(&planned.name, &planned.path);
            self.insert_token(
                format!("[{}]", planned.name),
                content,
                image.as_ref().map(crate::images::Attached::block),
                Some(planned.path),
                image.as_ref().map(image_details).unwrap_or_default(),
            );
        }
    }

    /// Inserts an image the clipboard held: a token with no text and no
    /// path, which is why it shows a marker of its own.
    pub fn attach_image(&mut self, image: crate::images::Attached) -> Result<(), String> {
        if self.image_count() >= MAX_IMAGES {
            return Err(t("A submission accepts at most 20 images").to_owned());
        }
        self.insert_token(
            "[Image]".to_owned(),
            String::new(),
            Some(image.block()),
            None,
            image_details(&image),
        );
        Ok(())
    }

    fn image_count(&self) -> usize {
        self.tokens
            .iter()
            .filter(|t| {
                matches!(t.block, Some(dim_agent_types::ContentBlock::Image { .. }))
                    || t.file.as_deref().is_some_and(crate::images::is_image)
            })
            .count()
    }

    /// The draft with every surviving token replaced by its content: what
    /// the message carries and what the input history keeps.
    pub fn expanded_text(&self) -> String {
        self.joined(|token| &token.content)
    }

    /// The draft as it is drawn: every surviving token by its marker.
    #[cfg(test)]
    pub fn display_text(&self) -> String {
        self.joined(|token| &token.label)
    }

    fn joined(&self, text_of: fn(&Token) -> &str) -> String {
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
                Some(token) => out.push_str(text_of(token)),
                None => out.push_str(&text[element.range.clone()]),
            }
            at = element.range.end;
        }
        out.push_str(&text[at..]);
        out
    }

    fn prune_tokens(&mut self) {
        let live: Vec<ElementId> = self.area.elements().iter().map(|e| e.id).collect();
        let before = self.tokens.len();
        self.tokens.retain(|t| live.contains(&t.id));
        if self.tokens.len() != before {
            self.renumber();
        }
    }

    /// Up at the start of the draft walks older entries; on a multi-line draft
    /// the first Up on the top row moves the cursor there. Down while browsing
    /// at the end (or at the start of a single line) walks back toward the
    /// saved draft.
    fn browse_history(&mut self, up: bool) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let cursor = self.area.cursor();
        let len = self.area.text().len();
        let single_line = !self.area.text().contains('\n');
        let browsing = self.history_index.is_some_and(|i| i < self.history.len());
        if up {
            if cursor != 0 {
                return false;
            }
            if !browsing {
                self.saved_draft = self.area.text().to_owned();
                self.saved_shell = self.shell;
                self.history_index = Some(self.history.len() - 1);
            } else if let Some(index) = self.history_index.filter(|i| *i > 0) {
                self.history_index = Some(index - 1);
            }
            self.load_history(0);
            return true;
        }
        if !browsing || !(cursor == len || (cursor == 0 && single_line)) {
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
                self.shell = self.saved_shell.take();
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
            let (shell, entry) = Shell::split(&entry);
            self.shell = shell;
            self.area.set_text(entry);
            self.area.set_cursor(cursor.min(entry.len()));
            self.tokens.clear();
        }
    }

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
                tf(
                    "[Text: {preview} ({count} chars)]",
                    &[("preview", &preview(&text)), ("count", &chars.to_string())],
                ),
                text.clone(),
            )
        } else if chars >= MULTILINE_PASTE_MIN_CHARS || lines >= MULTILINE_PASTE_MIN_LINES {
            (
                tf("[... {count} lines]", &[("count", &lines.to_string())]),
                text.clone(),
            )
        } else {
            self.area
                .insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
            return;
        };
        self.insert_token(label, content, None, None, String::new());
    }

    /// Inserts an atomic token, keeping a space between it and the text
    /// around it. The buffer holds `buffer`; what the draft draws comes from
    /// `renumber`.
    fn insert_token(
        &mut self,
        buffer: String,
        content: String,
        block: Option<dim_agent_types::ContentBlock>,
        file: Option<std::path::PathBuf>,
        details: String,
    ) {
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
        let id = self
            .area
            .insert_element(&buffer, TOKEN_KIND, Some(token_line(&buffer)));
        if need_trailing {
            self.area.insert_str(" ");
        }
        self.tokens.push(Token {
            id,
            label: buffer.clone(),
            buffer,
            content,
            block,
            file,
            details,
        });
        self.renumber();
    }

    /// Whether the token is sent as an image block: the decision `parts`
    /// makes when the draft goes.
    fn is_image(&self, token: &Token) -> bool {
        match (&token.block, &token.file) {
            (Some(_), None) => true,
            (Some(_), Some(_)) => self.accepts_images,
            (None, Some(path)) => self.accepts_images && crate::images::is_image(path),
            (None, None) => false,
        }
    }

    /// Numbers the draft's images 1..n in content order — the numbers the
    /// message shows — and names every other token, so adding or removing an
    /// image renumbers the rest.
    fn renumber(&mut self) {
        let mut elements: Vec<_> = self.area.elements().to_vec();
        elements.sort_by_key(|element| element.range.start);
        let mut images = 0;
        let mut taken: Vec<String> = Vec::new();
        let mut labels = Vec::with_capacity(self.tokens.len());
        for element in &elements {
            let Some(token) = self.tokens.iter().find(|token| token.id == element.id) else {
                continue;
            };
            let label = if self.is_image(token) {
                images += 1;
                crate::images::marker(images, &token.details)
            } else {
                let (stem, close) = match token.buffer.strip_suffix(']') {
                    Some(stem) if token.buffer.starts_with('[') => (stem, "]"),
                    _ => (token.buffer.as_str(), ""),
                };
                let mut label = token.buffer.clone();
                let mut repeat = 2;
                while taken.contains(&label) {
                    label = format!("{stem} #{repeat}{close}");
                    repeat += 1;
                }
                label
            };
            taken.push(label.clone());
            labels.push((element.id, label));
        }
        for (id, label) in labels {
            let Some(token) = self.tokens.iter_mut().find(|token| token.id == id) else {
                continue;
            };
            if token.label == label {
                continue;
            }
            token.label = label.clone();
            self.area.set_element_display(id, Some(token_line(&label)));
        }
    }

    /// Rows of the input at `text_width` columns (without prompt/border),
    /// bounded by `max_rows`, and the cursor (column, row) inside them.
    pub fn render_rows(
        &mut self,
        text_width: u16,
        max_rows: usize,
        p: &Palette,
    ) -> (Vec<String>, (u16, u16)) {
        let width = text_width.max(1);
        let needed = self.area.desired_height(width).max(1);
        let height = needed.min(max_rows.clamp(1, MAX_INPUT_ROWS) as u16);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        (&self.area).render_ref(area, &mut buffer, &mut self.state);
        let cursor = self
            .area
            .cursor_pos_with_state(area, self.state)
            .unwrap_or((0, 0));

        let text = crate::ui::theme::ansi::tui_color(&p.text);
        let accent = crate::ui::theme::ansi::tui_color(&p.accent);
        let mut rows = Vec::with_capacity(height as usize);
        for y in 0..height {
            let mut cells: Vec<Cell> = Vec::new();
            let mut x = 0;
            while x < width {
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol().to_owned();
                let w = unicode_width::UnicodeWidthStr::width(symbol.as_str());
                let mut style = Style::default();
                if let Some(color) = text {
                    style = style.fg(color);
                }
                if cell.modifier.contains(Modifier::BOLD) {
                    style = style.add_modifier(Modifier::BOLD);
                    if let Some(color) = accent {
                        style = style.fg(color);
                    }
                }
                if w > 0 {
                    cells.push((symbol, style, w, None));
                }
                x += w.max(1) as u16;
            }
            while cells.last().is_some_and(|c| c.0 == " ") {
                cells.pop();
            }
            rows.push(cells_to_ansi(&cells, &[]));
        }
        if let Some(argument) = self.argument_hint() {
            rows[0].push_str(&crate::ui::theme::ansi::fg(
                &format!(" {argument}"),
                &p.muted,
            ));
        }
        (rows, cursor)
    }

    /// The argument still to type after a command that takes one, as
    /// `/branch ` leaves it: `<prompt>`.
    fn argument_hint(&self) -> Option<&'static str> {
        if self.shell.is_some() {
            return None;
        }
        let name = self.area.text().strip_prefix('/')?.strip_suffix(' ')?;
        crate::ui::commands::COMMANDS
            .iter()
            .find(|c| c.slash && c.name == name)?
            .argument
    }

    /// The fullscreen palette and its query cursor within the row budget.
    pub fn palette_overlay(
        &mut self,
        width: usize,
        max_rows: usize,
        p: &Palette,
    ) -> Option<crate::ui::render::Overlay> {
        let query = self.command_query()?.to_owned();
        self.palette.set_query(&query);
        let rows = self.palette_rows();
        Some(palette::render_overlay(
            &self.palette,
            &rows,
            width,
            max_rows,
            p,
        ))
    }

    pub fn palette_panel(
        &mut self,
        width: usize,
        max_rows: usize,
        p: &Palette,
    ) -> Option<Vec<String>> {
        let query = self.command_query()?.to_owned();
        self.palette.set_query(&query);
        let rows = self.palette_rows();
        Some(palette::render(&self.palette, &rows, width, max_rows, p))
    }
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
        return t("pasted text").to_owned();
    }
    if collapsed.trim().chars().count() > head.chars().count() {
        format!("{head}...")
    } else {
        head
    }
}

/// One path a paste will attach, decided without reading image bytes.
#[derive(Clone)]
pub struct PlannedFile {
    /// The token's name: the file name, or the whole path when it has none.
    pub name: String,
    /// The absolute path the token submits.
    pub path: std::path::PathBuf,
    /// Whether the model takes images, so the file's bytes must be read.
    pub read: bool,
}

/// Reads the planned image entries. A path that turned out not to be an
/// image comes back as `None`; the first refusal names its file. This is the
/// blocking part of an attach, so a caller with a thread of its own runs it
/// there.
pub fn read_planned(plan: &[PlannedFile]) -> Result<Vec<Option<crate::images::Attached>>, String> {
    let mut images = Vec::with_capacity(plan.len());
    for planned in plan {
        if !planned.read {
            images.push(None);
            continue;
        }
        let image = match crate::images::file(&planned.path)
            .map_err(|error| format!("{}: {error}", planned.name))?
        {
            crate::images::Item::Image(image) => Some(image),
            crate::images::Item::Text(_) => None,
        };
        images.push(image);
    }
    Ok(images)
}

/// What a marker adds to its plain form: both sizes when the sent size
/// differs from the original, both media types when the encoding changed the
/// type, the first-frame note of an animation, and a re-encode that kept both
/// the size and the type.
fn image_details(image: &crate::images::Attached) -> String {
    let mut details = String::new();
    if image.resized() {
        details.push_str(" · ");
        details.push_str(&image.sizes());
    }
    if let Some(types) = image.media_types() {
        details.push_str(" · ");
        details.push_str(&types);
    }
    if image.animated {
        details.push_str(" · ");
        details.push_str(t("first frame"));
    }
    if image.reencoded() {
        details.push_str(" · ");
        details.push_str(t("re-encoded"));
    }
    details
}

/// A token's text as the draft draws it: the accent the input uses for what
/// is not typed text.
fn token_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{Appearance, ColorMode, Theme};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    /// Attaches a group the way an interface does, reading on this thread.
    fn attach(c: &mut Composer, paths: &[std::path::PathBuf]) -> Result<(), String> {
        let plan = c.plan_files(paths)?;
        let images = read_planned(&plan)?;
        c.insert_files(plan, images);
        Ok(())
    }

    fn typed(c: &mut Composer, text: &str) {
        for ch in text.chars() {
            c.handle_key(key(KeyCode::Char(ch)));
        }
    }

    fn bitmap() -> crate::images::Attached {
        crate::images::from_pixels(image::DynamicImage::new_rgb8(12, 8)).unwrap()
    }

    /// A two-frame GIF: the sent form is its first frame.
    fn animated_gif() -> Vec<u8> {
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder
                .encode_frames([
                    image::Frame::new(image::RgbaImage::from_pixel(
                        2,
                        2,
                        image::Rgba([255, 0, 0, 255]),
                    )),
                    image::Frame::new(image::RgbaImage::from_pixel(
                        2,
                        2,
                        image::Rgba([0, 0, 255, 255]),
                    )),
                ])
                .unwrap();
        }
        gif
    }

    /// The transcript row a message made of these parts shows.
    fn transcript(parts: &[DraftPart]) -> String {
        let content: Vec<serde_json::Value> = parts
            .iter()
            .map(|part| match part {
                DraftPart::Text(text) | DraftPart::Paste(text) => {
                    serde_json::json!({"type": "text", "text": text})
                }
                DraftPart::Block(block) => serde_json::to_value(block).unwrap(),
            })
            .collect();
        let input = serde_json::json!({
            "id": "input", "sequence": 1, "create_at": 1,
            "type": "input", "name": "message",
            "data": {"user": {"content": content}, "mode": "queue"},
        });
        let applied = serde_json::json!({
            "id": "applied", "sequence": 2, "create_at": 2,
            "type": "input", "name": "applied",
            "data": {"input_id": "input"},
        });
        let mut projection = crate::projection::Projection::default();
        projection.apply("main", &serde_json::from_value(input).unwrap());
        projection.apply("main", &serde_json::from_value(applied).unwrap());
        let row = &projection.snapshot().rows[0];
        row.display.clone().unwrap_or_else(|| row.text.clone())
    }

    /// The leading token of every image marker, without the details an
    /// attach may add after it.
    fn markers(text: &str) -> Vec<String> {
        text.match_indices("[Image #")
            .map(|(start, _)| {
                let rest = &text[start + "[Image #".len()..];
                let digits = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                format!("[Image #{}]", &rest[..digits])
            })
            .collect()
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
    fn long_pastes_become_one_atomic_token_that_submits_as_a_paste_part() {
        let mut c = Composer::new(Vec::new());
        let block = (1..=12)
            .map(|n| format!("pasted line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        c.paste(&block);
        assert_eq!(c.text(), "[... 12 lines]");
        let mut again = Composer::new(Vec::new());
        again.paste(&block);
        again.handle_key(key(KeyCode::Backspace));
        assert_eq!(again.text(), "");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::SubmitDraft {
                text: block.clone(),
                parts: vec![DraftPart::Paste(block)],
            }
        );
        let mut short = Composer::new(Vec::new());
        short.paste("one\ntwo\nthree");
        assert_eq!(
            short.handle_key(key(KeyCode::Enter)),
            Outcome::Submit("one\ntwo\nthree".into())
        );
    }

    #[test]
    fn a_quoted_message_joins_the_draft_and_submits_with_it() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../../../web/src/features/chat/quote-cases.json"
        ))
        .unwrap();
        for case in cases {
            let field = |name: &str| case[name].as_str().unwrap().to_owned();
            let (draft, result) = (field("draft"), field("result"));
            let mut c = Composer::new(Vec::new());
            c.set_draft(&draft);
            assert_eq!(
                c.quote(&field("message")),
                draft != result,
                "{}",
                field("name")
            );
            assert_eq!(c.text(), result, "{}", field("name"));
            assert_eq!(
                c.handle_key(key(KeyCode::Enter)),
                if result.trim().is_empty() {
                    Outcome::Handled
                } else {
                    Outcome::Submit(result.clone())
                },
                "{}",
                field("name")
            );
        }
        let mut c = Composer::new(Vec::new());
        let block = (1..=12)
            .map(|n| format!("pasted line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        c.paste(&block);
        c.quote("earlier");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::SubmitDraft {
                text: format!("{block}\n\n> earlier\n\n"),
                parts: vec![
                    DraftPart::Paste(block),
                    DraftPart::Text("\n\n> earlier\n\n".into()),
                ],
            }
        );
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "!ls");
        assert!(!c.quote("earlier"));
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Shell {
                command: "ls".into(),
                shell: Shell::Recorded,
            }
        );
    }

    #[test]
    fn files_and_long_pastes_submit_as_ordered_parts() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("shot.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&image, png.into_inner()).unwrap();
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "plain").unwrap();
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "see");
        attach(&mut c, &[image.clone(), notes.clone()]).unwrap();
        let long = "one\ntwo\nthree\nfour".to_owned();
        c.paste(&long);
        assert!(
            c.text()
                .starts_with("see [shot.png] [notes.txt] [... 4 lines]"),
            "{}",
            c.text()
        );
        let Outcome::SubmitDraft { parts, .. } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a draft");
        };
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], DraftPart::Text("see ".into()));
        assert!(matches!(
            &parts[1],
            DraftPart::Block(dim_agent_types::ContentBlock::Image { source }) if source.media_type == "image/png"
        ));
        assert_eq!(
            parts[2],
            DraftPart::Text(format!(" [notes.txt]({}) ", notes.display()))
        );
        assert_eq!(parts[3], DraftPart::Paste(long));
        let mut c = Composer::new(Vec::new());
        let many = vec![image; 21];
        assert!(attach(&mut c, &many).is_err());
        assert_eq!(c.text(), "");
    }

    fn png(path: &std::path::Path) {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(path, png.into_inner()).unwrap();
    }

    fn wide_png(path: &std::path::Path, width: u32) {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(path, png.into_inner()).unwrap();
    }

    /// Deterministic RGB noise of `width`×`height` pixels.
    fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut step = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..width as usize * height as usize {
            let (red, green, blue) = (step(), step(), step());
            pixels.extend([(red >> 24) as u8, (green >> 24) as u8, (blue >> 24) as u8]);
        }
        pixels
    }

    /// Deterministic noise: a PNG within the edge limit whose base64 is over
    /// the byte limit, so it is sent as a JPEG at the same size.
    fn noise_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut bytes),
            &noise_rgb(width, height),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        bytes
    }

    fn noise_jpeg(width: u32, height: u32, quality: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality);
        encoder
            .encode(
                &noise_rgb(width, height),
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        bytes
    }

    /// `noise_jpeg`, with a minimal EXIF APP1 segment whose one IFD0 entry is
    /// the orientation: `3` turns the image 180° and `6` 90° clockwise.
    fn oriented_jpeg(width: u32, height: u32, orientation: u16) -> Vec<u8> {
        let mut bytes = noise_jpeg(width, height, 90);
        let mut exif = vec![0xFF, 0xE1, 0x00, 0x22];
        exif.extend(b"Exif\0\0");
        exif.extend(b"II\x2A\x00");
        exif.extend(8u32.to_le_bytes());
        exif.extend(1u16.to_le_bytes());
        exif.extend(0x0112u16.to_le_bytes());
        exif.extend(3u16.to_le_bytes());
        exif.extend(1u32.to_le_bytes());
        exif.extend(orientation.to_le_bytes());
        exif.extend([0, 0]);
        exif.extend(0u32.to_le_bytes());
        bytes.splice(2..2, exif);
        bytes
    }

    #[test]
    fn images_follow_what_the_model_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("shot.png");
        png(&image);
        let mut c = Composer::new(Vec::new());
        c.set_accepts_images(false);
        attach(&mut c, std::slice::from_ref(&image)).unwrap();
        attach(&mut c, std::slice::from_ref(&image)).unwrap();
        assert_eq!(c.display_text(), "[shot.png] [shot.png #2]");
        let link = format!("[shot.png]({})", image.display());
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit(format!("{link} {link}"))
        );
        let image = crate::images::from_bytes(std::fs::read(&image).unwrap()).unwrap();
        c.attach_image(image).unwrap();
        assert_eq!(c.display_text(), "[Image #1]");
        assert!(matches!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Rejected(reason) if reason.contains("[Image #1]")
        ));
        assert_eq!(c.display_text(), "[Image #1]");
        c.set_accepts_images(true);
        assert!(matches!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::SubmitDraft { parts, .. } if matches!(parts[..], [DraftPart::Block(_)])
        ));
    }

    #[test]
    fn a_path_attached_to_an_image_model_keeps_its_scaled_block_and_sizes() {
        use dim_agent_capabilities::images::MAX_EDGE;
        let width = MAX_EDGE * 2;
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("wide.png");
        wide_png(&image, width);
        let mut c = Composer::new(Vec::new());
        attach(&mut c, std::slice::from_ref(&image)).unwrap();
        assert_eq!(
            c.display_text(),
            format!("[Image #1 · {width}×2 → {MAX_EDGE}×1]")
        );
        c.set_accepts_images(false);
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit(format!("[wide.png]({})", image.display()))
        );
        let mut c = Composer::new(Vec::new());
        attach(&mut c, std::slice::from_ref(&image)).unwrap();
        let Outcome::SubmitDraft { parts, .. } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a draft")
        };
        assert!(matches!(
            &parts[..],
            [DraftPart::Block(dim_agent_types::ContentBlock::Image { source })]
                if source.media_type == "image/png"
        ));
    }

    #[test]
    fn an_animated_image_says_its_first_frame_is_sent() {
        let image = crate::images::from_bytes(animated_gif()).unwrap();
        assert!(image.animated && image.scaled && !image.resized());
        let mut c = Composer::new(Vec::new());
        c.attach_image(image).unwrap();
        assert_eq!(
            c.display_text(),
            "[Image #1 · image/gif → image/jpeg · first frame]"
        );
    }

    /// A PNG within the edge limit whose base64 is over the byte limit is
    /// re-encoded as JPEG at the same size: the marker names both types.
    #[test]
    fn a_reencoded_image_shows_both_media_types_in_its_marker() {
        let image = crate::images::from_bytes(noise_png(1500, 1500)).unwrap();
        assert!(image.scaled && !image.resized());
        let mut c = Composer::new(Vec::new());
        c.attach_image(image).unwrap();
        assert_eq!(c.display_text(), "[Image #1 · image/png → image/jpeg]");
    }

    /// A JPEG within both limits whose EXIF orientation turns it upright is
    /// sent re-encoded at the same size and media type: the marker says so.
    #[test]
    fn an_exif_rotation_reencoded_at_the_same_size_says_reencoded() {
        for orientation in [3, 6] {
            let image = crate::images::from_bytes(oriented_jpeg(40, 20, orientation)).unwrap();
            assert!(
                image.scaled && !image.resized() && image.media_types().is_none(),
                "orientation {orientation}"
            );
            let mut c = Composer::new(Vec::new());
            c.attach_image(image).unwrap();
            assert_eq!(c.display_text(), "[Image #1 · re-encoded]");
        }
    }

    /// A JPEG within the edge limit whose base64 is over the byte limit fits
    /// once it is re-encoded at quality 85, again at the same size and type.
    #[test]
    fn a_jpeg_that_fits_only_after_the_reencode_says_reencoded() {
        use dim_agent_capabilities::images::{MAX_BASE64_BYTES, base64_len};
        let bytes = noise_jpeg(1500, 1500, 100);
        assert!(
            base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
            "{}",
            bytes.len()
        );
        let image = crate::images::from_bytes(bytes).unwrap();
        assert!(image.scaled && !image.resized() && image.media_types().is_none());
        let mut c = Composer::new(Vec::new());
        c.attach_image(image).unwrap();
        assert_eq!(c.display_text(), "[Image #1 · re-encoded]");
    }

    #[test]
    fn a_clipboard_image_shows_both_sizes_in_its_marker() {
        use dim_agent_capabilities::images::MAX_EDGE;
        let width = MAX_EDGE * 2;
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("wide.png");
        wide_png(&image, width);
        let image = crate::images::from_bytes(std::fs::read(&image).unwrap()).unwrap();
        let mut c = Composer::new(Vec::new());
        c.attach_image(image).unwrap();
        assert_eq!(
            c.display_text(),
            format!("[Image #1 · {width}×2 → {MAX_EDGE}×1]")
        );
        assert!(matches!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::SubmitDraft { parts, .. } if matches!(parts[..], [DraftPart::Block(_)])
        ));
    }

    /// A bitmap that fits is re-encoded but not resized: its marker stays the
    /// plain form rather than naming one size twice.
    #[test]
    fn a_small_clipboard_bitmap_keeps_its_plain_marker() {
        let image = crate::images::from_pixels(image::DynamicImage::new_rgb8(12, 8)).unwrap();
        assert!(image.scaled && !image.resized());
        let mut c = Composer::new(Vec::new());
        c.attach_image(image).unwrap();
        assert_eq!(c.display_text(), "[Image #1]");
    }

    #[test]
    fn an_image_above_the_pixel_guard_is_refused_and_leaves_the_draft_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("huge.gif");
        let mut gif = b"GIF89a".to_vec();
        gif.extend(12000u16.to_le_bytes());
        gif.extend(9000u16.to_le_bytes());
        gif.extend([0, 0, 0, 0]);
        gif.push(b';');
        std::fs::write(&image, gif).unwrap();
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "look");
        let error = attach(&mut c, std::slice::from_ref(&image)).unwrap_err();
        assert!(error.contains("12000×9000"), "{error}");
        assert_eq!(c.text(), "look");
    }

    /// `/branch` opens a line from text: a clipboard image keeps the draft
    /// and is named, rather than dropped on the way.
    #[test]
    fn an_inline_branch_keeps_a_draft_holding_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("shot.png");
        png(&image);
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "/branch look at ");
        let image = crate::images::from_bytes(std::fs::read(&image).unwrap()).unwrap();
        c.attach_image(image).unwrap();
        assert!(matches!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Rejected(reason) if reason.contains("[Image #1]")
        ));
        assert_eq!(c.display_text(), "/branch look at [Image #1]");
    }

    /// A path pasted first and a clipboard image attached second read the
    /// same in the draft as in the message they are sent as.
    #[test]
    fn a_draft_and_its_message_show_the_same_image_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        png(&path);
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "这是什么？");
        let entries =
            crate::attachments::pasted_entries(path.to_str().unwrap(), dir.path()).unwrap();
        attach(&mut c, &entries).unwrap();
        c.attach_image(crate::images::from_bytes(std::fs::read(&path).unwrap()).unwrap())
            .unwrap();
        let draft = c.display_text();
        assert_eq!(draft, "这是什么？ [Image #1] [Image #2]");
        let Outcome::SubmitDraft { parts, .. } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the two images to submit as blocks")
        };
        let row = transcript(&parts);
        assert_eq!(markers(&row), ["[Image #1]", "[Image #2]"]);
        assert_eq!(markers(&draft), markers(&row));
    }

    /// An image chosen with `@` is an image block too, so it reads the same
    /// as a clipboard image.
    #[test]
    fn a_mentioned_image_shows_the_image_marker() {
        let dir = tempfile::tempdir().unwrap();
        png(&dir.path().join("shot.png"));
        let mut c = Composer::new(Vec::new());
        c.set_root(dir.path().to_owned());
        typed(&mut c, "look at @sh");
        assert_eq!(picked(&mut c, "sh"), ["shot.png"]);
        c.handle_key(key(KeyCode::Enter));
        assert_eq!(c.display_text(), "look at [Image #1] ");
        let Outcome::SubmitDraft { parts, .. } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the mentioned image to submit as a block")
        };
        assert!(matches!(parts[1], DraftPart::Block(_)));
        assert_eq!(parts[0], DraftPart::Text("look at ".into()));
    }

    /// Removing an image renumbers the rest from 1, and adding another one
    /// cannot repeat a number inside a marker.
    #[test]
    fn images_renumber_when_one_is_removed() {
        let mut c = Composer::new(Vec::new());
        c.attach_image(bitmap()).unwrap();
        c.attach_image(bitmap()).unwrap();
        assert_eq!(c.display_text(), "[Image #1] [Image #2]");
        c.handle_key(key(KeyCode::Home));
        c.handle_key(key(KeyCode::Delete));
        assert_eq!(c.display_text(), " [Image #1]");
        c.handle_key(key(KeyCode::End));
        c.attach_image(bitmap()).unwrap();
        assert_eq!(c.display_text(), " [Image #1] [Image #2]");
        assert!(!c.display_text().contains("#2 #2"));
    }

    /// The leading token is the same in the draft and the transcript under
    /// an interface language that translates the details after it.
    #[test]
    fn the_image_marker_is_not_translated() {
        let previous = crate::ui::i18n::active();
        crate::ui::i18n::set(crate::ui::i18n::Locale::ZhCn);
        let mut c = Composer::new(Vec::new());
        c.attach_image(crate::images::from_bytes(animated_gif()).unwrap())
            .unwrap();
        let draft = c.display_text();
        assert!(draft.starts_with("[Image #1"), "{draft}");
        assert!(draft.contains("仅首帧"), "{draft}");
        let Outcome::SubmitDraft { parts, .. } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a draft")
        };
        let row = transcript(&parts);
        crate::ui::i18n::set(previous);
        assert_eq!(row, "[Image #1]");
    }

    /// A sent image leaves nothing of its marker in the input history: a
    /// draft recalled with Up sends its text and no literal marker.
    #[test]
    fn an_image_marker_never_enters_the_input_history() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "look");
        c.attach_image(bitmap()).unwrap();
        let Outcome::SubmitDraft { text, parts } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a draft")
        };
        assert!(matches!(
            &parts[..],
            [DraftPart::Text(_), DraftPart::Block(_)]
        ));
        let text = text.trim().to_owned();
        assert_eq!(text, "look");
        c.push_history(&text);
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "look");
        assert!(!c.display_text().contains("[Image"));
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit("look".into())
        );
    }

    /// An image-only send records no empty input-history entry, so the first
    /// Up after it recalls the entry before it.
    #[test]
    fn an_image_only_send_leaves_the_first_up_working() {
        let mut c = Composer::new(vec!["previous".into()]);
        c.attach_image(bitmap()).unwrap();
        let Outcome::SubmitDraft { text, .. } = c.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the image to submit as a draft")
        };
        assert_eq!(text, "");
        c.push_history(text.trim());
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "previous");
    }

    /// A shell command is text: Enter on a draft holding only an image runs
    /// no command and records none, and the draft stays.
    #[test]
    fn a_shell_draft_holding_only_an_image_does_nothing() {
        let mut c = Composer::new(vec!["!ls".into()]);
        typed(&mut c, "!");
        c.attach_image(bitmap()).unwrap();
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Handled,
            "an empty shell command is neither run nor recorded"
        );
        assert_eq!(c.shell(), Some(Shell::Recorded));
        assert_eq!(c.display_text(), "[Image #1]");
        c.handle_key(key(KeyCode::Home));
        c.handle_key(key(KeyCode::Up));
        assert_eq!(
            c.text(),
            "ls",
            "the first Up recalls the previous command, not an empty one"
        );
    }

    #[test]
    fn a_dropped_directory_is_one_token_sent_as_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("my docs");
        std::fs::create_dir(&docs).unwrap();
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "look at");
        attach(&mut c, std::slice::from_ref(&docs)).unwrap();
        assert_eq!(c.text(), "look at [my docs]");
        c.handle_key(key(KeyCode::Backspace));
        assert_eq!(c.text(), "look at ");
        attach(&mut c, std::slice::from_ref(&docs)).unwrap();
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit(format!("look at [my docs](<{}>)", docs.display()))
        );
    }

    fn picked(c: &mut Composer, query: &str) -> Vec<String> {
        let started = std::time::Instant::now();
        loop {
            let found: Vec<String> = c
                .mention_candidates(query)
                .into_iter()
                .map(|candidate| candidate.text)
                .collect();
            let searching = c.mentions.as_ref().is_some_and(|m| m.searching());
            if !searching || started.elapsed() > std::time::Duration::from_secs(10) {
                return found;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn at_mentions_pick_files_as_tokens_and_open_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        let mut c = Composer::new(Vec::new());
        c.set_root(dir.path().to_owned());
        typed(&mut c, "read @no");
        assert!(c.mention_open());
        assert_eq!(picked(&mut c, "no"), ["notes.txt"]);
        c.handle_key(key(KeyCode::Enter));
        assert_eq!(c.text(), "read @notes.txt ");
        assert!(!c.mention_open());
        typed(&mut c, "@sr");
        assert_eq!(picked(&mut c, "sr")[0], "src/");
        c.handle_key(key(KeyCode::Tab));
        assert_eq!(c.text(), "read @notes.txt @src/");
        assert_eq!(picked(&mut c, "src/"), ["src/main.rs"]);
        c.handle_key(key(KeyCode::Enter));
        assert_eq!(c.text(), "read @notes.txt @src/main.rs ");
        typed(&mut c, "@x");
        c.handle_key(key(KeyCode::Esc));
        assert!(!c.mention_open());
        assert_eq!(c.text(), "read @notes.txt @src/main.rs @x");
        let notes = dir.path().join("notes.txt");
        let main = dir.path().join("src").join("main.rs");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit(format!(
                "read [@notes.txt]({}) [@src/main.rs]({}) @x",
                notes.display(),
                main.display()
            ))
        );
        typed(&mut c, "!ls @s");
        assert!(!c.mention_open());
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
        assert_eq!(c.display_text(), "[... 5 lines] [... 5 lines #2]");
    }

    #[test]
    fn history_walks_back_and_returns_the_saved_draft() {
        let mut c = Composer::new(vec!["first".into(), "second".into()]);
        typed(&mut c, "draft");
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
    fn a_multi_line_draft_walks_history_from_its_start_and_comes_back() {
        let mut c = Composer::new(vec!["one\ntwo".into(), "last".into()]);
        typed(&mut c, "top");
        c.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT));
        typed(&mut c, "bottom");
        c.handle_key(key(KeyCode::Up));
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "top\nbottom");
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "last");
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "one\ntwo");
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "one\ntwo");
        c.handle_key(key(KeyCode::Down));
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "last");
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "top\nbottom");
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
    fn palette_requires_an_exact_exit_or_suspend_before_dispatch() {
        for query in ["/", "/e", "/ex", "/sus"] {
            let mut c = Composer::new(Vec::new());
            typed(&mut c, query);
            assert_eq!(c.handle_key(key(KeyCode::Enter)), Outcome::Handled);
            assert!(matches!(c.text(), "/exit" | "/suspend"));
        }
        for query in ["/exit", "/quit", "/suspend"] {
            let mut c = Composer::new(Vec::new());
            typed(&mut c, query);
            assert!(matches!(
                c.handle_key(key(KeyCode::Enter)),
                Outcome::Command { .. }
            ));
        }
    }

    #[test]
    fn a_command_that_declares_an_argument_fills_the_prompt_instead_of_running() {
        for code in [KeyCode::Enter, KeyCode::Tab] {
            let mut c = Composer::new(Vec::new());
            typed(&mut c, "/ren");
            assert_eq!(c.handle_key(key(code)), Outcome::Handled);
            assert_eq!(c.text(), "/rename ", "the palette waits for the title");
            assert!(!c.palette_open());
        }
    }

    #[test]
    fn hidden_exact_approval_is_dispatched_before_palette_prefix_matches() {
        let mut c = Composer::new(Vec::new());
        typed(&mut c, "/approval");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Command {
                name: "permissions".into(),
                args: String::new(),
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
        typed(&mut c, "/resume abc123");
        assert!(!c.palette_open(), "a space ends the command name");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Command {
                name: "resume".into(),
                args: "abc123".into()
            }
        );
        typed(&mut c, "/etc/hosts is odd");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Submit("/etc/hosts is odd".into())
        );
    }

    #[test]
    fn bangs_on_an_empty_draft_switch_to_shell_mode() {
        let mut c = Composer::new(vec!["!!git log".into(), "plain".into()]);
        typed(&mut c, "!!!");
        assert_eq!(c.shell(), Some(Shell::Private));
        assert_eq!(c.text(), "!", "a third bang is text");
        c.handle_key(key(KeyCode::Backspace));
        c.handle_key(key(KeyCode::Backspace));
        assert_eq!(c.shell(), Some(Shell::Recorded));
        typed(&mut c, "/ls -a");
        assert!(!c.palette_open(), "shell mode has no palette");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Shell {
                command: "/ls -a".into(),
                shell: Shell::Recorded
            }
        );
        assert_eq!(c.shell(), None, "a run leaves shell mode");

        typed(&mut c, "hi!");
        assert_eq!((c.shell(), c.text()), (None, "hi!"));
        c.clear();

        typed(&mut c, "!");
        c.handle_key(key(KeyCode::Up));
        assert_eq!((c.shell(), c.text()), (None, "plain"));
        c.handle_key(key(KeyCode::Up));
        assert_eq!((c.shell(), c.text()), (Some(Shell::Private), "git log"));
        c.handle_key(key(KeyCode::Down));
        c.handle_key(key(KeyCode::Down));
        assert_eq!((c.shell(), c.text()), (Some(Shell::Recorded), ""));
    }

    #[test]
    fn rows_scroll_after_seven_and_the_cursor_stays_visible() {
        let theme = Theme::system(ColorMode::TrueColor, Appearance::Dark, None);
        let mut c = Composer::new(Vec::new());
        for n in 0..10 {
            typed(&mut c, &format!("line {n}"));
            c.handle_key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT));
        }
        let (rows, cursor) = c.render_rows(40, MAX_INPUT_ROWS, &theme.palette);
        assert_eq!(rows.len(), MAX_INPUT_ROWS);
        assert!((cursor.1 as usize) < rows.len());
    }
}
