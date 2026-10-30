//! The write path of the fullscreen interface.
//!
//! The transcript is a virtual list over the committed blocks the App keeps
//! as sources ([`Document`]): only each block's height is kept for the
//! current width, and rows are rendered for the blocks near the viewport and
//! dropped again once it moves away. Every frame paints the visible slice
//! above the dock (queue, composer, panels) pinned to the bottom of
//! the screen, with a scrollbar in the last column once the transcript is
//! taller than its viewport. A shorter transcript starts at the top and
//! grows down towards the dock. While the view follows new output it never
//! moves back up: when the live tail shrinks (a running thought folding into
//! one row) the rows above stay where they are and blank rows below take the
//! difference until new output fills them. Rows are diffed against what was
//! painted last, so a frame with nothing new writes nothing.
//!
//! ```text
//!  ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓     █  Tab: the picked
//!  ┃ › a user message                            ┃     ▅  message, boxed on
//!  ┗━e edit · b branch · f fork━━━━━━━━━━━━━━━━━┛     █  its blank rows
//!  answer rows (the live tail scrolls with them)       █  scrollbar thumb
//!  ╭───────────────────────────── provider / model ─╮      in eighths
//!  │ composer                                       │      dock: never
//!  ╰─ status ────────────────── ~/project (branch) ─╯      scrolls
//! ```
//!
//! An overlay (the command palette, a picker) is centred over the
//! transcript, which is dimmed behind it.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::ui::render::Frame;
use crate::ui::render::panel::{clip, escape_len, strip_escapes};
use crate::ui::render::screen::scrub;
use crate::ui::terminal::{MouseEvent, MouseKind};
use crate::ui::text::{MARGIN_MARKS, display_width};
use crate::ui::theme::Palette;
use crate::ui::theme::ansi::{Paint, fg, paint};

const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
const END_SYNC: &[u8] = b"\x1b[?2026l";
const CURSOR_SHOW: &[u8] = b"\x1b[?25h";
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";
const ERASE_LINE: &[u8] = b"\x1b[2K";
/// Transcript rows the dock always leaves visible.
const MIN_VIEW_ROWS: usize = 3;
/// Rows one wheel notch scrolls.
const WHEEL_ROWS: isize = 3;
/// Reports closer than this come from a trackpad's burst, not a notch.
const WHEEL_BURST: Duration = Duration::from_millis(6);
/// Burst gaps in a row that mark the device as a trackpad; a busy frame can
/// deliver two notches back to back, never this many.
const WHEEL_BURST_RUN: usize = 4;
/// A pause this long starts a new wheel streak at the base speed.
const WHEEL_STREAK_GAP: Duration = Duration::from_millis(150);
/// Most a fast streak multiplies a mouse notch and a trackpad report by.
const WHEEL_MAX_NOTCH: f64 = 3.0;
/// Presses closer than this on the same spot count as one multi-click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Characters that end a double-clicked word besides whitespace: tmux's
/// default `word-separators`.
const WORD_SEPARATORS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";
const WHEEL_MAX_FINE: f64 = 6.0;
/// Lower eighth blocks, index = eighths filled from the bottom.
const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
/// The keys a picked message takes, the full hint first.
const PICK_HINTS: [&str; 2] = ["e edit · b branch · f fork", "e edit"];

/// The committed transcript as the App keeps it: blocks, oldest first,
/// that can be laid out for any width.
pub trait Document {
    /// Blocks dropped from the front so far; block `index` here has the
    /// stable id `dropped() + index`.
    fn dropped(&self) -> usize;
    fn len(&self) -> usize;
    /// Block `index` laid out for a screen `cols` wide, one string per row.
    fn rows(&self, index: usize, cols: usize) -> Vec<String>;
    /// About how many rows block `index` takes at `cols`, without the costly
    /// layout: the viewport lays out exactly only the blocks it reaches.
    fn estimate(&self, index: usize, cols: usize) -> usize;
    /// The text of block `index` when it is a user message.
    fn message(&self, index: usize) -> Option<&str>;
    /// Where a line or fork opened from user message `index` cuts the shown
    /// line (timeline UI §4.1).
    fn cut(&self, index: usize) -> Option<u64>;
    /// The member cards of block `index` a pick can stop on, by delegation.
    fn cards(&self, index: usize) -> Vec<String>;
    /// Rows `[start, end)` of block `index` at `cols` that card `card` takes.
    fn card_rows(&self, index: usize, cols: usize, card: &str) -> Option<(usize, usize)>;
    /// The member card that row `row` of block `index` at `cols` opens.
    fn card_at(&self, index: usize, cols: usize, row: usize) -> Option<String>;
    /// How many times blocks were rewritten in place (a streamed code block
    /// grows line by line, a delegation card changes), and the stable ids
    /// rewritten after rewrite `since`; `None` when those are no longer
    /// known.
    fn rewritten(&self, since: u64) -> (u64, Option<Vec<usize>>);
}

/// What a mouse report did.
#[derive(Debug, PartialEq, Eq)]
pub enum MouseOutcome {
    Ignored,
    Redraw,
    /// A selection ended: its text, ready for the clipboard.
    Copy(String),
    /// The wheel turned over an overlay: the list in it moves (+ down).
    Overlay(isize),
    /// A click landed on a link: its target, to open.
    Open(String),
    /// A click landed elsewhere in the transcript: on row `row` of block
    /// `block` (stable id), or on row `row` of the live tail when `None`.
    Click {
        block: Option<usize>,
        row: usize,
    },
}

/// Where a scrolled view's top row is: `offset` rows into block `block`
/// (stable id; one past the newest block is the live tail). A block keeps
/// the view in place while heights above it change from estimates to exact
/// rows, and across a relayout.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    block: usize,
    offset: usize,
}

/// Turns wheel reports into rows. A mouse wheel reports once per notch, three
/// rows each; trackpads (iTerm2, Ghostty, kitty on macOS, libinput) send
/// dense bursts and scroll a row per report once one is seen. A fast streak
/// speeds up, as the TS interface does: the reports of one burst count as a
/// single sample, and the multiplier grows with the rate of the last three.
#[derive(Debug, Default)]
struct Wheel {
    last: Option<Instant>,
    run: usize,
    fine: bool,
    /// The last sample of the streak, and the gaps before the last three.
    sample: Option<Instant>,
    gaps: std::collections::VecDeque<f64>,
}

impl Wheel {
    fn rows(&mut self, now: Instant) -> isize {
        let burst = self
            .last
            .is_some_and(|last| now.duration_since(last) < WHEEL_BURST);
        self.run = if burst { self.run + 1 } else { 0 };
        self.fine |= self.run >= WHEEL_BURST_RUN;
        self.last = Some(now);
        let (base, max) = if self.fine {
            (1, WHEEL_MAX_FINE)
        } else {
            (WHEEL_ROWS, WHEEL_MAX_NOTCH)
        };
        if burst {
            return base;
        }
        let gap = self.sample.map(|at| now.duration_since(at));
        self.sample = Some(now);
        match gap {
            Some(gap) if gap <= WHEEL_STREAK_GAP => {
                if self.gaps.len() == 3 {
                    self.gaps.pop_front();
                }
                self.gaps.push_back(gap.as_secs_f64() * 1000.0);
            }
            _ => {
                self.gaps.clear();
                return base;
            }
        }
        let average = self.gaps.iter().sum::<f64>() / self.gaps.len() as f64;
        let rate = 100.0 / average.max(1.0);
        let factor = (1.0 + 0.8 * ((rate / 3.0).exp() - 1.0)).min(max);
        (base as f64 * factor).round() as isize
    }
}

/// A text selection in transcript coordinates: `(row, column)`, absolute
/// row index, display column. It shows once the pointer leaves the pressed
/// cell and stays after the release until the next press or Esc.
#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: (usize, usize),
    head: (usize, usize),
    dragged: bool,
}

impl Selection {
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The columns `[start, end)` selected on `row`, if any. `inset` is that
    /// row's chrome ([`text_inset`]); text before it is not selected.
    fn columns(&self, row: usize, inset: usize) -> Option<(usize, usize)> {
        let ((r0, c0), (r1, c1)) = self.ordered();
        if row < r0 || row > r1 {
            return None;
        }
        let start = if row == r0 { c0 } else { 0 };
        let end = if row == r1 { c1 + 1 } else { usize::MAX };
        Some((start.max(inset), end))
    }
}

/// Columns of chrome before the copyable text of a plain row.
///
/// Committed rows have one margin space. This fallback recognizes
/// [`MARGIN_MARKS`] after it. Ambiguous marks after the margin are kept, including shell/status chrome;
/// content starting with a margin mark is stripped. Per-row chrome metadata
/// is needed to distinguish those cases.
fn text_inset(plain: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut glyphs = plain.graphemes(true);
    let Some(first) = glyphs.next() else {
        return 0;
    };
    if first != " " {
        return 0;
    }
    match glyphs.next() {
        Some(mark) if MARGIN_MARKS.contains(&mark) => 1 + display_width(mark),
        _ => 1,
    }
}

pub struct FullScreen {
    size: (u16, u16),
    /// Columns the blocks are laid out for; changes only with `relayout`,
    /// so a resize drag does not measure the transcript on every step.
    layout_cols: usize,
    /// `Document::dropped` when the heights were last synced.
    base: usize,
    /// `Document::rewritten` when the heights were last synced.
    rewrites: u64,
    /// Rows of each block at `layout_cols`: exact where `measured`, else
    /// the document's estimate.
    heights: Vec<usize>,
    measured: Vec<bool>,
    /// First row of each block; one more entry holds the total.
    starts: Vec<usize>,
    /// Rendered rows of the blocks near the viewport, by block id.
    cache: HashMap<usize, Vec<String>>,
    /// The live tail of the last frame (streaming answer, running tools).
    tail: Vec<String>,
    /// Where the last press landed and the row it showed: a click counts
    /// only while that row still shows the same text at the release, so
    /// output that moves the transcript in between cannot redirect it.
    press: Option<(MouseOutcome, String)>,
    /// The last press: when, where, the row it showed and how many presses
    /// in a row landed there (a double click selects a word, a triple
    /// click the row).
    clicks: Option<(Instant, (usize, usize), String, u8)>,
    /// The top of the view while the reader has scrolled away from the
    /// bottom; `None` follows new output.
    pinned: Option<Anchor>,
    /// The lowest bottom row the followed view has reached: it does not
    /// move back up when the transcript gets shorter, and a dock that gets
    /// shorter (a panel closing) brings the rows above back down to it.
    floor: usize,
    /// Transcript viewport height of the last frame.
    view_rows: usize,
    wheel: Wheel,
    /// The last frame had an overlay: the wheel belongs to it.
    overlay_open: bool,
    selection: Option<Selection>,
    /// Block id of the user message or member card picked with Tab.
    focus: Option<usize>,
    /// The picked member card in that block, by delegation id.
    focus_card: Option<String>,
    /// Rows as last painted; `None` entries are unknown (repaint).
    painted: Vec<Option<String>>,
    cursor_visible: bool,
    cursor_at: Option<(u16, u16)>,
    synchronized: bool,
}

impl FullScreen {
    /// `synchronized`: emit DEC 2026 around frames (false inside tmux).
    pub fn new(size: (u16, u16), synchronized: bool) -> Self {
        Self {
            size,
            layout_cols: size.0 as usize,
            base: 0,
            rewrites: 0,
            heights: Vec::new(),
            measured: Vec::new(),
            starts: vec![0],
            cache: HashMap::new(),
            tail: Vec::new(),
            press: None,
            clicks: None,
            pinned: None,
            floor: 0,
            view_rows: 0,
            wheel: Wheel::default(),
            overlay_open: false,
            selection: None,
            focus: None,
            focus_card: None,
            painted: Vec::new(),
            cursor_visible: true,
            cursor_at: None,
            synchronized,
        }
    }

    /// The terminal size of the last frame.
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// The width the transcript is laid out for.
    pub fn layout_cols(&self) -> u16 {
        self.layout_cols as u16
    }

    /// Lays the transcript out again for `cols` columns (a settled resize,
    /// a theme change, tool details toggled) and repaints everything on the
    /// next frame. A scrolled view keeps its top block; rows into it scale
    /// with the width. A followed view starts over from the bottom: the
    /// floor held the old layout's top, and a transcript that got shorter
    /// (every group collapsed) would otherwise show only its last row.
    pub fn relayout(&mut self, cols: u16) {
        let cols = (cols as usize).max(1);
        if let Some(anchor) = &mut self.pinned {
            anchor.offset = anchor.offset * self.layout_cols / cols;
        }
        self.floor = 0;
        self.layout_cols = cols;
        self.heights.clear();
        self.measured.clear();
        self.starts = vec![0];
        self.cache.clear();
        self.selection = None;
        self.invalidate();
    }

    /// Forgets what is on screen: the next frame paints every row.
    fn invalidate(&mut self) {
        self.painted.clear();
        self.cursor_at = None;
        self.cursor_visible = true;
    }

    /// Bytes that hand the screen back: the cursor is shown again.
    pub fn leave(&mut self) -> Vec<u8> {
        self.cursor_visible = true;
        CURSOR_SHOW.to_vec()
    }

    /// Scrolls the transcript by `rows` (negative is up, towards older rows).
    pub fn scroll(&mut self, rows: isize) {
        let max_top = self.max_top();
        let next = (self.top() as isize + rows).clamp(0, max_top as isize) as usize;
        self.pin(next);
    }

    /// One page of the transcript, keeping two rows of context.
    pub fn page(&mut self, down: bool) {
        let step = self.view_rows.saturating_sub(2).max(1) as isize;
        self.scroll(if down { step } else { -step });
    }

    /// Holds the view at the top of the transcript.
    pub fn scroll_to_top(&mut self) {
        self.pin(0);
    }

    /// Follows new output again, from the bottom of the transcript.
    pub fn follow(&mut self) {
        self.pinned = None;
        self.floor = 0;
    }

    /// Holds the view where it is, even at the bottom: a group opened by a
    /// click grows below its header instead of pushing it up.
    pub fn hold(&mut self) {
        self.floor = 0;
        self.pinned = Some(self.anchor_at(self.top()));
    }

    /// Holds the view with row `top` first; the bottom row follows.
    fn pin(&mut self, top: usize) {
        self.floor = 0;
        self.pinned = (top < self.max_top()).then(|| self.anchor_at(top));
    }

    /// Row `top` as an anchor: its block (the live tail past the last one)
    /// and the rows into it.
    fn anchor_at(&self, top: usize) -> Anchor {
        let blocks = self.heights.len();
        if top >= self.starts[blocks] {
            return Anchor {
                block: self.base + blocks,
                offset: top - self.starts[blocks],
            };
        }
        let index = self.starts.partition_point(|&s| s <= top) - 1;
        Anchor {
            block: self.base + index,
            offset: top - self.starts[index],
        }
    }

    /// True while a user message or member card is picked.
    pub fn focused(&self) -> bool {
        self.focus.is_some()
    }

    /// Picks the message or card before the picked one (the newest when
    /// none is), on Tab. Stays on the oldest.
    pub fn focus_older(&mut self, doc: &dyn Document) {
        let from = match self.focus {
            Some(id) => {
                let index = id.saturating_sub(doc.dropped()).min(doc.len());
                let stops = if index < doc.len() {
                    stops(doc, index)
                } else {
                    Vec::new()
                };
                if let Some(at) = stops.iter().position(|stop| *stop == self.focus_card)
                    && at > 0
                {
                    self.focus_card = stops[at - 1].clone();
                    self.reveal_focus(doc);
                    return;
                }
                index
            }
            None => doc.len(),
        };
        for index in (0..from.min(doc.len())).rev() {
            if let Some(stop) = stops(doc, index).pop() {
                self.focus = Some(doc.dropped() + index);
                self.focus_card = stop;
                self.reveal_focus(doc);
                return;
            }
        }
    }

    /// Picks the message or card after the picked one; past the newest the
    /// pick ends and the view follows the bottom again (Shift+Tab).
    pub fn focus_newer(&mut self, doc: &dyn Document) {
        let Some(id) = self.focus else { return };
        let index = id.saturating_sub(doc.dropped());
        if index < doc.len() {
            let stops = stops(doc, index);
            if let Some(at) = stops.iter().position(|stop| *stop == self.focus_card)
                && at + 1 < stops.len()
            {
                self.focus_card = stops[at + 1].clone();
                self.reveal_focus(doc);
                return;
            }
        }
        for index in index + 1..doc.len() {
            if let Some(stop) = stops(doc, index).into_iter().next() {
                self.focus = Some(doc.dropped() + index);
                self.focus_card = stop;
                self.reveal_focus(doc);
                return;
            }
        }
        self.clear_focus();
        self.follow();
    }

    /// Ends the pick, leaving the view where it is.
    /// The selection kept after a drag is on screen.
    pub fn selecting(&self) -> bool {
        self.selection.is_some_and(|s| s.dragged)
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn clear_focus(&mut self) {
        self.focus = None;
        self.focus_card = None;
    }

    /// Picks the user message drawn as block `id`, as Tab would.
    pub fn pick_block(&mut self, id: usize, doc: &dyn Document) {
        let Some(index) = id.checked_sub(doc.dropped()).filter(|i| *i < doc.len()) else {
            return;
        };
        if doc.message(index).is_none() {
            return;
        }
        self.focus = Some(id);
        self.focus_card = None;
        self.reveal_focus(doc);
    }

    /// Scrolls so block `id` starts at the top of the view.
    pub fn show_block(&mut self, id: usize, doc: &dyn Document) {
        self.sync(doc);
        if let Some(start) = id
            .checked_sub(self.base)
            .and_then(|index| self.starts.get(index))
        {
            self.pin((*start).min(self.max_top()));
        }
    }

    /// The picked message's text.
    pub fn focused_text(&self, doc: &dyn Document) -> Option<String> {
        if self.focus_card.is_some() {
            return None;
        }
        let index = self.focus?.checked_sub(doc.dropped())?;
        (index < doc.len())
            .then(|| doc.message(index))
            .flatten()
            .map(str::to_owned)
    }

    /// Where a line or fork opened from the picked message cuts.
    pub fn focused_cut(&self, doc: &dyn Document) -> Option<u64> {
        if self.focus_card.is_some() {
            return None;
        }
        let index = self.focus?.checked_sub(doc.dropped())?;
        (index < doc.len()).then(|| doc.cut(index)).flatten()
    }

    /// The picked member card's delegation, or the line a picked marker
    /// opened.
    pub fn focused_card(&self) -> Option<&str> {
        self.focus_card.as_deref()
    }

    /// The member card a click on row `row` of block `block` (stable id)
    /// opens.
    pub fn card_at(&self, doc: &dyn Document, block: usize, row: usize) -> Option<String> {
        let index = block
            .checked_sub(doc.dropped())
            .filter(|index| *index < doc.len())?;
        doc.card_at(index, self.layout_cols, row)
    }

    /// Scrolls so the picked message's box or member card is on screen, its
    /// top at the top of the viewport when it was not: a card inside a tall
    /// tool group is revealed itself, not the group's first row.
    fn reveal_focus(&mut self, doc: &dyn Document) {
        self.sync(doc);
        let Some(index) = self.focus.and_then(|id| id.checked_sub(self.base)) else {
            return;
        };
        let rows = match self.focus_card {
            Some(_) => self.card_focus_rows(doc),
            None => self
                .starts
                .get(index)
                .zip(self.starts.get(index + 1))
                .map(|(s, e)| (*s, *e)),
        };
        let Some((start, end)) = rows else {
            return;
        };
        let top = self.top();
        if start < top || end >= top + self.view_rows {
            self.pin(start.min(self.max_top()));
        }
    }

    /// Wheel scrolling and drag selection inside the transcript viewport.
    pub fn mouse(&mut self, event: MouseEvent, doc: &dyn Document) -> MouseOutcome {
        self.sync(doc);
        let row = event.row as usize;
        let column = event.column as usize;
        if self.overlay_open {
            return match event.kind {
                MouseKind::WheelUp => MouseOutcome::Overlay(-1),
                MouseKind::WheelDown => MouseOutcome::Overlay(1),
                _ => MouseOutcome::Ignored,
            };
        }
        match event.kind {
            MouseKind::WheelUp => {
                let rows = self.wheel.rows(Instant::now());
                self.scroll(-rows);
                MouseOutcome::Redraw
            }
            MouseKind::WheelDown => {
                let rows = self.wheel.rows(Instant::now());
                self.scroll(rows);
                MouseOutcome::Redraw
            }
            MouseKind::Press => {
                let shown = self.selection.take().is_some_and(|s| s.dragged);
                self.press = None;
                let Some((index, text)) = (row < self.view_rows)
                    .then(|| self.at(row))
                    .flatten()
                    .and_then(|index| Some((index, self.row(index)?.to_owned())))
                else {
                    self.clicks = None;
                    return if shown {
                        MouseOutcome::Redraw
                    } else {
                        MouseOutcome::Ignored
                    };
                };
                let now = Instant::now();
                let count = match &self.clicks {
                    Some((when, (r, c), pressed, n))
                        if now.duration_since(*when) < MULTI_CLICK
                            && *r == index
                            && c.abs_diff(column) <= 1
                            && *pressed == text =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.press = Some((self.spot(index), text.clone()));
                let width = content_width(self.layout_cols);
                let plain = strip_escapes(&text);
                let inset = text_inset(&plain);
                let span = match count {
                    2 if column >= inset => word_at(&plain, column),
                    3 => Some((inset, width)),
                    _ => None,
                };
                self.clicks = Some((now, (index, column), text, count));
                self.selection = Some(match span {
                    Some((start, end)) => Selection {
                        anchor: (index, start),
                        head: (index, end.saturating_sub(1).max(start)),
                        dragged: true,
                    },
                    None => Selection {
                        anchor: (index, column),
                        head: (index, column),
                        dragged: false,
                    },
                });
                if shown || span.is_some() {
                    MouseOutcome::Redraw
                } else {
                    MouseOutcome::Ignored
                }
            }
            MouseKind::Drag => {
                let Some(selection) = self.selection else {
                    return MouseOutcome::Ignored;
                };
                let row = row.min(self.view_rows.saturating_sub(1));
                if !selection.dragged
                    && (self.at(row), column) == (Some(selection.anchor.0), selection.anchor.1)
                {
                    return MouseOutcome::Ignored;
                }
                if row == 0 {
                    self.scroll(-1);
                } else if row + 1 >= self.view_rows {
                    self.scroll(1);
                }
                let Some(index) = self.at(row) else {
                    return MouseOutcome::Ignored;
                };
                self.selection = Some(Selection {
                    head: (index, column),
                    dragged: true,
                    ..selection
                });
                MouseOutcome::Redraw
            }
            MouseKind::Release => {
                let Some(selection) = self.selection else {
                    return MouseOutcome::Ignored;
                };
                if selection.dragged {
                    let text = self.selected_text(&selection, doc);
                    if !text.is_empty() {
                        return MouseOutcome::Copy(text);
                    }
                    self.selection = None;
                    return MouseOutcome::Redraw;
                }
                self.selection = None;
                let (row, column) = selection.anchor;
                match (self.press.take(), self.row(row)) {
                    (Some((spot, pressed)), Some(text)) if pressed == text => {
                        match link_at(text, column) {
                            Some(url) => MouseOutcome::Open(url),
                            None => spot,
                        }
                    }
                    _ => MouseOutcome::Ignored,
                }
            }
        }
    }

    /// Renders `frame` over the transcript in `doc` for a terminal of
    /// `size`; returns the bytes to write (empty when nothing changed).
    pub fn render(
        &mut self,
        frame: &Frame,
        size: (u16, u16),
        palette: &Palette,
        doc: &dyn Document,
    ) -> Vec<u8> {
        if size != self.size {
            self.size = size;
            self.invalidate();
        }
        let (cols, rows) = (size.0 as usize, size.1.max(1) as usize);
        let width = content_width(cols);
        self.sync(doc);
        let split = frame.dock.min(frame.live.len());
        self.tail = frame.live[..split]
            .iter()
            .flat_map(|row| wrap_row(row, width))
            .collect();
        let dock = &frame.live[split..];
        let dock_rows = dock.len().min(rows.saturating_sub(MIN_VIEW_ROWS)).max(1);
        let dock_skip = dock.len().saturating_sub(dock_rows);
        self.view_rows = rows - dock_rows.min(rows);
        self.load(doc);
        let top = self.top();
        if self.pinned.is_none() {
            self.floor = top + self.view_rows.min(self.content_rows());
        }

        let dim = frame.overlay.is_some();
        self.overlay_open = dim;
        let bar = scrollbar(
            self.view_rows,
            self.content_rows(),
            top.min(self.max_top()),
            palette,
        );
        let focus = self.focus_rows();
        let card = self.card_focus_rows(doc);
        let mut screen: Vec<String> = Vec::with_capacity(rows);
        for y in 0..self.view_rows {
            let body = match self.at(y) {
                None => String::new(),
                Some(index) => {
                    let row = self.row(index).unwrap_or("");
                    match focus {
                        Some((start, end)) if (start..=end).contains(&index) => {
                            focus_row(row, index - start, end - start, cols, palette)
                        }
                        _ if card.is_some_and(|(start, end)| (start..end).contains(&index)) => {
                            card_focus_row(row, width, palette)
                        }
                        _ => match self
                            .selection
                            .filter(|s| s.dragged)
                            .and_then(|s| s.columns(index, text_inset(&strip_escapes(row))))
                        {
                            Some(range) => highlight(row, range, width),
                            None => clip(row, width).0,
                        },
                    }
                }
            };
            let body = if dim { faint(&body) } else { body };
            screen.push(match &bar {
                Some(bar) => {
                    let pad = cols.saturating_sub(1 + clip(&body, width).1);
                    format!("{body}\x1b[0m{}{}", " ".repeat(pad), bar[y])
                }
                None => body,
            });
        }
        for row in dock.iter().skip(dock_skip) {
            screen.push(clip(row, cols).0);
        }
        screen.truncate(rows);

        let mut cursor = frame.cursor.and_then(|(row, column)| {
            let row = (row as usize).checked_sub(split + dock_skip)?;
            Some((column, (self.view_rows + row).min(rows - 1) as u16))
        });
        if let Some(overlay) = &frame.overlay {
            let (x, y) = overlay_origin(overlay, cols, rows, self.view_rows);
            for (offset, row) in overlay.rows.iter().enumerate() {
                if let Some(base) = screen.get_mut(y + offset) {
                    *base = composite(base, row, x, cols);
                }
            }
            if let Some((row, column)) = overlay.cursor {
                cursor = Some(((x + column as usize) as u16, (y + row as usize) as u16));
            }
        }
        self.paint(screen, cursor)
    }

    /// Writes the rows that changed and moves the cursor.
    fn paint(&mut self, screen: Vec<String>, cursor: Option<(u16, u16)>) -> Vec<u8> {
        let mut out = Vec::new();
        if self.painted.len() != screen.len() {
            self.painted = vec![None; screen.len()];
        }
        let changed = screen
            .iter()
            .enumerate()
            .any(|(i, row)| self.painted[i].as_deref() != Some(row.as_str()));
        if !changed && cursor == self.cursor_at && self.cursor_visible == cursor.is_some() {
            return out;
        }
        if self.synchronized {
            out.extend_from_slice(BEGIN_SYNC);
        } else if self.cursor_visible {
            out.extend_from_slice(CURSOR_HIDE);
            self.cursor_visible = false;
        }
        for (i, row) in screen.into_iter().enumerate() {
            if self.painted[i].as_deref() == Some(row.as_str()) {
                continue;
            }
            push_move(&mut out, 0, i as u16);
            out.extend_from_slice(ERASE_LINE);
            out.extend_from_slice(scrub(&row).as_bytes());
            out.extend_from_slice(b"\x1b[0m");
            self.painted[i] = Some(row);
            self.cursor_at = None;
        }
        match cursor {
            Some(at) => {
                if self.cursor_at != Some(at) {
                    push_move(&mut out, at.0, at.1);
                    self.cursor_at = Some(at);
                }
                if !self.cursor_visible {
                    out.extend_from_slice(CURSOR_SHOW);
                    self.cursor_visible = true;
                }
            }
            None => {
                if self.cursor_visible {
                    out.extend_from_slice(CURSOR_HIDE);
                    self.cursor_visible = false;
                }
            }
        }
        if self.synchronized {
            out.extend_from_slice(END_SYNC);
        }
        out
    }

    /// Brings the heights in line with `doc`: blocks dropped at the front
    /// leave, new blocks at the back get estimates.
    fn sync(&mut self, doc: &dyn Document) {
        let dropped = doc.dropped();
        let gone = dropped.saturating_sub(self.base).min(self.heights.len());
        let mut changed = false;
        if gone > 0 {
            self.heights.drain(..gone);
            self.measured.drain(..gone);
            self.selection = None;
            self.cache.retain(|id, _| *id >= dropped);
            changed = true;
        }
        if let Some(anchor) = &mut self.pinned
            && anchor.block < dropped
        {
            *anchor = Anchor {
                block: dropped,
                offset: 0,
            };
        }
        if self.focus.is_some_and(|id| id < dropped) {
            self.clear_focus();
        }
        self.base = dropped;
        if self.heights.len() > doc.len() {
            self.heights.truncate(doc.len());
            self.measured.truncate(doc.len());
            changed = true;
        }
        let (rewrites, ids) = doc.rewritten(self.rewrites);
        if rewrites != self.rewrites {
            self.rewrites = rewrites;
            let ids = ids.unwrap_or_else(|| (dropped..dropped + self.heights.len()).collect());
            for id in ids {
                if let Some(index) = id.checked_sub(dropped).filter(|i| *i < self.heights.len()) {
                    self.heights[index] = doc.estimate(index, self.layout_cols);
                    self.measured[index] = false;
                    self.cache.remove(&id);
                    changed = true;
                }
            }
        }
        while self.heights.len() < doc.len() {
            let index = self.heights.len();
            self.heights.push(doc.estimate(index, self.layout_cols));
            self.measured.push(false);
            changed = true;
        }
        if changed {
            self.restart();
        }
    }

    /// Recomputes the first row of each block from the heights.
    fn restart(&mut self) {
        self.starts.clear();
        self.starts.push(0);
        let mut total = 0;
        for height in &self.heights {
            total += height;
            self.starts.push(total);
        }
    }

    fn layout(&self, doc: &dyn Document, index: usize, width: usize) -> Vec<String> {
        doc.rows(index, self.layout_cols)
            .iter()
            .flat_map(|row| wrap_row(row, width))
            .collect()
    }

    /// Lays out the blocks within a screen of the viewport exactly, keeping
    /// their rows, and forgets the rows of blocks further away. Exact heights
    /// move the rows of an anchored view, so this repeats until the blocks
    /// around the settled viewport are all measured.
    fn load(&mut self, doc: &dyn Document) {
        let view = self.view_rows.max(1);
        let width = content_width(self.layout_cols);
        let blocks = loop {
            let top = self.top();
            let blocks = self.blocks_between(top.saturating_sub(view), top + 2 * view);
            let mut changed = false;
            for index in blocks.clone() {
                if !self.measured[index] {
                    let rows = self.layout(doc, index, width);
                    self.heights[index] = rows.len();
                    self.measured[index] = true;
                    self.cache.insert(self.base + index, rows);
                    changed = true;
                }
            }
            if !changed {
                break blocks;
            }
            self.restart();
        };
        self.cache
            .retain(|id, _| blocks.contains(&id.wrapping_sub(self.base)));
        for index in blocks {
            let id = self.base + index;
            if !self.cache.contains_key(&id) {
                let rows = self.layout(doc, index, width);
                self.cache.insert(id, rows);
            }
        }
    }

    /// Indexes of the blocks with rows in `[from, to)`.
    fn blocks_between(&self, from: usize, to: usize) -> std::ops::Range<usize> {
        let total = self.heights.len();
        let first = self
            .starts
            .partition_point(|&s| s <= from)
            .saturating_sub(1);
        let last = self.starts.partition_point(|&s| s < to).min(total);
        first.min(total)..last
    }

    fn content_rows(&self) -> usize {
        self.starts.last().copied().unwrap_or(0) + self.tail.len()
    }

    fn max_top(&self) -> usize {
        self.content_rows().saturating_sub(self.view_rows)
    }

    /// The transcript row shown on viewport row `y`, none past its ends. A
    /// transcript shorter than the viewport sits at its bottom, against the
    /// composer, so the thread grows upwards.
    fn at(&self, y: usize) -> Option<usize> {
        let top = self.top();
        let pad = match top {
            0 => self.view_rows.saturating_sub(self.content_rows()),
            _ => 0,
        };
        Some(top + y.checked_sub(pad)?).filter(|&row| row < self.content_rows())
    }

    fn top(&self) -> usize {
        let max_top = self.max_top();
        let Some(anchor) = self.pinned else {
            let floor = self.floor.saturating_sub(self.view_rows);
            return max_top.max(floor.min(self.content_rows().saturating_sub(1)));
        };
        let start = anchor
            .block
            .checked_sub(self.base)
            .and_then(|index| self.starts.get(index))
            .copied()
            .unwrap_or(0);
        (start + anchor.offset).min(max_top)
    }

    /// Row `index` of the transcript, when its block is loaded.
    fn row(&self, index: usize) -> Option<&str> {
        let blocks = self.starts.last().copied().unwrap_or(0);
        if index >= blocks {
            return self.tail.get(index - blocks).map(String::as_str);
        }
        let block = self.starts.partition_point(|&s| s <= index) - 1;
        self.cache
            .get(&(self.base + block))?
            .get(index - self.starts[block])
            .map(String::as_str)
    }

    /// Where transcript row `index` is, as a click reports it.
    fn spot(&self, index: usize) -> MouseOutcome {
        let blocks = self.starts.last().copied().unwrap_or(0);
        if index >= blocks {
            return MouseOutcome::Click {
                block: None,
                row: index - blocks,
            };
        }
        let block = self.starts.partition_point(|&s| s <= index) - 1;
        MouseOutcome::Click {
            block: Some(self.base + block),
            row: index - self.starts[block],
        }
    }

    /// Rows `[top border, bottom border]` of the picked message's box: its
    /// block's leading blank row, its rows, and the next block's leading
    /// blank row.
    fn focus_rows(&self) -> Option<(usize, usize)> {
        if self.focus_card.is_some() {
            return None;
        }
        let index = self.focus?.checked_sub(self.base)?;
        let start = *self.starts.get(index)?;
        let end = *self.starts.get(index + 1)?;
        Some((start, end.min(self.content_rows())))
    }

    /// Rows `[start, end)` of the picked member card.
    fn card_focus_rows(&self, doc: &dyn Document) -> Option<(usize, usize)> {
        let card = self.focus_card.as_deref()?;
        let index = self.focus?.checked_sub(self.base)?;
        let start = *self.starts.get(index)?;
        let (first, last) = doc.card_rows(index, self.layout_cols, card)?;
        Some((start + first, start + last))
    }

    fn selected_text(&self, selection: &Selection, doc: &dyn Document) -> String {
        let ((r0, _), (r1, _)) = selection.ordered();
        let width = content_width(self.layout_cols);
        let mut rendered: HashMap<usize, Vec<String>> = HashMap::new();
        let mut lines = Vec::new();
        for index in r0..=r1 {
            let row = match self.row(index) {
                Some(row) => row.to_owned(),
                None => {
                    let block = self.starts.partition_point(|&s| s <= index) - 1;
                    rendered
                        .entry(block)
                        .or_insert_with(|| self.layout(doc, block, width))
                        .get(index - self.starts[block])
                        .cloned()
                        .unwrap_or_default()
                }
            };
            let plain = strip_escapes(&row);
            let (start, end) = selection
                .columns(index, text_inset(&plain))
                .unwrap_or((0, 0));
            lines.push(columns_of(&plain, start, end).trim_end().to_owned());
        }
        lines.join("\n")
    }
}

/// The link under display column `column` of `row`: the OSC 8 target there,
/// else a bare `http(s)://` URL in the visible text.
fn link_at(row: &str, column: usize) -> Option<String> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut target: Option<&str> = None;
    let mut at = 0;
    let mut rest = row;
    while !rest.is_empty() {
        if let Some(body) = rest.strip_prefix("\x1b]8;") {
            // OSC 8: params `;` target, ended by BEL or ST.
            let (end, skip) = match (body.find('\x07'), body.find("\x1b\\")) {
                (Some(b), Some(s)) if s < b => (s, 2),
                (Some(b), _) => (b, 1),
                (None, Some(s)) => (s, 2),
                (None, None) => break,
            };
            let url = body[..end].split_once(';').map_or("", |(_, url)| url);
            target = (!url.is_empty()).then_some(url);
            rest = &body[end + skip..];
            continue;
        }
        if rest.starts_with('\x1b') {
            let (piece, _) = clip(rest, 0);
            rest = &rest[piece.len().max(1)..];
            continue;
        }
        let next = rest.find('\x1b').unwrap_or(rest.len());
        for cluster in rest[..next].graphemes(true) {
            let width = display_width(cluster);
            if column < at + width.max(1) {
                if let Some(url) = target {
                    return Some(url.to_owned());
                }
                return bare_url(&strip_escapes(row), column);
            }
            at += width;
        }
        rest = &rest[next..];
    }
    None
}

/// The `http(s)://` word under display column `column` of plain `text`,
/// without the punctuation that usually follows a URL in prose.
fn bare_url(text: &str, column: usize) -> Option<String> {
    let mut at = 0;
    for word in text.split(' ') {
        let width = display_width(word);
        if column < at + width {
            let url = word.trim_end_matches(['.', ',', ';', ':', ')', ']', '>', '"', '\'']);
            let url = url.trim_start_matches(['(', '[', '<', '"', '\'']);
            let lower = url.to_ascii_lowercase();
            return (lower.starts_with("https://") || lower.starts_with("http://"))
                .then(|| url.to_owned());
        }
        at += width + 1;
    }
    None
}

/// Transcript rows are laid out one column short of the screen: the last
/// column belongs to the scrollbar.
fn content_width(cols: usize) -> usize {
    cols.saturating_sub(1).max(1)
}

/// `row` cut into screen rows of at most `width` columns.
fn wrap_row(row: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = row;
    loop {
        let (piece, _) = clip(rest, width);
        if piece.len() >= rest.len() {
            out.push(piece);
            return out;
        }
        if piece.is_empty() {
            let next = rest.chars().next().map_or(rest.len(), char::len_utf8);
            out.push(rest[..next].to_owned());
            rest = &rest[next..];
        } else {
            rest = &rest[piece.len()..];
            out.push(piece);
        }
        if rest.is_empty() {
            return out;
        }
    }
}

/// The display columns `[start, end)` of the word under `column` of plain
/// `text`: a run of characters other than whitespace and
/// [`WORD_SEPARATORS`], or the single separator there; `None` on whitespace
/// and past the text.
fn word_at(text: &str, column: usize) -> Option<(usize, usize)> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut spans = Vec::new();
    let mut at = 0;
    for cluster in text.graphemes(true) {
        let cells = display_width(cluster);
        spans.push((at, at + cells, cluster));
        at += cells;
    }
    let is_word = |&(_, _, g): &(usize, usize, &str)| {
        !g.chars()
            .all(|c| c.is_whitespace() || WORD_SEPARATORS.contains(c))
    };
    let hit = spans
        .iter()
        .position(|&(start, end, _)| column >= start && column < end)?;
    let (start, end, cluster) = spans[hit];
    if !is_word(&spans[hit]) {
        return (!cluster.trim().is_empty()).then_some((start, end));
    }
    let first = spans[..hit]
        .iter()
        .rposition(|s| !is_word(s))
        .map_or(0, |i| i + 1);
    let last = spans[hit..]
        .iter()
        .position(|s| !is_word(s))
        .map_or(spans.len(), |i| hit + i);
    Some((spans[first].0, spans[last - 1].1))
}

/// The display columns `[start, end)` of plain `text`.
fn columns_of(text: &str, start: usize, end: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = String::new();
    let mut column = 0;
    for cluster in text.graphemes(true) {
        let width = display_width(cluster);
        if column >= end {
            break;
        }
        if column + width > start {
            out.push_str(cluster);
        }
        column += width;
    }
    out
}

/// `row` with the graphemes overlapping display columns `[start, end)` in
/// one reverse-video band, padded to `end` within `width`; the rest keeps
/// its styling. A wide character cut by either edge is taken whole.
fn highlight(row: &str, (start, end): (usize, usize), width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    const BAND: &str = "\x1b[0;7m";
    let (row, _) = clip(row, width);
    let mut out = String::with_capacity(row.len() + 16);
    let mut style = String::new();
    let mut column = 0;
    let mut inside = false;
    let mut rest = row.as_str();
    while !rest.is_empty() {
        let len = escape_len(rest);
        if len > 0 {
            let escape = &rest[..len];
            match escape
                .strip_prefix("\x1b[")
                .and_then(|e| e.strip_suffix('m'))
            {
                Some(params) => {
                    if matches!(params.split(';').next(), Some("" | "0")) {
                        style.clear();
                    }
                    style.push_str(escape);
                    if !inside {
                        out.push_str(escape);
                    }
                }
                None => out.push_str(escape),
            }
            rest = &rest[len..];
            continue;
        }
        let text = rest.find('\x1b').unwrap_or(rest.len());
        for cluster in rest[..text].graphemes(true) {
            let cells = display_width(cluster);
            let selected = column < end && column + cells.max(1) > start;
            if selected != inside {
                inside = selected;
                if selected {
                    out.push_str(BAND);
                } else {
                    out.push_str("\x1b[0m");
                    out.push_str(&style);
                }
            }
            out.push_str(cluster);
            column += cells;
        }
        rest = &rest[text..];
    }
    let pad_to = end.min(width);
    if pad_to > column.max(start) {
        if !inside {
            out.push_str(&" ".repeat(start.saturating_sub(column)));
            out.push_str(BAND);
        }
        out.push_str(&" ".repeat(pad_to - column.max(start)));
        inside = true;
    }
    if inside {
        out.push_str("\x1b[0m");
    }
    out
}

/// Row `offset` of the picked message's box, `last` being its bottom
/// border. The box spans the composer's columns; its left border takes the
/// message's gutter mark, so the text does not move.
fn focus_row(row: &str, offset: usize, last: usize, cols: usize, p: &Palette) -> String {
    let outer = cols.saturating_sub(3).max(4);
    let inner = outer - 2;
    let edge = |s: &str| fg(s, &p.accent);
    if offset == 0 {
        return format!(" {}", edge(&format!("┏{}┓", "━".repeat(inner))));
    }
    if offset == last {
        let hint = PICK_HINTS
            .iter()
            .map(|hint| crate::ui::i18n::t(hint))
            .find(|hint| inner > display_width(hint) + 1)
            .unwrap_or("");
        let fill = inner.saturating_sub(1 + display_width(hint));
        return format!(
            " {}{}{}",
            edge("┗━"),
            fg(hint, &p.text),
            edge(&format!("{}┛", "━".repeat(fill)))
        );
    }
    let text = columns_of(&strip_escapes(row), 2, 1 + outer - 1);
    let pad = inner.saturating_sub(display_width(&text));
    format!(
        " {}{}{}{}",
        edge("┃"),
        fg(&text, &p.text),
        " ".repeat(pad),
        edge("┃")
    )
}

/// A row of the picked member card: an accent bar in its margin column,
/// the row itself unchanged.
fn card_focus_row(row: &str, width: usize, p: &Palette) -> String {
    let rest = row.strip_prefix(' ').unwrap_or(row);
    format!(
        "{}{}",
        fg("▌", &p.accent),
        clip(rest, width.saturating_sub(1)).0
    )
}

/// What a pick can stop on in block `index`: a user message, or each of
/// its member cards.
fn stops(doc: &dyn Document, index: usize) -> Vec<Option<String>> {
    if doc.message(index).is_some() {
        vec![None]
    } else {
        doc.cards(index).into_iter().map(Some).collect()
    }
}

/// Where an overlay's top-left cell goes: centred across the screen, and
/// in the transcript viewport when it fits there.
/// `row` with the faint attribute held through every SGR it sets: behind an
/// overlay the transcript keeps its colours, darkened.
fn faint(row: &str) -> String {
    let mut out = String::from("\x1b[2m");
    let mut rest = row;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find(|c: char| ('\u{40}'..='\u{7e}').contains(&c)) else {
            rest = &rest[start..];
            break;
        };
        let params = &after[..end];
        match &after[end..end + 1] {
            "m" if params.is_empty() => out.push_str("\x1b[0;2m"),
            "m" => out.push_str(&format!("\x1b[{params};2m")),
            _ => out.push_str(&rest[start..start + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn overlay_origin(
    overlay: &crate::ui::render::Overlay,
    cols: usize,
    rows: usize,
    view: usize,
) -> (usize, usize) {
    let width = overlay.rows.first().map_or(0, |r| clip(r, usize::MAX).1);
    let height = overlay.rows.len();
    let x = cols.saturating_sub(width) / 2;
    let y = if height <= view {
        (view - height) / 2
    } else {
        rows.saturating_sub(height) / 2
    };
    (x, y)
}

/// `base` with `row` laid over it from column `x`; what shows of `base`
/// around it is plain, dimmed text.
fn composite(base: &str, row: &str, x: usize, cols: usize) -> String {
    let plain = strip_escapes(base);
    let width = clip(row, usize::MAX).1;
    format!(
        "\x1b[2m{}\x1b[0m{row}\x1b[0m\x1b[2m{}",
        cells(&plain, 0, x),
        cells(&plain, x + width, cols)
    )
}

/// Exactly the cells `[start, end)` of plain `text`: a wide character cut
/// by either edge, and cells past the end, become blanks. An empty range
/// (an overlay as wide as the screen leaves nothing right of it) is "".
fn cells(text: &str, start: usize, end: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if start >= end {
        return String::new();
    }
    let mut out = String::new();
    let mut column = 0;
    for cluster in text.graphemes(true) {
        let width = display_width(cluster);
        if column >= end {
            break;
        }
        if column >= start && column + width <= end {
            out.push_str(cluster);
        } else if column + width > start {
            out.push_str(&" ".repeat((column + width).min(end) - column.max(start)));
        }
        column += width;
    }
    if column < end {
        out.push_str(&" ".repeat(end - column.max(start)));
    }
    out
}

/// The scrollbar column for a viewport of `view` rows over `content` rows
/// starting at `top`, one cell per row, in eighths of a row; `None` when
/// everything fits.
fn scrollbar(view: usize, content: usize, top: usize, palette: &Palette) -> Option<Vec<String>> {
    if content <= view || view == 0 {
        return None;
    }
    let track = &palette.scroll_track;
    let thumb = &palette.scroll_thumb;
    let total = view * 8;
    let size = (total * view / content).max(8);
    let start = (total - size) * top / (content - view);
    let end = start + size;
    let cell = |text: &str, fg: &str, bg: &str| {
        paint(
            text,
            Paint {
                fg: Some(fg),
                bg: Some(bg),
                ..Paint::default()
            },
        )
    };
    Some(
        (0..view)
            .map(|y| {
                let (lo, hi) = (y * 8, y * 8 + 8);
                let covered = end.min(hi).saturating_sub(start.max(lo));
                if covered == 0 {
                    cell(" ", thumb, track)
                } else if covered == 8 {
                    cell(EIGHTHS[8], thumb, track)
                } else if start > lo {
                    cell(EIGHTHS[covered], thumb, track)
                } else {
                    cell(EIGHTHS[8 - covered], track, thumb)
                }
            })
            .collect(),
    )
}

/// CUP with 0-based (column, row).
fn push_move(out: &mut Vec<u8>, column: u16, row: u16) {
    let _ = write!(out, "\x1b[{};{}H", row + 1, column + 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render::Overlay;
    use crate::ui::render::testterm::Emu;
    use crate::ui::theme::{Appearance, ColorMode, palette::terminal_palette};

    const SIZE: (u16, u16) = (30, 8);

    /// Blocks as the App commits them: a blank row, then the text rows.
    struct Doc {
        blocks: Vec<(bool, Vec<String>)>,
        dropped: usize,
        /// Blocks laid out so far.
        laid: std::cell::Cell<usize>,
    }

    impl Doc {
        fn new() -> Self {
            Self {
                blocks: Vec::new(),
                dropped: 0,
                laid: std::cell::Cell::new(0),
            }
        }

        fn say(&mut self, rows: &[&str]) {
            self.blocks
                .push((false, rows.iter().map(|r| r.to_string()).collect()));
        }

        fn user(&mut self, text: &str) {
            self.blocks.push((true, vec![format!(" › {text}")]));
        }
    }

    impl Document for Doc {
        fn dropped(&self) -> usize {
            self.dropped
        }
        fn len(&self) -> usize {
            self.blocks.len()
        }
        fn rows(&self, index: usize, _cols: usize) -> Vec<String> {
            self.laid.set(self.laid.get() + 1);
            std::iter::once(String::new())
                .chain(self.blocks[index].1.iter().cloned())
                .collect()
        }
        /// A blank row and one more: short of every taller block.
        fn estimate(&self, _index: usize, _cols: usize) -> usize {
            2
        }
        fn rewritten(&self, _since: u64) -> (u64, Option<Vec<usize>>) {
            (0, Some(Vec::new()))
        }
        fn cards(&self, _index: usize) -> Vec<String> {
            Vec::new()
        }
        fn card_rows(&self, _index: usize, _cols: usize, _card: &str) -> Option<(usize, usize)> {
            None
        }
        fn card_at(&self, _index: usize, _cols: usize, _row: usize) -> Option<String> {
            None
        }
        fn message(&self, index: usize) -> Option<&str> {
            let (user, rows) = &self.blocks[index];
            user.then(|| rows[0].trim_start_matches(" › "))
        }
        fn cut(&self, index: usize) -> Option<u64> {
            self.blocks[index].0.then_some(index as u64)
        }
    }

    fn frame(tail: &[&str], dock: &[&str]) -> Frame {
        let mut live: Vec<String> = tail.iter().map(|s| s.to_string()).collect();
        live.extend(dock.iter().map(|s| s.to_string()));
        Frame {
            cursor: Some((tail.len() as u16, 2)),
            live,
            dock: tail.len(),
            ..Frame::default()
        }
    }

    fn draw(screen: &mut FullScreen, emu: &mut Emu, f: &Frame, doc: &Doc) -> Vec<u8> {
        let palette = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let bytes = screen.render(f, SIZE, &palette, doc);
        emu.feed(&bytes);
        bytes
    }

    fn setup() -> (FullScreen, Emu) {
        (
            FullScreen::new(SIZE, false),
            Emu::new(SIZE.0 as usize, SIZE.1 as usize),
        )
    }

    const DOCK: [&str; 2] = ["> x", "status"];

    #[test]
    fn a_short_transcript_sits_on_the_dock_and_a_shrinking_tail_moves_nothing() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        doc.say(&["a", "b"]);
        draw(&mut screen, &mut emu, &frame(&["tail"], &DOCK), &doc);
        let rows = emu.rows_text();
        assert_eq!(&rows[..6], &["", "", "", "a", "b", "tail"]);
        assert_eq!(&rows[6..], &DOCK);
        assert_eq!((emu.cy, emu.cx), (6, 2));
        assert!(draw(&mut screen, &mut emu, &frame(&["tail"], &DOCK), &doc).is_empty());

        doc.say(&["c", "d"]);
        let view = |emu: &Emu| -> Vec<String> {
            (0..6)
                .map(|y| {
                    emu.row(y)
                        .chars()
                        .take(20)
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect()
        };
        let tall = ["t1", "t2", "t3", "t4"];
        draw(&mut screen, &mut emu, &frame(&tall, &DOCK), &doc);
        assert_eq!(view(&emu), ["c", "d", "t1", "t2", "t3", "t4"]);
        draw(&mut screen, &mut emu, &frame(&["t1"], &DOCK), &doc);
        assert_eq!(view(&emu), ["c", "d", "t1", "", "", ""]);
        screen.follow();
        draw(&mut screen, &mut emu, &frame(&["t1"], &DOCK), &doc);
        assert_eq!(view(&emu)[5], "t1");
    }

    #[test]
    fn overflow_follows_the_bottom_until_scrolled_and_keeps_the_view_pinned() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        for i in 0..10 {
            doc.say(&[&format!("row {i}")]);
        }
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(5).starts_with("row 9"));
        assert!(emu.row(5).ends_with('█'));
        screen.page(false);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(5).starts_with("row 7"));
        doc.say(&["more"]);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(5).starts_with("row 7"));
        screen.follow();
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(5).starts_with("more "));
        assert_eq!(&emu.rows_text()[6..], &DOCK);
    }

    #[test]
    fn only_blocks_near_the_viewport_keep_rendered_rows() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        for i in 0..2_000 {
            doc.say(&[&format!("row {i}")]);
        }
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(screen.cache.len() <= 12, "{}", screen.cache.len());
        screen.scroll(-100_000);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(1).starts_with("row 0"));
        assert!(screen.cache.len() <= 12, "{}", screen.cache.len());
        screen.scroll(40);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(1).starts_with("row 20"), "{:?}", emu.rows_text());
        doc.blocks.drain(..10);
        doc.dropped = 10;
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(1).starts_with("row 20"), "{:?}", emu.rows_text());
    }

    #[test]
    fn a_long_transcript_is_laid_out_near_the_view_only_and_keeps_its_place() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        for i in 0..2_000 {
            doc.say(&[&format!("row {i}"), "detail"]);
        }
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(4).starts_with("row 1999"), "{:?}", emu.rows_text());
        assert!(emu.row(5).starts_with("detail"));
        assert!(doc.laid.get() < 20, "{}", doc.laid.get());
        screen.scroll(-100_000);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(1).starts_with("row 0"), "{:?}", emu.rows_text());
        for _ in 0..10 {
            screen.scroll(WHEEL_ROWS);
            draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        }
        assert!(emu.row(1).starts_with("row 10"), "{:?}", emu.rows_text());
        doc.laid.set(0);
        screen.relayout(SIZE.0);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert!(emu.row(1).starts_with("row 10"), "{:?}", emu.rows_text());
        assert!(doc.laid.get() < 20, "{}", doc.laid.get());
    }

    #[test]
    fn a_drag_selects_whole_graphemes_past_the_gutter_and_stays_until_the_next_press() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        doc.say(&[" ◆\x1b[1mhello\x1b[0m world", " ◆分析仓库 ok"]);
        draw(&mut screen, &mut emu, &frame(&[], &["> x"]), &doc);
        let before = emu.rows_text();
        let at = |kind, column, row| MouseEvent { kind, column, row };
        assert_eq!(
            screen.mouse(at(MouseKind::Press, 8, 5), &doc),
            MouseOutcome::Ignored
        );
        assert!(draw(&mut screen, &mut emu, &frame(&[], &["> x"]), &doc).is_empty());
        assert_eq!(
            screen.mouse(at(MouseKind::Drag, 3, 6), &doc),
            MouseOutcome::Redraw
        );
        let bytes = draw(&mut screen, &mut emu, &frame(&[], &["> x"]), &doc);
        assert_eq!(emu.rows_text(), before);
        let bytes = String::from_utf8(bytes).unwrap();
        assert!(
            bytes.contains("\x1b[1mhello\x1b[0m \x1b[0;7mworld"),
            "{bytes:?}"
        );
        assert!(bytes.contains("◆\x1b[0;7m分\x1b[0m析"), "{bytes:?}");
        assert_eq!(
            screen.mouse(at(MouseKind::Release, 3, 6), &doc),
            MouseOutcome::Copy("world\n分".to_owned())
        );
        assert!(screen.selecting());
        assert_eq!(
            screen.mouse(at(MouseKind::Press, 4, 5), &doc),
            MouseOutcome::Redraw
        );
        assert!(!screen.selecting());
        draw(&mut screen, &mut emu, &frame(&[], &["> x"]), &doc);
        assert_eq!(emu.rows_text(), before);
    }

    #[test]
    fn selection_on_a_wide_screen_keeps_the_first_column_of_text() {
        let size = (80, 12);
        let mut screen = FullScreen::new(size, false);
        let mut emu = Emu::new(size.0 as usize, size.1 as usize);
        let mut doc = Doc::new();
        let code = format!("{}code", " ".repeat(5));
        doc.say(&[
            " - if only darwin",
            " n missing-package",
            &code,
            " \u{2503} user text",
            " 关键在",
        ]);
        let palette = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let dock = ["> x"];
        emu.feed(&screen.render(&frame(&[], &dock), size, &palette, &doc));
        let at = |kind, column, row| MouseEvent { kind, column, row };
        screen.mouse(at(MouseKind::Press, 0, 6), &doc);
        screen.mouse(at(MouseKind::Drag, 20, 10), &doc);
        assert_eq!(
            screen.mouse(at(MouseKind::Release, 20, 10), &doc),
            MouseOutcome::Copy(
                "- if only darwin\nn missing-package\n    code\n user text\n关键在".to_owned()
            )
        );
        let mut click = |column, row| {
            screen.mouse(at(MouseKind::Press, column, row), &doc);
            screen.mouse(at(MouseKind::Release, column, row), &doc)
        };
        click(1, 7);
        assert_eq!(click(1, 7), MouseOutcome::Copy("n".to_owned()));
        assert_eq!(
            click(1, 7),
            MouseOutcome::Copy("n missing-package".to_owned())
        );
    }

    #[test]
    fn selection_on_a_wide_screen_keeps_ambiguous_marks() {
        let size = (80, 13);
        let mut screen = FullScreen::new(size, false);
        let mut emu = Emu::new(size.0 as usize, size.1 as usize);
        let mut doc = Doc::new();
        doc.say(&[
            " \u{25c6} Thought for 3.2s",
            " \u{25a0} something failed",
            " \u{25e6} status note",
            " $ cargo test",
            " $10 kept",
            " ✓ installed",
            " - kept",
        ]);
        let palette = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let dock = ["> x"];
        emu.feed(&screen.render(&frame(&[], &dock), size, &palette, &doc));
        let at = |kind, column, row| MouseEvent { kind, column, row };
        screen.mouse(at(MouseKind::Press, 0, 5), &doc);
        screen.mouse(at(MouseKind::Drag, 24, 11), &doc);
        assert_eq!(
            screen.mouse(at(MouseKind::Release, 24, 11), &doc),
            MouseOutcome::Copy(
                " Thought for 3.2s\n something failed\n status note\n$ cargo test\n$10 kept\n✓ installed\n- kept"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_double_click_copies_the_word_and_a_triple_click_the_row() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        doc.say(&[" ◆see foo-bar.rs now", " ◆分析仓库, ok"]);
        draw(&mut screen, &mut emu, &frame(&[], &["> x"]), &doc);
        let at = |kind, column, row| MouseEvent { kind, column, row };
        let mut click = |column, row| {
            screen.mouse(at(MouseKind::Press, column, row), &doc);
            screen.mouse(at(MouseKind::Release, column, row), &doc)
        };
        assert_eq!(
            click(11, 5),
            MouseOutcome::Click {
                block: Some(0),
                row: 1
            }
        );
        assert_eq!(click(11, 5), MouseOutcome::Copy("bar".to_owned()));
        assert_eq!(
            click(11, 5),
            MouseOutcome::Copy("see foo-bar.rs now".to_owned())
        );
        click(5, 6);
        assert_eq!(click(5, 6), MouseOutcome::Copy("分析仓库".to_owned()));
    }

    #[test]
    fn tab_boxes_user_messages_on_their_blank_rows() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        doc.user("first");
        doc.user("second");
        doc.say(&["reply"]);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        let before = emu.rows_text();
        screen.focus_older(&doc);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        let rows = emu.rows_text();
        assert_eq!(rows[1], before[1]);
        assert!(rows[2].starts_with(" ┏━"), "{rows:?}");
        assert!(rows[3].starts_with(" ┃ second"), "{rows:?}");
        assert!(rows[4].starts_with(" ┗━e edit━"), "{rows:?}");
        assert_eq!(rows[5], before[5]);
        assert_eq!(screen.focused_text(&doc).as_deref(), Some("second"));
        assert_eq!(screen.focused_cut(&doc), Some(1));
        screen.focus_older(&doc);
        assert_eq!(screen.focused_text(&doc).as_deref(), Some("first"));
        screen.focus_older(&doc);
        assert_eq!(screen.focused_text(&doc).as_deref(), Some("first"));
        screen.focus_newer(&doc);
        screen.focus_newer(&doc);
        assert!(!screen.focused());
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert_eq!(emu.rows_text(), before);
    }

    #[test]
    fn an_overlay_is_centred_over_the_dimmed_transcript_and_owns_the_wheel() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        doc.say(&["", "", "", "", "behind the box"]);
        let mut f = frame(&[], &DOCK);
        f.overlay = Some(Overlay {
            rows: vec!["┌──┐".into(), "│> │".into(), "└──┘".into()],
            cursor: Some((1, 2)),
        });
        let bytes = draw(&mut screen, &mut emu, &f, &doc);
        let rows = emu.rows_text();
        assert_eq!(
            &rows[1..4],
            &[
                "             ┌──┐",
                "             │> │",
                "             └──┘"
            ]
        );
        assert_eq!(rows[5], "behind the box");
        assert_eq!((emu.cy, emu.cx), (2, 15));
        assert!(String::from_utf8_lossy(&bytes).contains("\x1b[2m"));
        let wheel = MouseEvent {
            kind: MouseKind::WheelDown,
            column: 0,
            row: 0,
        };
        assert_eq!(screen.mouse(wheel, &doc), MouseOutcome::Overlay(1));
    }

    #[test]
    fn the_wheel_speeds_up_with_the_streak_and_a_trackpad_goes_fine() {
        let mut wheel = Wheel::default();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        assert_eq!(wheel.rows(at(0)), WHEEL_ROWS);
        assert_eq!(wheel.rows(at(1)), WHEEL_ROWS);
        assert_eq!(wheel.rows(at(200)), WHEEL_ROWS);
        let spin: Vec<isize> = (1..=4).map(|i| wheel.rows(at(200 + 20 * i))).collect();
        assert_eq!(spin, [9, 9, 9, 9]);
        assert_eq!(wheel.rows(at(1_000)), WHEEL_ROWS);
        let rows: Vec<isize> = (0..6).map(|i| wheel.rows(at(2_000 + 2 * i))).collect();
        assert_eq!(rows, [3, 3, 3, 3, 1, 1]);
        assert_eq!(wheel.rows(at(3_000)), 1);
    }

    /// A repaint must assume the cursor is visible: the alternate screen may
    /// have been entered with it shown, so a frame with `cursor: None` alone
    /// cannot prove it is hidden.
    #[test]
    fn invalidate_shows_the_cursor_again() {
        let (mut screen, _) = setup();
        screen.cursor_visible = false;
        screen.invalidate();
        assert!(screen.cursor_visible);
    }

    /// A panel opening in the dock pushes the followed transcript up; when
    /// it closes, the transcript comes back down onto the composer. A
    /// transcript shorter than the view stays on the dock as well.
    #[test]
    fn a_dock_that_grows_or_shrinks_keeps_the_transcript_on_it() {
        let tall = ["panel", "panel", "> x", "status"];
        for blocks in [1, 4] {
            let (mut screen, mut emu) = setup();
            let mut doc = Doc::new();
            for n in 0..blocks {
                doc.say(&[&format!("line {n}")]);
            }
            let last = format!("line {}", blocks - 1);
            for dock in [&DOCK[..], &tall, &DOCK] {
                draw(&mut screen, &mut emu, &frame(&[], dock), &doc);
                let rows = emu.rows_text();
                let above = rows[SIZE.1 as usize - dock.len() - 1]
                    .trim_end_matches(|c: char| c == ' ' || "▁▂▃▄▅▆▇█".contains(c));
                assert_eq!(above, last, "{blocks} blocks, dock {}", dock.len());
            }
        }
    }

    /// Tool details collapsed: the transcript gets much shorter and the
    /// followed view must lay out from the bottom again, not keep the
    /// floor the tall layout reached (which left one row over blank space).
    #[test]
    fn a_relayout_that_shortens_the_transcript_fills_the_view_again() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        for n in 0..6 {
            let rows: Vec<String> = (0..4).map(|r| format!("block {n} row {r}")).collect();
            let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
            doc.say(&rows);
        }
        let text = |rows: &[String]| -> Vec<String> {
            rows[..6]
                .iter()
                .map(|row| row.trim_end_matches(|c: char| c == ' ' || "▁▂▃▄▅▆▇█".contains(c)))
                .map(str::to_owned)
                .collect()
        };
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        assert_eq!(text(&emu.rows_text())[5], "block 5 row 3");
        for block in &mut doc.blocks {
            block.1.truncate(1);
        }
        screen.relayout(SIZE.0);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        let rows = text(&emu.rows_text());
        assert_eq!(
            rows,
            [
                "",
                "block 3 row 0",
                "",
                "block 4 row 0",
                "",
                "block 5 row 0"
            ],
        );
    }

    #[test]
    fn a_click_lands_where_it_was_pressed_unless_the_row_moved_meanwhile() {
        let (mut screen, mut emu) = setup();
        let doc = Doc::new();
        let at = |kind| MouseEvent {
            kind,
            column: 1,
            row: 5,
        };
        draw(&mut screen, &mut emu, &frame(&["· Running ▸"], &DOCK), &doc);
        screen.mouse(at(MouseKind::Press), &doc);
        assert_eq!(
            screen.mouse(at(MouseKind::Release), &doc),
            MouseOutcome::Click {
                block: None,
                row: 0
            }
        );
        screen.mouse(at(MouseKind::Press), &doc);
        draw(
            &mut screen,
            &mut emu,
            &frame(&["◆ new answer"], &DOCK),
            &doc,
        );
        assert_eq!(
            screen.mouse(at(MouseKind::Release), &doc),
            MouseOutcome::Ignored
        );
    }

    #[test]
    fn a_click_on_a_link_opens_its_target() {
        let (mut screen, mut emu) = setup();
        let mut doc = Doc::new();
        let link = crate::ui::render::osc8::hyperlink("https://a.example/x", "docs");
        doc.say(&[&format!("see {link} or"), "(https://b.example/y)."]);
        draw(&mut screen, &mut emu, &frame(&[], &DOCK), &doc);
        let mut click = |column, row| {
            let at = |kind| MouseEvent { kind, column, row };
            screen.mouse(at(MouseKind::Press), &doc);
            screen.mouse(at(MouseKind::Release), &doc)
        };
        assert_eq!(
            click(5, 4),
            MouseOutcome::Open("https://a.example/x".into())
        );
        assert_eq!(
            click(1, 4),
            MouseOutcome::Click {
                block: Some(0),
                row: 1
            }
        );
        assert_eq!(
            click(8, 5),
            MouseOutcome::Open("https://b.example/y".into())
        );
    }

    #[test]
    fn scrollbar_thumb_tracks_the_view_in_eighths() {
        let palette = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let plain = |bar: Vec<String>| -> String { bar.iter().map(|c| strip_escapes(c)).collect() };
        assert!(scrollbar(4, 4, 0, &palette).is_none());
        assert_eq!(plain(scrollbar(4, 8, 0, &palette).unwrap()), "██  ");
        assert_eq!(plain(scrollbar(4, 8, 4, &palette).unwrap()), "  ██");
        assert_eq!(plain(scrollbar(4, 8, 1, &palette).unwrap()), "▄█▄ ");
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            if n == 0 { 0 } else { self.next() % n }
        }
        fn rows(&mut self, most: u64, width: u16) -> Vec<String> {
            (0..self.below(most + 1))
                .map(|n| {
                    format!(
                        "{n}{}",
                        "y".repeat(self.below(u64::from(width) * 2 + 2) as usize)
                    )
                })
                .collect()
        }
    }

    #[test]
    fn random_documents_frames_and_input_at_any_size_never_panic() {
        let palette = terminal_palette(ColorMode::TrueColor, Appearance::Dark);
        let seeds: u64 = std::env::var("DIM_STRESS_SEEDS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(300);
        for seed in 1..=seeds {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut size = (1 + rng.below(90) as u16, 1 + rng.below(40) as u16);
            let mut screen = FullScreen::new(size, rng.below(2) == 0);
            let mut doc = Doc::new();
            for _ in 0..40 {
                if rng.below(8) == 0 {
                    size = (1 + rng.below(90) as u16, 1 + rng.below(40) as u16);
                }
                match rng.below(14) {
                    0 => doc.user(&"u".repeat(rng.below(200) as usize)),
                    1 => {
                        let rows = rng.rows(3 * u64::from(size.1), size.0);
                        doc.blocks.push((false, rows));
                    }
                    2 if !doc.blocks.is_empty() => {
                        let drop = 1 + rng.below(doc.blocks.len() as u64) as usize;
                        doc.blocks.drain(..drop);
                        doc.dropped += drop;
                    }
                    3 => screen.scroll(rng.below(200) as isize - 100),
                    4 => screen.page(rng.below(2) == 0),
                    5 => screen.scroll_to_top(),
                    6 => screen.follow(),
                    7 => screen.focus_older(&doc),
                    8 => screen.focus_newer(&doc),
                    9 => {
                        let kinds = [
                            MouseKind::Press,
                            MouseKind::Drag,
                            MouseKind::Release,
                            MouseKind::WheelUp,
                            MouseKind::WheelDown,
                        ];
                        let event = MouseEvent {
                            kind: kinds[rng.below(5) as usize],
                            column: rng.below(u64::from(size.0) + 3) as u16,
                            row: rng.below(u64::from(size.1) + 3) as u16,
                        };
                        screen.mouse(event, &doc);
                    }
                    10 => screen.relayout(1 + rng.below(90) as u16),
                    _ => {
                        let mut live = rng.rows(u64::from(size.1) + 5, size.0);
                        let dock = rng.below(live.len() as u64 + 2) as usize;
                        if live.is_empty() {
                            live.push(String::new());
                        }
                        let overlay = (rng.below(4) == 0).then(|| Overlay {
                            rows: rng.rows(u64::from(size.1) + 4, size.0),
                            cursor: (rng.below(2) == 0).then(|| {
                                (
                                    rng.below(50) as u16,
                                    rng.below(u64::from(size.0) + 3) as u16,
                                )
                            }),
                        });
                        let cursor = (rng.below(3) > 0).then(|| {
                            (
                                rng.below(live.len() as u64 + 2) as u16,
                                rng.below(u64::from(size.0) + 2) as u16,
                            )
                        });
                        let frame = Frame {
                            commits: Vec::new(),
                            live,
                            cursor,
                            dock,
                            anchor: dock,
                            overlay,
                        };
                        screen.render(&frame, size, &palette, &doc);
                    }
                }
                let _ = screen.focused_text(&doc);
            }
            screen.leave();
        }
    }
}
