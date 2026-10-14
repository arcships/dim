//! Terminal input decoding.
//!
//! The decoder turns the raw byte stream into key and paste events. It owns
//! three jobs the line reader cannot do: reassemble escape sequences that
//! arrive split across reads, swallow terminal replies (DA1/DA2, XTVERSION,
//! kitty flag reports, focus, cursor position) so they never reach the
//! composer as text, and understand the three key encodings a terminal may
//! use (legacy, kitty `CSI u`, xterm modifyOtherKeys).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    F(u8),
}

/// Modifier bit set: shift 1, alt 2, ctrl 4, super 8.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const CTRL: Self = Self(4);
    pub const SUPER: Self = Self(8);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }

    fn from_wire(value: u32) -> Self {
        Self((value.saturating_sub(1) & 0x0f) as u8)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Modifiers,
}

impl KeyEvent {
    pub const fn new(code: KeyCode, mods: Modifiers) -> Self {
        Self { code, mods }
    }

    pub fn is_ctrl(self, c: char) -> bool {
        self.mods.contains(Modifiers::CTRL) && self.code == KeyCode::Char(c)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    /// A bracketed paste, line endings normalized to `\n`.
    Paste(String),
    /// A mouse report; only the fullscreen interface turns them on.
    Mouse(MouseEvent),
}

/// What the mouse did. Only the left button selects; other buttons are
/// dropped by the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Drag,
    Release,
    WheelUp,
    WheelDown,
}

/// A mouse report at a 0-based screen cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
}

/// Upper bound for one unfinished escape sequence; garbage beyond it is
/// dropped instead of growing without limit.
const MAX_PENDING: usize = 8 * 1024;
/// Upper bound for one bracketed paste.
const MAX_PASTE: usize = 16 * 1024 * 1024;

const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Default)]
pub struct Decoder {
    pending: Vec<u8>,
    paste: Option<Vec<u8>>,
    /// The paste outgrew `MAX_PASTE`: its text was delivered, and the rest is
    /// discarded up to the end marker instead of being read as keys.
    overflowing: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while bytes of an unfinished sequence are buffered; the caller
    /// uses it to decide whether a read timeout means "lone Esc".
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty() && self.paste.is_none()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        let mut events = Vec::new();
        if let Some(paste) = &mut self.paste {
            paste.extend_from_slice(bytes);
            if self.overflowing {
                if let Some(end) = find(paste, PASTE_END) {
                    let tail = paste.split_off(end + PASTE_END.len());
                    self.paste = None;
                    self.overflowing = false;
                    self.pending = tail;
                    self.drain(&mut events);
                } else {
                    // Only the last bytes can start the end marker.
                    let keep = PASTE_END.len() - 1;
                    if paste.len() > keep {
                        paste.drain(..paste.len() - keep);
                    }
                }
                return events;
            }
            if let Some(end) = find(paste, PASTE_END) {
                let tail = paste.split_off(end + PASTE_END.len());
                paste.truncate(end);
                let text = String::from_utf8_lossy(paste)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                self.paste = None;
                events.push(InputEvent::Paste(text));
                // Whatever followed the paste end is ordinary input again.
                self.pending = tail;
                self.drain(&mut events);
            } else if paste.len() > MAX_PASTE {
                // Unterminated flood: deliver what we have rather than hold it,
                // and keep discarding until the end marker arrives.
                let text = String::from_utf8_lossy(paste)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                events.push(InputEvent::Paste(text));
                self.overflowing = true;
                let keep = PASTE_END.len() - 1;
                paste.drain(..paste.len() - keep);
            }
            return events;
        }
        self.pending.extend_from_slice(bytes);
        self.drain(&mut events);
        events
    }

    /// The read timed out with an escape prefix still buffered: a lone ESC is
    /// the Escape key, an incomplete sequence is dropped.
    pub fn flush_timeout(&mut self) -> Vec<InputEvent> {
        let pending = std::mem::take(&mut self.pending);
        match pending.as_slice() {
            [0x1b] => vec![InputEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                Modifiers::NONE,
            ))],
            [0x1b, rest @ ..]
                if !rest.starts_with(b"[")
                    && !rest.starts_with(b"O")
                    && !rest.starts_with(b"P")
                    && !rest.starts_with(b"]") =>
            {
                // ESC + a partial UTF-8 char is not worth reporting.
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn drain(&mut self, events: &mut Vec<InputEvent>) {
        loop {
            if self.pending.is_empty() {
                return;
            }
            match parse_one(&self.pending) {
                Parse::Incomplete => {
                    if self.pending.len() > MAX_PENDING {
                        self.pending.clear();
                    }
                    return;
                }
                Parse::Consumed(len, outcome) => {
                    self.pending.drain(..len);
                    match outcome {
                        Outcome::Key(key) => events.push(InputEvent::Key(key)),
                        Outcome::Mouse(mouse) => events.push(InputEvent::Mouse(mouse)),
                        Outcome::Ignored => {}
                        Outcome::PasteStart => {
                            let rest = std::mem::take(&mut self.pending);
                            self.paste = Some(Vec::new());
                            events.extend(self.feed(&rest));
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Minimum run of printable keys inside one read burst that counts as a
/// paste on terminals without bracketed paste (Windows, where that is the
/// norm). A typed burst over a slow link can reach eight keys, so Unix uses a
/// run no typing produces in one read.
pub const COALESCE_MIN_RUN: usize = 8;
pub const COALESCE_MIN_RUN_UNIX: usize = 64;

/// Merges a long run of plain printable keys (and Enter/Tab) that arrived in
/// one read burst into a single paste. Terminals without bracketed paste
/// (Windows PowerShell and others) deliver a paste as a very fast key stream;
/// treating each as a keystroke would submit at every newline.
pub fn coalesce_paste(events: Vec<InputEvent>, min_run: usize) -> Vec<InputEvent> {
    fn plain(event: &InputEvent) -> Option<char> {
        match event {
            InputEvent::Key(key) if key.mods.0 & !Modifiers::SHIFT.0 == 0 => match key.code {
                KeyCode::Char(c) => Some(c),
                KeyCode::Enter => Some('\n'),
                KeyCode::Tab => Some('\t'),
                _ => None,
            },
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(events.len());
    let mut run = String::new();
    let mut run_events: Vec<InputEvent> = Vec::new();
    let flush = |out: &mut Vec<InputEvent>, run: &mut String, run_events: &mut Vec<InputEvent>| {
        if run_events.len() >= min_run {
            out.push(InputEvent::Paste(std::mem::take(run)));
            run_events.clear();
        } else {
            out.append(run_events);
            run.clear();
        }
    };
    for event in events {
        match plain(&event) {
            Some(c) => {
                run.push(c);
                run_events.push(event);
            }
            None => {
                flush(&mut out, &mut run, &mut run_events);
                out.push(event);
            }
        }
    }
    flush(&mut out, &mut run, &mut run_events);
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

enum Parse {
    Incomplete,
    Consumed(usize, Outcome),
}

enum Outcome {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Ignored,
    PasteStart,
}

fn key(code: KeyCode, mods: Modifiers) -> Outcome {
    Outcome::Key(KeyEvent::new(code, mods))
}

fn parse_one(buf: &[u8]) -> Parse {
    match buf[0] {
        0x1b => parse_escape(buf),
        0x0d => Parse::Consumed(1, key(KeyCode::Enter, Modifiers::NONE)),
        0x09 => Parse::Consumed(1, key(KeyCode::Tab, Modifiers::NONE)),
        0x7f | 0x08 => Parse::Consumed(1, key(KeyCode::Backspace, Modifiers::NONE)),
        0x00 => Parse::Consumed(1, key(KeyCode::Char(' '), Modifiers::CTRL)),
        byte @ 0x01..=0x1a => {
            // Ctrl+A..Ctrl+Z (Ctrl+I/Ctrl+M/Ctrl+H were matched above).
            let letter = (b'a' + byte - 1) as char;
            Parse::Consumed(1, key(KeyCode::Char(letter), Modifiers::CTRL))
        }
        byte @ 0x1c..=0x1f => {
            let c = b"\\]^_"[(byte - 0x1c) as usize] as char;
            Parse::Consumed(1, key(KeyCode::Char(c), Modifiers::CTRL))
        }
        _ => match decode_char(buf) {
            Some((c, len)) => Parse::Consumed(len, key(KeyCode::Char(c), Modifiers::NONE)),
            None if utf8_needs_more(buf) => Parse::Incomplete,
            None => Parse::Consumed(1, Outcome::Ignored),
        },
    }
}

/// The first UTF-8 scalar of `buf` and its byte length.
fn decode_char(buf: &[u8]) -> Option<(char, usize)> {
    let need = match buf[0] {
        0x20..=0x7e => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    let chunk = buf.get(..need)?;
    let text = std::str::from_utf8(chunk).ok()?;
    text.chars().next().map(|c| (c, need))
}

fn utf8_needs_more(buf: &[u8]) -> bool {
    let need = match buf[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return false,
    };
    buf.len() < need && buf[1..].iter().all(|b| b & 0xc0 == 0x80)
}

fn parse_escape(buf: &[u8]) -> Parse {
    let Some(&second) = buf.get(1) else {
        return Parse::Incomplete;
    };
    match second {
        b'[' => parse_csi(buf),
        b'O' => match buf.get(2) {
            None => Parse::Incomplete,
            Some(&final_byte) => {
                let code = match final_byte {
                    b'A' => Some(KeyCode::Up),
                    b'B' => Some(KeyCode::Down),
                    b'C' => Some(KeyCode::Right),
                    b'D' => Some(KeyCode::Left),
                    b'H' => Some(KeyCode::Home),
                    b'F' => Some(KeyCode::End),
                    b'P'..=b'S' => Some(KeyCode::F(final_byte - b'P' + 1)),
                    _ => None,
                };
                match code {
                    Some(code) => Parse::Consumed(3, key(code, Modifiers::NONE)),
                    None => Parse::Consumed(3, Outcome::Ignored),
                }
            }
        },
        // DCS (XTVERSION reply and friends) and OSC: swallow to the terminator.
        b'P' | b']' | b'_' | b'^' | b'X' => match string_terminator(&buf[2..]) {
            Some(len) => Parse::Consumed(2 + len, Outcome::Ignored),
            None => Parse::Incomplete,
        },
        0x1b => Parse::Consumed(1, key(KeyCode::Esc, Modifiers::NONE)),
        _ => {
            // Alt+<char> (ESC prefix).
            match parse_one(&buf[1..]) {
                Parse::Incomplete => Parse::Incomplete,
                Parse::Consumed(len, Outcome::Key(mut event)) => {
                    event.mods.0 |= Modifiers::ALT.0;
                    Parse::Consumed(1 + len, Outcome::Key(event))
                }
                Parse::Consumed(len, other) => Parse::Consumed(1 + len, other),
            }
        }
    }
}

/// Length through the terminator (`BEL` or `ESC \`) of a string sequence.
fn string_terminator(body: &[u8]) -> Option<usize> {
    for (index, &byte) in body.iter().enumerate() {
        if byte == 0x07 {
            return Some(index + 1);
        }
        if byte == 0x1b && body.get(index + 1) == Some(&b'\\') {
            return Some(index + 2);
        }
    }
    None
}

fn parse_csi(buf: &[u8]) -> Parse {
    // buf = ESC [ <params 0x30-0x3f>* <intermediates 0x20-0x2f>* <final 0x40-0x7e>
    let mut index = 2;
    while index < buf.len() && (0x30..=0x3f).contains(&buf[index]) {
        index += 1;
    }
    let params_end = index;
    while index < buf.len() && (0x20..=0x2f).contains(&buf[index]) {
        index += 1;
    }
    let Some(&final_byte) = buf.get(index) else {
        return Parse::Incomplete;
    };
    if !(0x40..=0x7e).contains(&final_byte) {
        // Malformed: drop the introducer so the stream resynchronizes.
        return Parse::Consumed(2, Outcome::Ignored);
    }
    let len = index + 1;
    let params = std::str::from_utf8(&buf[2..params_end]).unwrap_or("");
    Parse::Consumed(len, csi_outcome(params, final_byte))
}

fn numbers(params: &str) -> Vec<Vec<u32>> {
    params
        .split(';')
        .map(|group| {
            group
                .split(':')
                .map(|n| n.parse::<u32>().unwrap_or(0))
                .collect()
        })
        .collect()
}

fn csi_outcome(params: &str, final_byte: u8) -> Outcome {
    if let Some(sgr) = params.strip_prefix('<') {
        return mouse(sgr, final_byte);
    }
    // Replies and reports: leading private markers belong to terminal answers.
    if params.starts_with('?') || params.starts_with('>') || params.starts_with('=') {
        return Outcome::Ignored;
    }
    let groups = numbers(params);
    let first = groups.first().and_then(|g| g.first()).copied().unwrap_or(0);
    let mods_of = |index: usize| {
        groups
            .get(index)
            .and_then(|g| g.first())
            .copied()
            .map(Modifiers::from_wire)
            .unwrap_or(Modifiers::NONE)
    };
    match final_byte {
        b'A' => key(KeyCode::Up, mods_of(1)),
        b'B' => key(KeyCode::Down, mods_of(1)),
        b'C' => key(KeyCode::Right, mods_of(1)),
        b'D' => key(KeyCode::Left, mods_of(1)),
        b'H' => key(KeyCode::Home, mods_of(1)),
        b'F' => key(KeyCode::End, mods_of(1)),
        b'Z' => key(KeyCode::BackTab, Modifiers::SHIFT),
        b'P' | b'Q' | b'S' => key(
            KeyCode::F(match final_byte {
                b'P' => 1,
                b'Q' => 2,
                _ => 4,
            }),
            mods_of(1),
        ),
        // F3 is `CSI 1 ; mods R`; a cursor position report is `CSI row ; col R`.
        b'R' if groups.len() == 2 && first == 1 => key(KeyCode::F(3), mods_of(1)),
        b'~' => tilde(first, &groups, mods_of(1)),
        b'u' => kitty(&groups),
        // Focus in/out, DA replies without markers, CPR, window reports.
        _ => Outcome::Ignored,
    }
}

/// SGR mouse report `CSI < button ; column ; row M|m` (1-based cells).
fn mouse(params: &str, final_byte: u8) -> Outcome {
    let mut fields = params.split(';').map(|n| n.parse::<u32>().ok());
    let (Some(Some(button)), Some(Some(column)), Some(Some(row))) =
        (fields.next(), fields.next(), fields.next())
    else {
        return Outcome::Ignored;
    };
    // Modifier bits (4 shift, 8 alt, 16 ctrl) do not change the meaning.
    let code = button & !(4 | 8 | 16);
    let kind = match (code, final_byte) {
        (64, b'M') => MouseKind::WheelUp,
        (65, b'M') => MouseKind::WheelDown,
        (0, b'M') => MouseKind::Press,
        (32, b'M') => MouseKind::Drag,
        (0, b'm') => MouseKind::Release,
        _ => return Outcome::Ignored,
    };
    let cell = |n: u32| n.saturating_sub(1).min(u16::MAX as u32) as u16;
    Outcome::Mouse(MouseEvent {
        kind,
        column: cell(column),
        row: cell(row),
    })
}

fn tilde(first: u32, groups: &[Vec<u32>], mods: Modifiers) -> Outcome {
    match first {
        200 => Outcome::PasteStart,
        201 => Outcome::Ignored,
        1 | 7 => key(KeyCode::Home, mods),
        2 => key(KeyCode::Insert, mods),
        3 => key(KeyCode::Delete, mods),
        4 | 8 => key(KeyCode::End, mods),
        5 => key(KeyCode::PageUp, mods),
        6 => key(KeyCode::PageDown, mods),
        11..=15 => key(KeyCode::F((first - 10) as u8), mods),
        17..=21 => key(KeyCode::F((first - 11) as u8), mods),
        23 | 24 => key(KeyCode::F((first - 12) as u8), mods),
        // xterm modifyOtherKeys: CSI 27 ; mods ; code ~
        27 => {
            let mods = groups
                .get(1)
                .and_then(|g| g.first())
                .copied()
                .map(Modifiers::from_wire)
                .unwrap_or(Modifiers::NONE);
            let code = groups.get(2).and_then(|g| g.first()).copied().unwrap_or(0);
            unicode_key(code, mods)
        }
        _ => Outcome::Ignored,
    }
}

/// Kitty `CSI code[:shifted[:base]] ; mods[:event] ; text u`. Releases are
/// dropped: the composer acts on presses and repeats only.
fn kitty(groups: &[Vec<u32>]) -> Outcome {
    let code = groups.first().and_then(|g| g.first()).copied().unwrap_or(0);
    let mod_group = groups.get(1);
    let mods = mod_group
        .and_then(|g| g.first())
        .copied()
        .map(Modifiers::from_wire)
        .unwrap_or(Modifiers::NONE);
    let event = mod_group.and_then(|g| g.get(1)).copied().unwrap_or(1);
    if event == 3 {
        return Outcome::Ignored;
    }
    unicode_key(code, mods)
}

fn unicode_key(code: u32, mods: Modifiers) -> Outcome {
    match code {
        13 => key(KeyCode::Enter, mods),
        9 => key(KeyCode::Tab, mods),
        27 => key(KeyCode::Esc, mods),
        127 | 8 => key(KeyCode::Backspace, mods),
        0 => Outcome::Ignored,
        _ => match char::from_u32(code) {
            Some(c) if !c.is_control() => key(KeyCode::Char(c), mods),
            _ => Outcome::Ignored,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(events: Vec<InputEvent>) -> Vec<(KeyCode, u8)> {
        events
            .into_iter()
            .map(|event| match event {
                InputEvent::Key(k) => (k.code, k.mods.0),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn plain_text_and_controls() {
        let mut d = Decoder::new();
        assert_eq!(
            keys(d.feed(b"a\x03\r")),
            vec![
                (KeyCode::Char('a'), 0),
                (KeyCode::Char('c'), Modifiers::CTRL.0),
                (KeyCode::Enter, 0)
            ]
        );
    }

    #[test]
    fn sgr_mouse_reports_become_mouse_events() {
        let mut d = Decoder::new();
        let events = d.feed(b"\x1b[<64;3;5M\x1b[<0;10;2M\x1b[<32;11;2M\x1b[<0;12;2m\x1b[<2;1;1M");
        let kinds: Vec<_> = events
            .into_iter()
            .map(|event| match event {
                InputEvent::Mouse(m) => (m.kind, m.column, m.row),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                (MouseKind::WheelUp, 2, 4),
                (MouseKind::Press, 9, 1),
                (MouseKind::Drag, 10, 1),
                (MouseKind::Release, 11, 1),
            ]
        );
    }

    #[test]
    fn split_csi_is_reassembled() {
        let mut d = Decoder::new();
        assert!(d.feed(b"\x1b[1;").is_empty());
        assert!(d.has_pending());
        assert_eq!(keys(d.feed(b"5A")), vec![(KeyCode::Up, Modifiers::CTRL.0)]);
    }

    #[test]
    fn split_utf8_is_reassembled() {
        let mut d = Decoder::new();
        let bytes = "中".as_bytes();
        assert!(d.feed(&bytes[..1]).is_empty());
        assert_eq!(keys(d.feed(&bytes[1..])), vec![(KeyCode::Char('中'), 0)]);
    }

    #[test]
    fn terminal_replies_never_become_keys() {
        let mut d = Decoder::new();
        // kitty flags report, DA1, DA2, XTVERSION, focus in, cursor position
        let replies =
            b"\x1b[?5u\x1b[?62;4c\x1b[>1;10;0c\x1bP>|WezTerm 2024\x1b\\\x1b[I\x1b[12;40Rx";
        assert_eq!(keys(d.feed(replies)), vec![(KeyCode::Char('x'), 0)]);
    }

    #[test]
    fn kitty_press_repeat_and_release() {
        let mut d = Decoder::new();
        // Ctrl+C press, then its release, then Ctrl+D press.
        let events = d.feed(b"\x1b[99;5u\x1b[99;5:3u\x1b[100;5:1u");
        assert_eq!(
            keys(events),
            vec![
                (KeyCode::Char('c'), Modifiers::CTRL.0),
                (KeyCode::Char('d'), Modifiers::CTRL.0)
            ]
        );
    }

    #[test]
    fn modify_other_keys_ctrl_letter() {
        let mut d = Decoder::new();
        assert_eq!(
            keys(d.feed(b"\x1b[27;5;99~")),
            vec![(KeyCode::Char('c'), Modifiers::CTRL.0)]
        );
    }

    #[test]
    fn alt_prefix_and_lone_escape() {
        let mut d = Decoder::new();
        assert_eq!(
            keys(d.feed(b"\x1bb")),
            vec![(KeyCode::Char('b'), Modifiers::ALT.0)]
        );
        assert!(d.feed(b"\x1b").is_empty());
        assert!(d.has_pending());
        assert_eq!(keys(d.flush_timeout()), vec![(KeyCode::Esc, 0)]);
    }

    #[test]
    fn bracketed_paste_across_reads_with_trailing_input() {
        let mut d = Decoder::new();
        assert!(d.feed(b"\x1b[200~line1\r\nli").is_empty());
        let events = d.feed(b"ne2\x1b[201~z");
        assert_eq!(events[0], InputEvent::Paste("line1\nline2".into()));
        assert_eq!(
            events[1],
            InputEvent::Key(KeyEvent::new(KeyCode::Char('z'), Modifiers::NONE))
        );
    }

    #[test]
    fn fast_plain_runs_become_one_paste_but_short_ones_stay_keys() {
        let mut d = Decoder::new();
        let burst = coalesce_paste(d.feed(b"line one\rline two"), COALESCE_MIN_RUN);
        assert_eq!(burst, vec![InputEvent::Paste("line one\nline two".into())]);

        let mut d = Decoder::new();
        let typed = coalesce_paste(d.feed(b"hi\r"), COALESCE_MIN_RUN);
        assert_eq!(typed.len(), 3, "a short burst is ordinary typing");

        // A control key splits runs; each side is judged on its own.
        let mut d = Decoder::new();
        let mixed = coalesce_paste(d.feed(b"abcdefghij\x03kl"), COALESCE_MIN_RUN);
        assert_eq!(mixed.len(), 4);
        assert_eq!(mixed[0], InputEvent::Paste("abcdefghij".into()));
    }

    #[test]
    fn unix_needs_a_much_longer_burst_before_it_is_a_paste() {
        // A typed line that arrives in one read stays keystrokes, Enter included.
        let mut d = Decoder::new();
        let typed = coalesce_paste(d.feed(b"fix the bug\r"), COALESCE_MIN_RUN_UNIX);
        assert!(
            typed.iter().all(|e| matches!(e, InputEvent::Key(_))),
            "{typed:?}"
        );

        let mut d = Decoder::new();
        let pasted = format!("{}\rnext", "x".repeat(70));
        let burst = coalesce_paste(d.feed(pasted.as_bytes()), COALESCE_MIN_RUN_UNIX);
        assert!(matches!(&burst[0], InputEvent::Paste(t) if t.ends_with("\nnext")));
    }

    #[test]
    fn an_oversized_paste_is_cut_and_its_rest_never_runs_as_keys() {
        let mut d = Decoder::new();
        let mut events = d.feed(b"\x1b[200~");
        events.extend(d.feed(&vec![b'a'; MAX_PASTE + 1]));
        // The rest of the paste, with newlines, then the end marker.
        events.extend(d.feed(b"\r\rrm -rf\x1b[201~x"));
        assert!(matches!(events[0], InputEvent::Paste(_)), "{events:?}");
        let keys: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, InputEvent::Key(_)))
            .collect();
        assert_eq!(
            keys.len(),
            1,
            "only the key after the paste end: {events:?}"
        );
    }

    #[test]
    fn function_and_navigation_keys() {
        let mut d = Decoder::new();
        assert_eq!(
            keys(d.feed(b"\x1b[3~\x1b[5~\x1bOP\x1b[15~\x1b[Z")),
            vec![
                (KeyCode::Delete, 0),
                (KeyCode::PageUp, 0),
                (KeyCode::F(1), 0),
                (KeyCode::F(5), 0),
                (KeyCode::BackTab, Modifiers::SHIFT.0),
            ]
        );
    }
}
