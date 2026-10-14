//! The write path of the scrollback interface (design §6.2, §6.3).
//!
//! History is committed once into the terminal's native scrollback; only the
//! live region at the bottom of the screen is ever repainted. `Screen` turns a
//! frame description into one byte string: commits through the vendored
//! `emit_to_scrollback` (wrap-aware, soft/hard breaks), a row diff of the live
//! region, and cursor movement/visibility only when they changed. A frame with
//! nothing new produces no bytes at all, which keeps the hardware cursor
//! blinking.

use std::io::{self, Write};

use ratatui::layout::{Rect, Size};
use xai_ratatui_inline::{TerminalLike, emit_to_scrollback};

/// History rows kept visible when the live region grows (design §6.1).
pub const MIN_HISTORY_ROWS: u16 = 4;

const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";
const CURSOR_SHOW: &[u8] = b"\x1b[?25h";
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";
const ERASE_LINE: &[u8] = b"\x1b[2K";
const ERASE_DOWN: &[u8] = b"\x1b[J";

/// What one frame wants on screen.
#[derive(Debug, Default, Clone)]
pub struct Frame {
    /// Text to commit into scrollback before painting. Each entry is a whole
    /// block; `\n` separates logical lines.
    pub commits: Vec<String>,
    /// The live region, one string per row, already wrapped to the width.
    /// Styling escapes are allowed; control characters are not.
    pub live: Vec<String>,
    /// Cursor position inside the live region (row, column), `None` hides it.
    pub cursor: Option<(u16, u16)>,
}

pub struct Screen {
    size: (u16, u16),
    viewport_height: u16,
    /// First screen row of the live region. It follows the end of the
    /// committed content down the screen and is pinned to the bottom once
    /// the content reaches it; shrinking the region leaves rows *below* it.
    top: u16,
    /// Cursor row of the shell when the interface started; `None` when the
    /// terminal did not answer, which pins the region to the bottom.
    origin: Option<u16>,
    /// First screen row holding rows this interface wrote (0 once they have
    /// reached the top).
    content_start: u16,
    /// Some of those rows already left through the top of the screen.
    scrolled: bool,
    /// The screen row the cursor was parked on after the last frame; a
    /// resize report is compared against it to find where the region went.
    anchor: u16,
    /// Rows as last painted; `None` entries are unknown (repaint).
    painted: Vec<Option<String>>,
    cursor_visible: bool,
    /// Where the terminal's cursor is after our last write, when known.
    cursor_at: Option<(u16, u16)>,
    synchronized: bool,
}

impl Screen {
    /// `synchronized`: emit DEC 2026 around frames (false inside tmux).
    pub fn new(size: (u16, u16), synchronized: bool) -> Self {
        Self {
            size,
            viewport_height: 0,
            top: size.1,
            origin: None,
            content_start: 0,
            scrolled: false,
            anchor: 0,
            painted: Vec::new(),
            cursor_visible: true,
            cursor_at: None,
            synchronized,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Where the shell left the cursor: the region starts there instead of
    /// at the bottom of the screen.
    pub fn set_origin(&mut self, row: u16) {
        self.origin = Some(row);
    }

    pub fn viewport_height(&self) -> u16 {
        self.viewport_height
    }

    /// How many rows the region takes for `live` rows at `size`.
    pub fn wanted_for(&self, size: (u16, u16), live: usize) -> u16 {
        let cap = size.1.saturating_sub(MIN_HISTORY_ROWS).max(1);
        live.clamp(1, cap as usize) as u16
    }

    fn cap(&self) -> u16 {
        self.size.1.saturating_sub(MIN_HISTORY_ROWS).max(1)
    }

    fn viewport_top(&self) -> u16 {
        self.top
    }

    /// Where the region starts once it is `wanted` rows tall.
    fn top_for(&self, wanted: u16) -> u16 {
        self.top.min(self.size.1.saturating_sub(wanted))
    }

    /// Renders `frame` for a terminal of `size`; returns the bytes to write
    /// (empty when nothing changed).
    pub fn render(&mut self, frame: &Frame, size: (u16, u16)) -> Vec<u8> {
        let mut out = Vec::new();
        let resized = size != self.size;
        let cap = if resized {
            size.1.saturating_sub(MIN_HISTORY_ROWS).max(1)
        } else {
            self.cap()
        };
        let wanted = frame.live.len().clamp(1, cap as usize) as u16;
        let rows_changed = self.painted_differs(frame, wanted);
        let nothing = frame.commits.is_empty()
            && !resized
            && !rows_changed
            && wanted == self.viewport_height
            && !self.cursor_needs_move(frame, wanted)
            && self.cursor_visible == frame.cursor.is_some();
        if nothing {
            return out;
        }

        // The first frame lays the viewport out; commits need it to exist.
        if self.viewport_height == 0 {
            self.size = size;
            self.viewport_height = wanted;
            self.painted = vec![None; wanted as usize];
            self.cursor_at = None;
            self.begin(&mut out);
            self.hide_for_paint(&mut out);
            let bottom = size.1.saturating_sub(1);
            match self.origin {
                Some(origin) => {
                    // Start where the shell stopped; scroll only as much as
                    // the region needs below that row.
                    let origin = origin.min(bottom);
                    let top = origin.min(size.1.saturating_sub(wanted));
                    let overflow = (origin + wanted).saturating_sub(size.1);
                    if overflow > 0 {
                        push_move(&mut out, 0, bottom);
                        out.extend(std::iter::repeat_n(b"\n", overflow as usize).flatten());
                    }
                    self.top = top;
                }
                None => {
                    // Make room at the bottom: the live region owns these rows.
                    self.top = size.1.saturating_sub(wanted);
                    push_move(&mut out, 0, bottom);
                    out.extend(
                        std::iter::repeat_n(b"\n", wanted.saturating_sub(1) as usize).flatten(),
                    );
                }
            }
            self.content_start = self.top;
            push_move(&mut out, 0, self.top);
            out.extend_from_slice(ERASE_DOWN);
            self.cursor_at = Some((0, self.top));
        } else {
            self.begin(&mut out);
            if resized {
                self.resize_to(size, wanted, &mut out);
            }
            self.hide_for_paint(&mut out);
        }

        for commit in &frame.commits {
            self.commit(commit, &mut out);
        }
        if wanted != self.viewport_height {
            self.set_height(wanted, &mut out);
        }
        self.paint(frame, &mut out);
        self.place_cursor(frame, &mut out);
        self.end(&mut out);
        out
    }

    fn begin(&self, out: &mut Vec<u8>) {
        if self.synchronized {
            out.extend_from_slice(BEGIN_SYNC);
        }
    }

    fn end(&self, out: &mut Vec<u8>) {
        if self.synchronized {
            out.extend_from_slice(END_SYNC);
        }
    }

    /// Without synchronized output, hide the cursor while rows are rewritten.
    fn hide_for_paint(&mut self, out: &mut Vec<u8>) {
        if !self.synchronized && self.cursor_visible {
            out.extend_from_slice(CURSOR_HIDE);
            self.cursor_visible = false;
        }
    }

    fn painted_differs(&self, frame: &Frame, wanted: u16) -> bool {
        let rows = visible_rows(&frame.live, wanted);
        rows.iter()
            .enumerate()
            .any(|(i, row)| self.painted.get(i).and_then(|p| p.as_deref()) != Some(row.as_str()))
    }

    fn cursor_needs_move(&self, frame: &Frame, wanted: u16) -> bool {
        match (frame.cursor, self.cursor_at) {
            (None, _) => false,
            (Some((row, col)), at) => at != Some((col, self.top_for(wanted) + row)),
        }
    }

    fn commit(&mut self, text: &str, out: &mut Vec<u8>) {
        let mut term = OutTerm {
            out,
            size: self.size,
            viewport: Rect::new(0, self.viewport_top(), self.size.0, self.viewport_height),
            reset: false,
        };
        // A commit's closing newline terminates its last row; the emitter
        // would draw it as one more, blank, row.
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = &scrub_keeping_newlines(text);
        // The writer is a Vec, so writes cannot fail.
        let written = text.split('\n').count() as u16;
        let before = self.top;
        let _ = emit_to_scrollback(&mut term, text);
        // The emitter moved the region below what it wrote.
        let moved = term.viewport.y;
        self.top = moved;
        self.note_scroll((before + written).saturating_sub(moved));
        if term.reset {
            self.painted.fill(None);
        }
        // The vendored routine leaves the cursor at the new viewport top.
        self.cursor_at = Some((0, self.top));
    }

    /// `rows` rows of the screen left through the top.
    fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        if rows > self.content_start {
            self.scrolled = true;
        }
        self.content_start = self.content_start.saturating_sub(rows);
    }

    fn set_height(&mut self, wanted: u16, out: &mut Vec<u8>) {
        let (_, rows) = self.size;
        let old = self.viewport_height;
        if wanted > old {
            // Scroll the screen up when the region would run past the bottom;
            // the old live rows move up with it and are repainted.
            let overflow = (self.top + wanted).saturating_sub(rows);
            if overflow > 0 {
                push_move(out, 0, rows.saturating_sub(1));
                out.extend(std::iter::repeat_n(b"\n", overflow as usize).flatten());
                self.top -= overflow;
                self.note_scroll(overflow);
            }
        } else {
            // Rows that left the region stay where they are, blank, below it.
            for i in wanted..old {
                push_move(out, 0, self.top + i);
                out.extend_from_slice(ERASE_LINE);
            }
        }
        self.viewport_height = wanted;
        self.painted = vec![None; wanted as usize];
        self.cursor_at = None;
    }

    fn resize_to(&mut self, size: (u16, u16), wanted: u16, out: &mut Vec<u8>) {
        // Stage 2.8 replaces this with the reflow contract; until then keep
        // the live region valid at the new geometry.
        self.size = size;
        self.viewport_height = wanted;
        self.top = size.1.saturating_sub(wanted);
        self.painted = vec![None; wanted as usize];
        let top = self.viewport_top();
        push_move(out, 0, top);
        out.extend_from_slice(ERASE_DOWN);
        self.cursor_at = Some((0, top));
    }

    /// Repaints after a resize by rewriting the terminal's whole history.
    ///
    /// Erasing cannot survive a resize: reflowing terminals (iTerm2, Ghostty,
    /// Apple Terminal) split every row wider than the new width before the
    /// app hears about it, pushing stale pieces of the old frame into native
    /// scrollback where no escape sequence reaches them. So the screen and the
    /// scrollback are cleared (`2J`, then `3J`; `2J` first so terminals that
    /// archive the screen on it archive into what `3J` clears) and rewritten:
    /// `prefix` raw, as committed, letting the terminal soft-wrap rows from a
    /// wider measure; `tail`, laid out at the new width, from the top of a
    /// clean screen. Output from before this interface started is lost.
    pub fn rewrite(
        &mut self,
        size: (u16, u16),
        prefix: &[String],
        tail: &[String],
        wanted: u16,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let rows = size.1.max(1);
        let wanted = wanted.clamp(1, rows);
        self.size = size;
        self.begin(&mut out);
        if self.cursor_visible {
            out.extend_from_slice(CURSOR_HIDE);
            self.cursor_visible = false;
        }
        out.extend_from_slice(b"\x1b[2J\x1b[H\x1b[3J");
        for row in prefix {
            out.extend_from_slice(scrub(row).as_bytes());
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
        if !prefix.is_empty() {
            // How many screen rows the raw prefix took depends on the
            // terminal's wrapping: scroll all of it into scrollback so the
            // tail starts on a clean screen at a known row.
            push_move(&mut out, 0, rows - 1);
            out.extend(std::iter::repeat_n(b"\n", rows as usize).flatten());
            push_move(&mut out, 0, 0);
        }
        for row in tail {
            out.extend_from_slice(scrub(row).as_bytes());
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
        // The cursor is on the row after the tail; the region starts there,
        // scrolling the screen up for whatever does not fit below it.
        let after = tail.len().min((rows - 1) as usize) as u16;
        let overflow = (after + wanted).saturating_sub(rows);
        if overflow > 0 {
            push_move(&mut out, 0, rows - 1);
            out.extend(std::iter::repeat_n(b"\n", overflow as usize).flatten());
        }
        let top = after - overflow;
        self.scrolled = !prefix.is_empty() || tail.len() > top as usize;
        self.content_start = 0;
        self.top = top;
        self.viewport_height = wanted;
        self.painted = vec![None; wanted as usize];
        self.cursor_at = None;
        self.anchor = top;
        push_move(&mut out, 0, top);
        self.end(&mut out);
        out
    }

    fn paint(&mut self, frame: &Frame, out: &mut Vec<u8>) {
        let top = self.viewport_top();
        let rows = visible_rows(&frame.live, self.viewport_height);
        for (i, row) in rows.iter().enumerate() {
            if self.painted.get(i).and_then(|p| p.as_deref()) == Some(row.as_str()) {
                continue;
            }
            push_move(out, 0, top + i as u16);
            out.extend_from_slice(ERASE_LINE);
            out.extend_from_slice(scrub(row).as_bytes());
            // Styling must not leak into the next row.
            out.extend_from_slice(b"\x1b[0m");
            if let Some(slot) = self.painted.get_mut(i) {
                *slot = Some(row.clone());
            }
            self.cursor_at = None;
        }
    }

    fn place_cursor(&mut self, frame: &Frame, out: &mut Vec<u8>) {
        match frame.cursor {
            Some((row, col)) => {
                let target = (col, self.viewport_top() + row);
                self.anchor = target.1;
                if self.cursor_at != Some(target) {
                    push_move(out, target.0, target.1);
                    self.cursor_at = Some(target);
                }
                if !self.cursor_visible {
                    out.extend_from_slice(CURSOR_SHOW);
                    self.cursor_visible = true;
                }
            }
            None => {
                // Parked on the region's first row, so the resize report has
                // a row to be compared against.
                let target = (0, self.viewport_top());
                self.anchor = target.1;
                if self.cursor_at != Some(target) {
                    push_move(out, target.0, target.1);
                    self.cursor_at = Some(target);
                }
                if self.cursor_visible {
                    out.extend_from_slice(CURSOR_HIDE);
                    self.cursor_visible = false;
                }
            }
        }
    }
}

/// The rows that fit: when the live region is taller than the viewport the
/// bottom rows win (the composer lives there).
fn visible_rows(live: &[String], height: u16) -> Vec<String> {
    let height = height.max(1) as usize;
    let skip = live.len().saturating_sub(height);
    let mut rows: Vec<String> = live[skip..].to_vec();
    while rows.len() < height {
        rows.push(String::new());
    }
    rows
}

/// C0 controls that move the cursor or otherwise act on the terminal; ESC
/// and BEL stay, every escape sequence the interface emits is built from
/// them. Model text reaches rows verbatim, and a lone `\r` in it sends the
/// rest of a row back to column 0 over its own border and desynchronises the
/// row count of everything painted after it.
fn moves_cursor(c: char) -> bool {
    matches!(c, '\x00'..='\x06' | '\x08'..='\x1a' | '\x1c'..='\x1f' | '\x7f')
}

/// One row as it may reach the terminal: nothing in it moves the cursor.
fn scrub(row: &str) -> std::borrow::Cow<'_, str> {
    if row.contains(moves_cursor) {
        row.replace(moves_cursor, "").into()
    } else {
        row.into()
    }
}

/// A commit: rows separated by `\n`, each scrubbed.
fn scrub_keeping_newlines(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(|c| c != '\n' && moves_cursor(c)) {
        text.replace(|c| c != '\n' && moves_cursor(c), "").into()
    } else {
        text.into()
    }
}

/// CUP with 0-based (column, row).
fn push_move(out: &mut Vec<u8>, column: u16, row: u16) {
    let _ = write!(out, "\x1b[{};{}H", row + 1, column + 1);
}

/// The vendored scrollback emitter writes through this view of the screen.
struct OutTerm<'a> {
    out: &'a mut Vec<u8>,
    size: (u16, u16),
    viewport: Rect,
    reset: bool,
}

impl TerminalLike for OutTerm<'_> {
    type Writer = Vec<u8>;

    fn size(&self) -> io::Result<Size> {
        Ok(Size::new(self.size.0, self.size.1))
    }

    fn viewport_area(&self) -> Rect {
        self.viewport
    }

    fn clear(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn reset_back_buffer(&mut self) {
        self.reset = true;
    }

    fn set_viewport_area(&mut self, area: Rect) {
        self.viewport = area;
    }

    fn writer_mut(&mut self) -> &mut Vec<u8> {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render::testterm::Emu;

    const SIZE: (u16, u16) = (40, 12);

    fn frame(commits: &[&str], live: &[&str], cursor: Option<(u16, u16)>) -> Frame {
        Frame {
            commits: commits.iter().map(|s| s.to_string()).collect(),
            live: live.iter().map(|s| s.to_string()).collect(),
            cursor,
        }
    }

    fn drive(screen: &mut Screen, emu: &mut Emu, f: &Frame) -> Vec<u8> {
        let bytes = screen.render(f, SIZE);
        emu.feed(&bytes);
        bytes
    }

    fn setup(sync: bool) -> (Screen, Emu) {
        (
            Screen::new(SIZE, sync),
            Emu::new(SIZE.0 as usize, SIZE.1 as usize),
        )
    }

    #[test]
    fn live_region_is_pinned_to_the_bottom_rows() {
        let (mut screen, mut emu) = setup(true);
        drive(
            &mut screen,
            &mut emu,
            &frame(&[], &["box top", "> hi", "status"], Some((1, 4))),
        );
        let rows = emu.rows_text();
        assert_eq!(&rows[9..], ["box top", "> hi", "status"]);
        assert!(rows[..9].iter().all(|r| r.is_empty()));
        assert_eq!((emu.cx, emu.cy), (4, 10));
    }

    #[test]
    fn commits_land_once_in_order_above_the_live_region() {
        let (mut screen, mut emu) = setup(true);
        let live = ["composer", "status"];
        drive(&mut screen, &mut emu, &frame(&[], &live, None));
        for n in 1..=15 {
            let line = format!("message {n}");
            drive(&mut screen, &mut emu, &frame(&[&line], &live, None));
        }
        let transcript = emu.transcript();
        let messages: Vec<&String> = transcript
            .iter()
            .filter(|l| l.starts_with("message"))
            .collect();
        assert_eq!(
            messages.len(),
            15,
            "each commit appears exactly once: {transcript:?}"
        );
        let expected: Vec<String> = (1..=15).map(|n| format!("message {n}")).collect();
        assert_eq!(messages.into_iter().cloned().collect::<Vec<_>>(), expected);
        let rows = emu.rows_text();
        assert_eq!(&rows[10..], ["composer", "status"], "live region intact");
    }

    #[test]
    fn newline_terminated_commits_do_not_double_the_gap() {
        let (mut screen, mut emu) = setup(true);
        let live = ["composer", "status"];
        drive(&mut screen, &mut emu, &frame(&[], &live, None));
        drive(&mut screen, &mut emu, &frame(&["\none\n"], &live, None));
        drive(&mut screen, &mut emu, &frame(&["\ntwo\n"], &live, None));
        let rows = emu.rows_text();
        assert_eq!(
            rows[6..],
            ["", "one", "", "two", "composer", "status"],
            "{rows:?}"
        );
    }

    #[test]
    fn region_starts_at_the_shell_cursor_row() {
        let (mut screen, mut emu) = setup(true);
        emu.feed(b"$ dim\r\n");
        screen.set_origin(1);
        drive(
            &mut screen,
            &mut emu,
            &frame(&["\nbanner\n"], &["composer", "status"], None),
        );
        let rows = emu.rows_text();
        assert_eq!(rows[0], "$ dim");
        assert_eq!(rows[1..5], ["", "banner", "composer", "status"], "{rows:?}");
    }

    #[test]
    fn rewrite_clears_scrollback_and_writes_the_history_once() {
        let (mut screen, mut emu) = setup(true);
        emu.feed(b"before-start\r\n");
        screen.set_origin(1);
        let live = ["composer", "status"];
        drive(
            &mut screen,
            &mut emu,
            &frame(&["\nfirst\n", "\nsecond\n"], &live, Some((0, 0))),
        );
        let prefix = vec!["archived".to_owned()];
        let tail: Vec<String> = ["", "first", "", "second"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let wanted = screen.wanted_for(SIZE, live.len());
        let bytes = screen.rewrite(SIZE, &prefix, &tail, wanted);
        emu.feed(&bytes);
        drive(&mut screen, &mut emu, &frame(&[], &live, Some((0, 0))));
        let rows = emu.rows_text();
        assert_eq!(
            rows[..6],
            ["", "first", "", "second", "composer", "status"],
            "{rows:?}"
        );
        let transcript = emu.transcript();
        assert!(
            !transcript.iter().any(|l| l == "before-start"),
            "{transcript:?}"
        );
        assert_eq!(transcript.iter().filter(|l| *l == "archived").count(), 1);
        assert_eq!(transcript.iter().filter(|l| *l == "first").count(), 1);
    }

    #[test]
    fn row_text_never_moves_the_cursor() {
        let (mut screen, mut emu) = setup(true);
        drive(
            &mut screen,
            &mut emu,
            &frame(
                &["分位数（推荐\r）\n"],
                &["│ 1 ( ) 分位数（推荐\r） │"],
                None,
            ),
        );
        // The emulator keeps a `\0` spacer after each wide character.
        let transcript: Vec<String> = emu
            .transcript()
            .iter()
            .map(|l| l.replace('\0', ""))
            .collect();
        assert!(
            transcript.iter().any(|l| l == "分位数（推荐）"),
            "{transcript:?}"
        );
        assert!(
            transcript.iter().any(|l| l == "│ 1 ( ) 分位数（推荐） │"),
            "{transcript:?}"
        );
    }

    #[test]
    fn unchanged_frame_writes_nothing_and_keeps_the_cursor_still() {
        let (mut screen, mut emu) = setup(true);
        let f = frame(&[], &["a", "b"], Some((0, 1)));
        drive(&mut screen, &mut emu, &f);
        let again = drive(&mut screen, &mut emu, &f);
        assert!(
            again.is_empty(),
            "no bytes for an identical frame: {again:?}"
        );
    }

    #[test]
    fn only_changed_rows_are_rewritten() {
        let (mut screen, mut emu) = setup(true);
        drive(
            &mut screen,
            &mut emu,
            &frame(&[], &["top", "middle", "bottom"], Some((1, 0))),
        );
        let bytes = drive(
            &mut screen,
            &mut emu,
            &frame(&[], &["top", "MIDDLE", "bottom"], Some((1, 0))),
        );
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("MIDDLE"));
        assert!(
            !text.contains("top") && !text.contains("bottom"),
            "{text:?}"
        );
        assert_eq!(emu.rows_text()[10], "MIDDLE");
    }

    #[test]
    fn cursor_moves_without_repainting_rows() {
        let (mut screen, mut emu) = setup(true);
        drive(
            &mut screen,
            &mut emu,
            &frame(&[], &["abcdef"], Some((0, 1))),
        );
        let bytes = drive(
            &mut screen,
            &mut emu,
            &frame(&[], &["abcdef"], Some((0, 4))),
        );
        assert_eq!((emu.cx, emu.cy), (4, 11));
        assert!(!String::from_utf8_lossy(&bytes).contains("abcdef"));
    }

    #[test]
    fn cursor_visibility_toggles_only_when_it_changes() {
        let (mut screen, mut emu) = setup(true);
        drive(&mut screen, &mut emu, &frame(&[], &["x"], None));
        assert!(!emu.cursor_visible);
        let before = emu.visibility_changes;
        drive(&mut screen, &mut emu, &frame(&[], &["xy"], None));
        assert_eq!(emu.visibility_changes, before, "stayed hidden, no toggles");
        drive(&mut screen, &mut emu, &frame(&[], &["xy"], Some((0, 2))));
        assert!(emu.cursor_visible);
    }

    #[test]
    fn growing_and_shrinking_the_live_region_keeps_history_intact() {
        let (mut screen, mut emu) = setup(true);
        drive(
            &mut screen,
            &mut emu,
            &frame(&["history line"], &["c", "s"], None),
        );
        drive(
            &mut screen,
            &mut emu,
            &frame(&[], &["stream 1", "stream 2", "c", "s"], None),
        );
        let rows = emu.rows_text();
        assert_eq!(&rows[8..], ["stream 1", "stream 2", "c", "s"]);
        assert!(emu.transcript().contains(&"history line".to_owned()));
        drive(&mut screen, &mut emu, &frame(&[], &["c", "s"], None));
        let rows = emu.rows_text();
        // The region keeps its top and gives its rows back below it.
        assert_eq!(&rows[8..10], ["c", "s"]);
        assert!(rows[10..].iter().all(String::is_empty), "{rows:?}");
        assert!(
            rows.iter().all(|r| !r.contains("stream")),
            "stale rows cleared: {rows:?}"
        );
        assert_eq!(
            emu.transcript()
                .iter()
                .filter(|l| *l == "history line")
                .count(),
            1
        );
    }

    #[test]
    fn long_commit_wraps_softly_and_appears_once() {
        let (mut screen, mut emu) = setup(true);
        let live = ["c", "s"];
        drive(&mut screen, &mut emu, &frame(&[], &live, None));
        let long = "x".repeat(95);
        drive(&mut screen, &mut emu, &frame(&[&long], &live, None));
        let joined: String = emu
            .transcript()
            .iter()
            .filter(|l| l.starts_with('x'))
            .cloned()
            .collect();
        assert_eq!(joined.len(), 95);
    }

    #[test]
    fn synchronized_blocks_balance_and_are_skipped_when_disabled() {
        let (mut screen, mut emu) = setup(true);
        drive(&mut screen, &mut emu, &frame(&["a"], &["c"], Some((0, 0))));
        assert_eq!(emu.sync_depth, 0);
        assert!(emu.sync_blocks >= 1);

        let (mut screen, mut emu) = setup(false);
        drive(&mut screen, &mut emu, &frame(&["a"], &["c"], Some((0, 0))));
        assert_eq!(emu.sync_blocks, 0);
        assert!(emu.cursor_visible, "cursor shown again after the paint");
    }

    #[test]
    fn oversized_live_region_keeps_the_bottom_rows_and_some_history() {
        let (mut screen, mut emu) = setup(true);
        let live: Vec<String> = (0..20).map(|n| format!("row {n}")).collect();
        let refs: Vec<&str> = live.iter().map(String::as_str).collect();
        drive(&mut screen, &mut emu, &frame(&[], &refs, None));
        assert_eq!(screen.viewport_height(), SIZE.1 - MIN_HISTORY_ROWS);
        assert_eq!(emu.row(11), "row 19");
    }

    #[test]
    fn hyperlinks_survive_wrapping_and_commit_once() {
        let (mut screen, mut emu) = setup(true);
        let live = ["c", "s"];
        drive(&mut screen, &mut emu, &frame(&[], &live, None));
        let link = crate::ui::render::osc8::hyperlink("https://example.com/a", "docs link");
        let text = format!("see {link} and then {}", "word ".repeat(12));
        let bytes = drive(&mut screen, &mut emu, &frame(&[&text], &live, None));
        let sent = String::from_utf8_lossy(&bytes).into_owned();
        assert!(sent.contains("\x1b]8;;https://example.com/a\x1b\\docs link\x1b]8;;\x1b\\"));
        let joined = emu.transcript().join("\n");
        assert_eq!(joined.matches("docs link").count(), 1);
    }
}
