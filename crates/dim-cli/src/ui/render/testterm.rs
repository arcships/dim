//! A small VT model for tests: enough of xterm to check what the write path
//! leaves on screen and in scrollback, and what it sends while doing so.
//!
//! Supported: printable text with wide characters, CR/LF/BS/HT, autowrap with
//! the deferred-wrap state, CUP/CUU/CUD/CUF/CUB/CHA, EL, ED, SGR (ignored),
//! DECSET/DECRST 25 and 2026, OSC (skipped). Anything else is ignored.

use unicode_width::UnicodeWidthChar;

#[derive(Debug)]
pub struct Emu {
    pub cols: usize,
    pub rows: usize,
    pub screen: Vec<Vec<char>>,
    pub scrollback: Vec<String>,
    pub cx: usize,
    pub cy: usize,
    pub wrap_pending: bool,
    pub cursor_visible: bool,
    pub sync_depth: i32,
    pub sync_blocks: usize,
    /// Cursor visibility toggles seen.
    pub visibility_changes: usize,
    pub bytes: usize,
}

impl Emu {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            screen: vec![vec![' '; cols]; rows],
            scrollback: Vec::new(),
            cx: 0,
            cy: 0,
            wrap_pending: false,
            cursor_visible: true,
            sync_depth: 0,
            sync_blocks: 0,
            visibility_changes: 0,
            bytes: 0,
        }
    }

    pub fn row(&self, y: usize) -> String {
        self.screen[y]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    pub fn rows_text(&self) -> Vec<String> {
        (0..self.rows).map(|y| self.row(y)).collect()
    }

    /// Scrollback followed by the visible rows, trailing blanks trimmed.
    pub fn transcript(&self) -> Vec<String> {
        let mut all = self.scrollback.clone();
        all.extend(self.rows_text());
        while all.last().is_some_and(|l| l.is_empty()) {
            all.pop();
        }
        all
    }

    fn scroll_up(&mut self) {
        let top = self.screen.remove(0);
        self.scrollback
            .push(top.iter().collect::<String>().trim_end().to_owned());
        self.screen.push(vec![' '; self.cols]);
    }

    fn line_feed(&mut self) {
        if self.cy + 1 >= self.rows {
            self.scroll_up();
        } else {
            self.cy += 1;
        }
    }

    fn put(&mut self, c: char) {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            return;
        }
        if self.wrap_pending {
            self.cx = 0;
            self.line_feed();
            self.wrap_pending = false;
        }
        if self.cx + width > self.cols {
            self.cx = 0;
            self.line_feed();
        }
        self.screen[self.cy][self.cx] = c;
        if width == 2 {
            self.screen[self.cy][self.cx + 1] = '\u{0}';
        }
        if self.cx + width >= self.cols {
            self.cx = self.cols - 1;
            self.wrap_pending = true;
        } else {
            self.cx += width;
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.bytes += bytes.len();
        let text = String::from_utf8_lossy(bytes).into_owned();
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\x1b' => i = self.escape(&chars, i),
                '\r' => {
                    self.cx = 0;
                    self.wrap_pending = false;
                    i += 1;
                }
                '\n' => {
                    self.line_feed();
                    self.wrap_pending = false;
                    i += 1;
                }
                '\x08' => {
                    self.cx = self.cx.saturating_sub(1);
                    self.wrap_pending = false;
                    i += 1;
                }
                c if c.is_control() => i += 1,
                c => {
                    self.put(c);
                    i += 1;
                }
            }
        }
    }

    fn escape(&mut self, chars: &[char], start: usize) -> usize {
        match chars.get(start + 1) {
            Some('[') => {
                let mut end = start + 2;
                while end < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[end]) {
                    end += 1;
                }
                if end >= chars.len() {
                    return chars.len();
                }
                let params: String = chars[start + 2..end].iter().collect();
                self.csi(&params, chars[end]);
                end + 1
            }
            Some(']') | Some('P') => {
                let mut end = start + 2;
                while end < chars.len() {
                    if chars[end] == '\x07' {
                        return end + 1;
                    }
                    if chars[end] == '\x1b' && chars.get(end + 1) == Some(&'\\') {
                        return end + 2;
                    }
                    end += 1;
                }
                chars.len()
            }
            _ => start + 2,
        }
    }

    fn csi(&mut self, params: &str, final_byte: char) {
        let private = params.starts_with('?');
        let nums: Vec<usize> = params
            .trim_start_matches('?')
            .split(';')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let first = |default: usize| match nums.first() {
            Some(&n) if n > 0 => n,
            _ => default,
        };
        match (private, final_byte) {
            (false, 'H') | (false, 'f') => {
                let row = first(1);
                let col = nums.get(1).copied().filter(|n| *n > 0).unwrap_or(1);
                self.cy = (row - 1).min(self.rows - 1);
                self.cx = (col - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            (false, 'A') => self.cy = self.cy.saturating_sub(first(1)),
            (false, 'B') => self.cy = (self.cy + first(1)).min(self.rows - 1),
            (false, 'C') => {
                self.cx = (self.cx + first(1)).min(self.cols - 1);
                self.wrap_pending = false;
            }
            (false, 'D') => {
                self.cx = self.cx.saturating_sub(first(1));
                self.wrap_pending = false;
            }
            (false, 'G') => {
                self.cx = (first(1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            (false, 'K') => {
                let (from, to) = match nums.first().copied().unwrap_or(0) {
                    1 => (0, self.cx + 1),
                    2 => (0, self.cols),
                    _ => (self.cx, self.cols),
                };
                for x in from..to {
                    self.screen[self.cy][x] = ' ';
                }
            }
            (false, 'J') => {
                let mode = nums.first().copied().unwrap_or(0);
                let rows: Vec<usize> = match mode {
                    // E3: the scrollback only.
                    3 => {
                        self.scrollback.clear();
                        Vec::new()
                    }
                    2 => (0..self.rows).collect(),
                    1 => (0..self.cy).collect(),
                    _ => (self.cy + 1..self.rows).collect(),
                };
                for y in rows {
                    self.screen[y] = vec![' '; self.cols];
                }
                if mode == 0 {
                    for x in self.cx..self.cols {
                        self.screen[self.cy][x] = ' ';
                    }
                }
            }
            (true, 'h') | (true, 'l') => {
                let on = final_byte == 'h';
                match nums.first().copied().unwrap_or(0) {
                    25 => {
                        if self.cursor_visible != on {
                            self.visibility_changes += 1;
                        }
                        self.cursor_visible = on;
                    }
                    2026 => {
                        if on {
                            self.sync_depth += 1;
                            self.sync_blocks += 1;
                        } else {
                            self.sync_depth -= 1;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
