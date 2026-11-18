use crate::{
    Result,
    attachments::{INLINE_PASTE, MAX_IMAGES, MAX_PASTE, pasted_paths},
    client::{Client, RequestTooLarge, RpcError},
    clipboard::{self, Source},
    images,
    input::Input,
};
use dim_agent_types::{ContentBlock, ImageSourceType, UserInput};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{self, Write},
    ops::Range,
    path::{Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

enum Part {
    Text(String),
    Paste {
        text: String,
        lines: usize,
        path: Option<String>,
    },
    File {
        label: String,
        content: ContentBlock,
    },
}
impl Part {
    fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Paste { .. } | Self::File { .. } => 1,
        }
    }
}

/// One path a paste will attach, decided before its bytes are read.
struct PlannedFile {
    /// The token's name: the file name.
    label: String,
    path: PathBuf,
    /// Whether the path names an image, so its bytes must be read.
    image: bool,
}

#[derive(Default)]
pub struct Draft {
    parts: Vec<Part>,
    cursor: usize,
}

impl Draft {
    pub fn clear(&mut self) {
        self.parts.clear();
        self.cursor = 0;
    }

    pub fn plain(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [Part::Text(text)] if !text.contains(['\r', '\n']) => Some(text),
            _ => None,
        }
    }

    pub fn command(&self) -> Option<&str> {
        let Part::Text(text) = self.parts.first()? else {
            return None;
        };
        match text.split_whitespace().next()? {
            command @ ("/exit" | "/export" | "/mcp" | "/effort" | "/reasoning" | "/fast"
            | "/agent") => Some(command),
            _ if text.starts_with('!') => Some("!"),
            _ => None,
        }
    }

    fn empty(&self) -> bool {
        self.parts.iter().all(|part| match part {
            Part::Text(text) => text.trim().is_empty(),
            Part::Paste { .. } | Part::File { .. } => false,
        })
    }

    fn locate(&self, position: usize) -> (usize, usize) {
        let mut start = 0;
        for (index, part) in self.parts.iter().enumerate() {
            if position <= start + part.len() {
                return (index, position - start);
            }
            start += part.len();
        }
        (self.parts.len(), 0)
    }

    fn insert(&mut self, part: Part) {
        let inserted = part.len();
        let (index, offset) = self.locate(self.cursor);
        match self.parts.get_mut(index) {
            Some(Part::Text(text)) => {
                let tail = text.split_off(offset);
                self.parts
                    .splice(index + 1..index + 1, [part, Part::Text(tail)]);
            }
            Some(Part::Paste { .. } | Part::File { .. }) => self.parts.insert(index + offset, part),
            None => self.parts.push(part),
        }
        self.cursor += inserted;
        self.join_text();
        self.snap_cursor();
    }

    fn snap_cursor(&mut self) {
        let (index, offset) = self.locate(self.cursor);
        if let Some(Part::Text(text)) = self.parts.get(index)
            && offset > 0
            && offset < text.len()
        {
            let next = text
                .grapheme_indices(true)
                .map(|(pos, _)| pos)
                .find(|&pos| pos >= offset)
                .unwrap_or(text.len());
            self.cursor += next - offset;
        }
    }

    fn join_text(&mut self) {
        self.parts
            .retain(|part| !matches!(part, Part::Text(text) if text.is_empty()));
        let mut index = 1;
        while index < self.parts.len() {
            if matches!(
                (&self.parts[index - 1], &self.parts[index]),
                (Part::Text(_), Part::Text(_))
            ) {
                let Part::Text(tail) = self.parts.remove(index) else {
                    unreachable!()
                };
                let Part::Text(text) = &mut self.parts[index - 1] else {
                    unreachable!()
                };
                text.push_str(&tail);
            } else {
                index += 1;
            }
        }
    }

    fn boundaries(&self) -> Vec<usize> {
        let mut positions = vec![0];
        let mut start = 0;
        for part in &self.parts {
            match part {
                Part::Text(text) => positions.extend(
                    text.grapheme_indices(true)
                        .map(|(offset, grapheme)| start + offset + grapheme.len()),
                ),
                Part::Paste { .. } | Part::File { .. } => positions.push(start + 1),
            }
            start += part.len();
        }
        positions
    }

    fn erase(&mut self, start: usize, end: usize) {
        let mut base = 0;
        for part in &mut self.parts {
            let length = part.len();
            match part {
                Part::Text(text) => {
                    let from = start.saturating_sub(base).min(length);
                    let to = end.saturating_sub(base).min(length);
                    text.replace_range(from..to, "");
                }
                Part::Paste { .. } | Part::File { .. } if start <= base && end > base => {
                    *part = Part::Text(String::new());
                }
                Part::Paste { .. } | Part::File { .. } => {}
            }
            base += length;
        }
        self.cursor = start;
        self.join_text();
        self.snap_cursor();
    }

    fn horizontal(&mut self, forward: bool, erase: bool) {
        let positions = self.boundaries();
        let next = if forward {
            positions.into_iter().find(|&pos| pos > self.cursor)
        } else {
            positions.into_iter().rfind(|&pos| pos < self.cursor)
        };
        if let Some(next) = next {
            if erase {
                self.erase(self.cursor.min(next), self.cursor.max(next));
            } else {
                self.cursor = next;
            }
        }
    }

    fn layout(&self, width: usize) -> (Vec<String>, Vec<(usize, usize, usize)>) {
        let columns = width.saturating_sub(1).max(3);
        let mut lines = vec!["> ".to_owned()];
        let mut positions = vec![(0, 0, 2)];
        let (mut offset, mut column, mut pastes, mut images) = (0, 2, 0, 0);
        for part in &self.parts {
            let pieces: Vec<(String, usize)> = match part {
                Part::Text(text) => text
                    .graphemes(true)
                    .map(|g| (g.to_owned(), g.len()))
                    .collect(),
                Part::Paste { lines, .. } => {
                    pastes += 1;
                    vec![(format!("[Paste {pastes}: {lines} lines]"), 1)]
                }
                Part::File { label, content } => {
                    let text = match content {
                        ContentBlock::Image { .. } => {
                            images += 1;
                            format!("[Image {images}: {label}]")
                        }
                        _ => format!("[File: {label}]"),
                    };
                    vec![(text, 1)]
                }
            };
            for (piece, length) in pieces {
                if matches!(piece.as_str(), "\n" | "\r\n") {
                    lines.push("  ".into());
                    column = 2;
                } else {
                    let mut shown = escaped(&piece);
                    if !matches!(part, Part::Text(_)) {
                        shown = truncate(&shown, columns.saturating_sub(2));
                    }
                    let cells = shown.width();
                    if column + cells > columns && column > 2 {
                        lines.push("  ".into());
                        column = 2;
                        *positions.last_mut().expect("cursor boundary") =
                            (offset, lines.len() - 1, column);
                    }
                    lines.last_mut().expect("input row").push_str(&shown);
                    column += cells;
                }
                offset += length;
                positions.push((offset, lines.len() - 1, column));
            }
        }
        (lines, positions)
    }

    fn vertical(&mut self, width: usize, direction: i32) {
        let (_, positions) = self.layout(width);
        let Some(&(_, row, column)) = positions.iter().rfind(|&&(pos, _, _)| pos == self.cursor)
        else {
            return;
        };
        let target = row as i32 + direction;
        if target >= 0
            && let Some(&(position, _, _)) = positions
                .iter()
                .filter(|&&(_, row, _)| row as i32 == target)
                .min_by_key(|&&(_, _, col)| col.abs_diff(column))
        {
            self.cursor = position;
        }
    }

    fn line_edge(&mut self, end: bool) {
        let (mut start, mut base) = (0, 0);
        for part in &self.parts {
            if let Part::Text(text) = part {
                for (offset, grapheme) in text.grapheme_indices(true) {
                    if matches!(grapheme, "\n" | "\r\n") {
                        if base + offset >= self.cursor {
                            self.cursor = if end { base + offset } else { start };
                            return;
                        }
                        start = base + offset + grapheme.len();
                    }
                }
            }
            base += part.len();
        }
        self.cursor = if end { base } else { start };
    }

    fn image_parts(&self) -> usize {
        self.parts
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    Part::File {
                        content: ContentBlock::Image { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Decides what a group of pasted paths becomes without reading image
    /// bytes: token labels and the 20-image count.
    fn plan_files(&self, paths: &[PathBuf]) -> Result<Vec<PlannedFile>> {
        let mut count = self.image_parts();
        let mut plan = Vec::with_capacity(paths.len());
        for path in paths {
            let label = path
                .file_name()
                .ok_or("Expected a local ordinary file")?
                .to_str()
                .ok_or("File paths must be UTF-8")?
                .to_owned();
            path.to_str().ok_or("File paths must be UTF-8")?;
            let image = images::is_image(path);
            if image {
                count += 1;
                if count > MAX_IMAGES {
                    return Err("A submission accepts at most 20 images".into());
                }
            }
            plan.push(PlannedFile {
                label,
                path: path.clone(),
                image,
            });
        }
        Ok(plan)
    }

    /// Reads a planned group: an image path is decoded and scaled here, so a
    /// caller with a runtime runs this on a blocking task.
    fn read_planned(plan: Vec<PlannedFile>) -> Result<Vec<Part>> {
        let mut parts = Vec::with_capacity(plan.len());
        for planned in plan {
            let content = images::file(&planned.path)
                .map_err(|error| format!("{}: {error}", truncate(&escaped(&planned.label), 24)))?;
            let part = match content {
                images::Item::Image(image) => Part::File {
                    label: image_label(&planned.label, &image),
                    content: image.block(),
                },
                images::Item::Text(text) => Part::File {
                    label: planned.label,
                    content: ContentBlock::Text { text },
                },
            };
            parts.push(part);
        }
        Ok(parts)
    }

    fn insert_files(&mut self, parts: Vec<Part>, replace: Range<usize>) {
        self.erase(replace.start, replace.end);
        for part in parts {
            self.insert(part);
        }
    }

    /// Reads and inserts a pasted group; the image work runs on a blocking
    /// task so a decode never holds the runtime.
    async fn files(&mut self, paths: &[PathBuf], replace: Range<usize>) -> Result<()> {
        let plan = self.plan_files(paths)?;
        let parts = if plan.iter().any(|planned| planned.image) {
            tokio::task::spawn_blocking(move || Self::read_planned(plan))
                .await
                .map_err(|error| error.to_string())??
        } else {
            Self::read_planned(plan)?
        };
        self.insert_files(parts, replace);
        Ok(())
    }

    async fn paste(&mut self, text: String, base: &Path) -> Result<()> {
        if let Some(paths) = pasted_paths(&text, base) {
            self.files(&paths, self.cursor..self.cursor).await
        } else {
            if text.len() > INLINE_PASTE || text.contains(['\r', '\n']) {
                let lines = text.lines().count();
                self.insert(Part::Paste {
                    text,
                    lines,
                    path: None,
                });
            } else if !text.is_empty() {
                self.insert(Part::Text(text));
            }
            Ok(())
        }
    }

    async fn clipboard(&mut self, source: Source, base: &Path) -> Result<()> {
        match source {
            Source::Text(text) => self.paste(text, base).await,
            Source::Files(paths) => self.files(&paths, self.cursor..self.cursor).await,
            Source::Image(image) => {
                if self.image_parts() >= MAX_IMAGES {
                    return Err("A submission accepts at most 20 images".into());
                }
                let label = image_label("clipboard", &image);
                self.insert(Part::File {
                    label,
                    content: image.block(),
                });
                Ok(())
            }
        }
    }

    fn completion(&self, base: &Path) -> Result<Option<Completion>> {
        let (index, offset) = self.locate(self.cursor);
        let Some(Part::Text(text)) = self.parts.get(index) else {
            return Ok(None);
        };
        let Some(start) = text[..offset]
            .rfind('@')
            .filter(|&pos| pos == 0 || text[..pos].ends_with(char::is_whitespace))
        else {
            return Ok(None);
        };
        let query = &text[start + 1..offset];
        if query.contains(['\n', '\r']) {
            return Ok(None);
        }
        let boundary = query
            .rfind(std::path::is_separator)
            .map_or(0, |position| position + 1);
        let directory = base.join(&query[..boundary]);
        let prefix = &query[boundary..];
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with(prefix) {
                let path = entry.path();
                let directory = path.is_dir();
                entries.push(CompletionEntry {
                    path,
                    name,
                    directory,
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(Completion {
            range: self.cursor - (offset - start)..self.cursor,
            entries,
            selected: 0,
        }))
    }

    pub async fn prepare(
        &mut self,
        client: &mut Client,
        session: &str,
        #[cfg(unix)] interrupts: &mut tokio::signal::unix::Signal,
        #[cfg(windows)] interrupts: &mut tokio::signal::windows::CtrlC,
        notice: &mut Option<String>,
    ) -> Result<Option<UserInput>> {
        let mut pastes = 0;
        for part in &mut self.parts {
            let Part::Paste { text, path, .. } = part else {
                continue;
            };
            pastes += 1;
            if text.len() <= INLINE_PASTE || path.is_some() {
                continue;
            }
            tokio::select! {
                biased;
                _ = interrupts.recv() => {
                    *notice = Some("Preparation cancelled; draft retained".into());
                    return Ok(None);
                },
                _ = std::future::ready(()) => {}
            }
            println!("Saving text attachment...");
            let mut cancelled = false;
            let stored: Result<String> = async {
                let id = client
                    .send("attachment.put", json!({"sessionId":session,"text":text}))
                    .await?;
                let response = client.response(id);
                tokio::pin!(response);
                let reply = loop {
                    tokio::select! {
                        biased;
                        _ = interrupts.recv() => {
                            if cancelled {
                                println!("Still waiting for the text attachment to settle...");
                            } else {
                                cancelled = true;
                                println!("Waiting for the text attachment to settle...");
                            }
                        }
                        result = &mut response => break result?,
                    }
                };
                #[derive(Deserialize)]
                struct Stored {
                    path: String,
                }
                Ok(serde_json::from_value::<Stored>(reply)
                    .map_err(|_| "Attachment outcome could not be confirmed; resume the Session instead of retrying automatically")?.path)
            }.await;
            match stored {
                Ok(stored) => *path = Some(stored),
                Err(error) if error.is::<RpcError>() || error.is::<RequestTooLarge>() => {
                    *notice = Some(format!("Paste {pastes}: {error}; draft retained"));
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            if cancelled {
                *notice = Some("Preparation cancelled; draft retained".into());
                return Ok(None);
            }
        }
        for part in &mut self.parts {
            let Part::File { content, .. } = part else {
                continue;
            };
            if !matches!(
                content,
                ContentBlock::Image { source } if source.source_type == ImageSourceType::Base64
            ) {
                continue;
            }
            let import = crate::images::import(client, session, std::slice::from_mut(content));
            tokio::pin!(import);
            let mut cancelled = false;
            let result = loop {
                tokio::select! {
                    _ = interrupts.recv() => { cancelled = true; }
                    result = &mut import => break result,
                }
            };
            if let Err(error) = result {
                *notice = Some(format!("Image import failed: {error}; draft retained"));
                return Ok(None);
            }
            if cancelled {
                *notice = Some("Preparation cancelled; draft retained".into());
                return Ok(None);
            }
        }
        let mut content = Vec::new();
        let mut text = String::new();
        for part in &self.parts {
            match part {
                Part::Text(value) => text.push_str(value),
                Part::Paste {
                    text: value,
                    lines,
                    path,
                } => match path {
                    Some(path) => text.push_str(&format!("[Pasted text: {path} ({lines} lines)]")),
                    None => text.push_str(value),
                },
                Part::File { content: value, .. } => {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text {
                            text: std::mem::take(&mut text),
                        });
                    }
                    content.push(value.clone());
                }
            }
        }
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        tokio::select! {
            biased;
            _ = interrupts.recv() => {
                *notice = Some("Preparation cancelled; draft retained".into());
                return Ok(None);
            },
            _ = std::future::ready(()) => {}
        }
        Ok(Some(UserInput { content }))
    }
}

struct Completion {
    range: Range<usize>,
    entries: Vec<CompletionEntry>,
    selected: usize,
}

struct CompletionEntry {
    path: PathBuf,
    name: String,
    directory: bool,
}

enum Key {
    Text(String),
    Paste(String),
    Rejected(&'static str),
    Enter,
    Newline,
    Interrupt,
    Eof,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Delete,
    Tab,
    Escape,
    Clipboard,
}

#[derive(Default)]
struct Paste {
    bytes: Vec<u8>,
    rejected: Option<&'static str>,
}
impl Paste {
    fn append(&mut self, bytes: &[u8]) {
        if self.bytes.len() + bytes.len() > MAX_PASTE {
            self.rejected = Some("Paste exceeds 10 MiB; draft retained");
        } else if self.rejected.is_none() {
            self.bytes.extend_from_slice(bytes);
        }
    }
}

#[derive(Default)]
struct Decoder {
    bytes: Vec<u8>,
    paste: Option<Paste>,
}
impl Decoder {
    fn next(&mut self) -> Option<Key> {
        loop {
            if let Some(paste) = &mut self.paste {
                let end = self
                    .bytes
                    .windows(PASTE_END.len())
                    .position(|s| s == PASTE_END);
                if let Some(cancel) = self.bytes.windows(2).position(|s| s == b"\x03\x03")
                    && end.is_none_or(|end| cancel < end)
                {
                    self.bytes.clear();
                    self.paste = None;
                    return Some(Key::Rejected("Paste cancelled; draft retained"));
                }
                if let Some(end) = end {
                    paste.append(&self.bytes[..end]);
                    self.bytes.drain(..end + PASTE_END.len());
                    let paste = self.paste.take().expect("paste end");
                    return Some(if let Some(reason) = paste.rejected {
                        Key::Rejected(reason)
                    } else {
                        match String::from_utf8(paste.bytes) {
                            Ok(text) => Key::Paste(text),
                            Err(_) => Key::Rejected("Paste must be UTF-8; draft retained"),
                        }
                    });
                }
                let safe = self.bytes.len().saturating_sub(PASTE_END.len() - 1);
                paste.append(&self.bytes[..safe]);
                self.bytes.drain(..safe);
                return None;
            }
            let &first = self.bytes.first()?;
            let key = match first {
                3 => Key::Interrupt,
                4 => Key::Eof,
                #[cfg(windows)]
                26 => Key::Eof,
                8 | 127 => Key::Backspace,
                9 => Key::Tab,
                10 => Key::Newline,
                13 => Key::Enter,
                22 => Key::Clipboard,
                27 => {
                    if self.bytes.starts_with(PASTE_START) {
                        self.bytes.drain(..PASTE_START.len());
                        self.paste = Some(Paste::default());
                        continue;
                    }
                    if PASTE_START.starts_with(&self.bytes) || self.bytes.len() == 1 {
                        return None;
                    }
                    if !matches!(self.bytes[1], b'[' | b'O') {
                        self.bytes.remove(0);
                        return Some(Key::Escape);
                    }
                    let end = self.bytes[2..]
                        .iter()
                        .position(|b| (0x40..=0x7e).contains(b))?
                        + 2;
                    let key = match &self.bytes[..=end] {
                        b"\x1b[A" | b"\x1bOA" => Some(Key::Up),
                        b"\x1b[B" | b"\x1bOB" => Some(Key::Down),
                        b"\x1b[C" | b"\x1bOC" => Some(Key::Right),
                        b"\x1b[D" | b"\x1bOD" => Some(Key::Left),
                        b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" | b"\x1b[7~" => Some(Key::Home),
                        b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Some(Key::End),
                        b"\x1b[3~" => Some(Key::Delete),
                        b"\x1b[13;2u" | b"\x1b[27;2;13~" => Some(Key::Newline),
                        _ => None,
                    };
                    self.bytes.drain(..=end);
                    if let Some(key) = key {
                        return Some(key);
                    }
                    continue;
                }
                0..=31 => {
                    self.bytes.remove(0);
                    continue;
                }
                _ => {
                    let length = match first {
                        0..=0x7f => 1,
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => 1,
                    };
                    let length = length.min(self.bytes.len());
                    let value = std::str::from_utf8(&self.bytes[..length]);
                    let key = match value {
                        Ok(text) => Key::Text(text.into()),
                        Err(error) => {
                            let length = error.error_len()?;
                            self.bytes.drain(..length);
                            return Some(Key::Rejected(
                                "Terminal input must be UTF-8; draft retained",
                            ));
                        }
                    };
                    self.bytes.drain(..length);
                    return Some(key);
                }
            };
            self.bytes.remove(0);
            return Some(key);
        }
    }
}

/// An image item's marker label: the name, both sizes when the sent size
/// differs from the original, both media types when the encoding changed the
/// type, the first-frame note of an animation, and a re-encode that kept both
/// the size and the type.
fn image_label(label: &str, image: &images::Attached) -> String {
    let mut label = label.to_owned();
    if image.resized() {
        label.push_str(" · ");
        label.push_str(&image.sizes());
    }
    if let Some(types) = image.media_types() {
        label.push_str(" · ");
        label.push_str(&types);
    }
    if image.animated {
        label.push_str(" · first frame");
    }
    if image.reencoded() {
        label.push_str(" · re-encoded");
    }
    label
}

fn truncate(text: &str, columns: usize) -> String {
    if text.width() <= columns {
        return text.into();
    }
    let mut shown = String::new();
    for grapheme in text.graphemes(true) {
        if shown.width() + grapheme.width() + 1 > columns {
            break;
        }
        shown.push_str(grapheme);
    }
    if columns > 0 {
        shown.push('…');
    }
    shown
}

fn escaped(text: &str) -> String {
    let mut shown = String::new();
    for ch in text.chars() {
        if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            shown.extend(ch.escape_debug());
        } else {
            shown.push(ch);
        }
    }
    shown
}

#[derive(Default)]
struct Screen {
    widths: Vec<usize>,
    cursor: (usize, usize),
    columns: usize,
}
impl Screen {
    fn physical(&self, columns: usize) -> (usize, usize) {
        let columns = columns.max(1);
        let mut rows = 0;
        let mut cursor = 0;
        for (index, &width) in self.widths.iter().enumerate() {
            let cells = if index == self.cursor.0 {
                cursor = rows + self.cursor.1 / columns;
                width.max(self.cursor.1 + 1)
            } else {
                width
            };
            rows += cells.div_ceil(columns).max(1);
        }
        (rows, cursor)
    }

    fn draw(
        &mut self,
        draft: &Draft,
        size: (usize, usize),
        notice: Option<&str>,
        completion: Option<&Completion>,
    ) -> Result<()> {
        let (mut lines, positions) = draft.layout(size.0);
        let &(_, row, column) = positions
            .iter()
            .rfind(|&&(pos, _, _)| pos == draft.cursor)
            .expect("draft cursor");
        let suggestions = completion
            .map(|value| {
                value
                    .entries
                    .len()
                    .clamp(1, 5)
                    .min(size.1.saturating_sub(2))
            })
            .unwrap_or(0);
        let available = size.1.saturating_sub(suggestions + 1).max(1);
        let top = row.saturating_sub(available - 1);
        let end = (top + available).min(lines.len());
        lines = lines[top..end].to_vec();
        if let Some(value) = completion {
            if value.entries.is_empty() && suggestions > 0 {
                lines.push("  No matching files".into());
            }
            let start = value.selected.saturating_sub(suggestions.saturating_sub(1));
            for (index, entry) in value
                .entries
                .iter()
                .enumerate()
                .skip(start)
                .take(suggestions)
            {
                lines.push(truncate(
                    &escaped(&format!(
                        "{} {}{}",
                        if index == value.selected { "*" } else { " " },
                        entry.name,
                        if entry.directory { "/" } else { "" }
                    )),
                    size.0.saturating_sub(1),
                ));
            }
        }
        lines.push(truncate(
            &escaped(notice.unwrap_or(if completion.is_some() {
                "Tab: select or enter directory   Esc: close suggestions"
            } else {
                "Enter: send  Ctrl+J: newline  Ctrl+V: clipboard  Ctrl+C: clear  Ctrl+C twice: cancel paste"
            })),
            size.0.saturating_sub(1),
        ));
        let mut output = io::stdout().lock();
        let (previous_rows, previous_cursor) = self.physical(size.0);
        if previous_rows == 0 {
            writeln!(output)?;
        } else {
            write!(output, "\r")?;
            if previous_cursor > 0 {
                write!(output, "\x1b[{}A", previous_cursor)?;
            }
            for index in 0..previous_rows {
                write!(output, "\x1b[2K")?;
                if index + 1 < previous_rows {
                    write!(output, "\x1b[1B\r")?;
                }
            }
            if previous_rows > 1 {
                write!(output, "\x1b[{}A", previous_rows - 1)?;
            }
            write!(output, "\r")?;
        }
        for (index, text) in lines.iter().enumerate() {
            if index > 0 {
                write!(output, "\r\n")?;
            }
            write!(output, "{text}")?;
        }
        self.widths = lines.iter().map(|line| line.width()).collect();
        self.cursor = (row - top, column);
        self.columns = size.0;
        write!(output, "\r\x1b[{}A", lines.len() - self.cursor.0 - 1)?;
        if column > 0 {
            write!(output, "\x1b[{column}C")?;
        }
        output.flush()?;
        Ok(())
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let (rows, cursor) = self.physical(self.columns);
        if rows > 0 {
            let mut output = io::stdout().lock();
            let _ = write!(output, "\r\x1b[{}B\x1b[2K\n", rows - cursor - 1);
            let _ = output.flush();
        }
    }
}

pub async fn edit(
    input: &mut Input,
    draft: &mut Draft,
    mut notice: Option<String>,
    base: &Path,
    #[cfg(unix)] interrupts: &mut tokio::signal::unix::Signal,
    #[cfg(windows)] interrupts: &mut tokio::signal::windows::CtrlC,
) -> Result<bool> {
    let _mode = input.editor()?;
    let mut screen = Screen::default();
    let mut decoder = Decoder::default();
    let mut completion = None;
    let mut dismissed = false;
    let mut clipboard_read: Option<
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<Source>>>>,
    > = None;
    #[cfg(unix)]
    let mut resize = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut size = input.size()?;
    screen.draw(draft, size, notice.as_deref(), None)?;
    loop {
        let bytes = tokio::select! {
            _ = interrupts.recv() => {
                if draft.parts.is_empty() && decoder.paste.is_none() {
                    return Ok(false);
                }
                draft.clear();
                clipboard_read = None;
                decoder = Decoder::default();
                notice = None;
                completion = None;
                dismissed = false;
                screen.draw(draft, size, None, None)?;
                continue;
            },
            source = async { clipboard_read.as_mut().expect("pending clipboard read").as_mut().await }, if clipboard_read.is_some() => {
                clipboard_read = None;
                notice = match source {
                    Ok(source) => draft
                        .clipboard(source, base)
                        .await
                        .err()
                        .map(|error| format!("Clipboard: {error}")),
                    Err(error) => Some(format!("Clipboard: {error}")),
                };
                completion = None;
                screen.draw(draft, size, notice.as_deref(), None)?;
                continue;
            },
            _ = async {
                #[cfg(unix)]
                { resize.recv().await; }
                #[cfg(windows)]
                { std::future::pending::<()>().await; }
            } => {
                size = input.size()?;
                screen.draw(draft, size, notice.as_deref(), completion.as_ref())?;
                continue;
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)), if completion.is_some() && decoder.bytes == b"\x1b" && decoder.paste.is_none() => {
                completion = None;
                dismissed = true;
                screen.draw(draft, size, notice.as_deref(), None)?;
                continue;
            },
            bytes = input.read_bytes() => bytes?,
        };
        let Some(bytes) = bytes else { return Ok(false) };
        let mut changed = bytes.is_empty();
        decoder.bytes.extend(bytes);
        while let Some(key) = decoder.next() {
            changed = true;
            notice = None;
            match (&key, &mut completion) {
                (Key::Up | Key::Down, Some(value)) => {
                    if !value.entries.is_empty() {
                        value.selected = if matches!(key, Key::Up) {
                            value.selected.saturating_sub(1)
                        } else {
                            (value.selected + 1).min(value.entries.len() - 1)
                        };
                    }
                    continue;
                }
                (Key::Tab, Some(value)) => {
                    if let Some(entry) = value.entries.get(value.selected) {
                        let result = if entry.directory {
                            entry
                                .path
                                .trim_prefix(base)
                                .to_str()
                                .ok_or_else(|| "File paths must be UTF-8".into())
                                .map(|path| {
                                    draft.erase(value.range.start, value.range.end);
                                    draft.insert(Part::Text(format!(
                                        "@{path}{}",
                                        std::path::MAIN_SEPARATOR
                                    )));
                                })
                        } else {
                            draft
                                .files(std::slice::from_ref(&entry.path), value.range.clone())
                                .await
                        };
                        if let Err(error) = result {
                            notice = Some(error.to_string());
                        }
                    }
                }
                (Key::Escape, _) => {
                    completion = None;
                    dismissed = true;
                    continue;
                }
                _ => {}
            }
            if !matches!(key, Key::Up | Key::Down | Key::Tab) {
                dismissed = false;
            }
            match key {
                Key::Enter if clipboard_read.is_some() => {
                    notice = Some("Wait for clipboard insertion before sending".into());
                }
                Key::Enter if !draft.empty() => {
                    screen.draw(draft, size, None, None)?;
                    return Ok(true);
                }
                Key::Enter => {}
                Key::Eof if draft.parts.is_empty() => return Ok(false),
                Key::Eof => notice = Some("Draft retained; clear it before exiting".into()),
                Key::Interrupt if draft.parts.is_empty() => return Ok(false),
                Key::Interrupt => {
                    draft.clear();
                    clipboard_read = None;
                }
                Key::Text(text) => draft.insert(Part::Text(text)),
                Key::Newline => draft.insert(Part::Text("\n".into())),
                Key::Paste(text) => {
                    if let Err(error) = draft.paste(text, base).await {
                        notice = Some(error.to_string());
                    }
                }
                Key::Clipboard => {
                    clipboard_read = Some(Box::pin(clipboard::read()));
                    notice = Some("Reading clipboard... Ctrl+C: clear".into());
                }
                Key::Rejected(reason) => notice = Some(reason.into()),
                Key::Left => draft.horizontal(false, false),
                Key::Right => draft.horizontal(true, false),
                Key::Backspace => draft.horizontal(false, true),
                Key::Delete => draft.horizontal(true, true),
                Key::Up => draft.vertical(size.0, -1),
                Key::Down => draft.vertical(size.0, 1),
                Key::Home => draft.line_edge(false),
                Key::End => draft.line_edge(true),
                Key::Tab | Key::Escape => {}
            }
            completion = if dismissed {
                None
            } else {
                match draft.completion(base) {
                    Ok(value) => value,
                    Err(error) => {
                        notice = Some(error.to_string());
                        None
                    }
                }
            };
        }
        if changed {
            size = input.size()?;
            screen.draw(draft, size, notice.as_deref(), completion.as_ref())?;
        }
    }
}
