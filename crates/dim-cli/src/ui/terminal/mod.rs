//! The terminal layer of the scrollback interface (design §6.6, §6.10).
//!
//! Owns what must be handed back: raw mode, the negotiated keyboard protocol,
//! bracketed paste, cursor visibility, progress, the program status record.
//! It decodes input into key and paste events and guarantees the restore
//! sequence runs on every way out — normal return, panic, fatal signal, and a
//! vanished terminal.

pub mod input;
pub mod keyboard;
pub mod modes;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub use input::{InputEvent, KeyCode, KeyEvent, Modifiers, MouseEvent, MouseKind};
pub use keyboard::KeyboardProtocol;

/// Hands the terminal back if this process still holds it: the exit path of
/// whoever outlives a [`Terminal`] that was never left. Nothing is written
/// when no mode is live, so a terminal that was never entered stays as it is.
pub fn restore_if_held() {
    if modes::live_bits() != 0 {
        sys::restore_terminal_once();
    }
}

/// Columns and rows of the terminal on stdout, when it is one.
pub fn window_size() -> Option<(u16, u16)> {
    sys::window_size()
}
use modes::Mode;

/// Running inside tmux, which needs passthrough for some sequences.
pub fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

/// Running inside tmux or GNU screen: they answer the kitty keyboard query
/// on the outer terminal's behalf yet keep sending legacy keys.
fn in_multiplexer() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    in_tmux()
        || std::env::var_os("STY").is_some_and(|v| !v.is_empty())
        || term.starts_with("screen")
        || term.starts_with("tmux")
}

/// Names the window after the product (`ui::title`). Apple Terminal keeps
/// no title stack; elsewhere the shell's title is pushed first.
fn set_title() -> io::Result<()> {
    let apple = crate::ui::title::apple_terminal();
    let mut bytes = if apple {
        Vec::new()
    } else {
        b"\x1b[22;0t".to_vec()
    };
    bytes.extend(crate::ui::title::sequence(crate::ui::title::BASE, apple));
    sys::write_all(sys::STDOUT, &bytes)?;
    modes::mark(Mode::Title);
    Ok(())
}

/// Exit status when the terminal disappears under the process (SIGHUP shape).
pub const EXIT_TERMINAL_LOST: i32 = 129;

/// The bound on leaving the terminal: how long the exit may spend flushing
/// queued output and writing the restore sequence. A terminal that stopped
/// reading (a suspended application, a stalled remote session, a multiplexer
/// client not draining its pane) delays an exit by this much, never holds it.
/// Short enough that a person does not wait on a dead terminal, long enough
/// for the frames of a live one.
pub(crate) const EXIT_BUDGET: Duration = Duration::from_millis(1000);

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const FENCE_TIMEOUT: Duration = Duration::from_millis(300);
/// How long a cursor position report may take.
const CURSOR_REPORT_TIMEOUT: Duration = Duration::from_millis(250);
/// How long a lone ESC waits for the rest of a sequence.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(30);
/// How long a clipboard reply may stall mid-sequence before it is dropped and
/// keys are read again.
const REPLY_STALL: Duration = Duration::from_secs(5);

/// Why the terminal layer cannot run.
#[derive(Debug)]
pub enum TerminalError {
    /// stdin or stdout is not a terminal.
    NotATerminal,
    Io(io::Error),
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotATerminal => write!(f, "dim needs an interactive terminal"),
            Self::Io(error) => write!(f, "terminal setup failed: {error}"),
        }
    }
}

impl From<io::Error> for TerminalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// What came back from waiting for input.
#[derive(Debug)]
pub enum Wait {
    Event(InputEvent),
    Timeout,
    /// The terminal closed (EOF/EIO on input).
    Lost,
}

/// Terminal output goes through one writer thread so a slow terminal (ssh,
/// a busy multiplexer) never blocks the event loop; frames are queued whole
/// and written in order. Exit drains the queue before restoring modes.
struct WriteThread {
    sender: Option<mpsc::Sender<Vec<u8>>>,
    handle: Option<std::thread::JoinHandle<()>>,
    lost: Arc<AtomicBool>,
    /// Writes queued and not yet through to the terminal.
    pending: Arc<std::sync::atomic::AtomicUsize>,
}

impl WriteThread {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let lost = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&lost);
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = Arc::clone(&pending);
        let handle = std::thread::Builder::new()
            .name("dim-terminal-writer".into())
            .spawn(move || {
                for chunk in receiver {
                    if !flag.load(Ordering::SeqCst) && sys::write_all(sys::STDOUT, &chunk).is_err()
                    {
                        flag.store(true, Ordering::SeqCst);
                    }
                    done.fetch_sub(1, Ordering::SeqCst);
                }
            })
            .expect("spawn terminal writer");
        Self {
            sender: Some(sender),
            handle: Some(handle),
            lost,
            pending,
        }
    }

    fn send(&self, bytes: Vec<u8>) -> io::Result<()> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let Some(sender) = &self.sender else {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        };
        self.pending.fetch_add(1, Ordering::SeqCst);
        sender.send(bytes).map_err(|_| {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            io::Error::from(io::ErrorKind::BrokenPipe)
        })
    }

    /// Closes the queue and waits until everything queued was written. With a
    /// deadline, gives up there and leaves the writer behind, which only an
    /// exit may do: a writer still blocked on a terminal that stopped reading
    /// ends with the process. A suspend waits for its writer instead, so the
    /// next one starts on an empty queue.
    fn drain(&mut self, deadline: Option<Instant>) {
        self.sender = None;
        let Some(handle) = self.handle.take() else {
            return;
        };
        let Some(deadline) = deadline else {
            let _ = handle.join();
            return;
        };
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = handle.join();
    }
}

pub struct Terminal {
    writer: WriteThread,
    decoder: input::Decoder,
    /// Merge key runs of at least this many keys in one read into a paste
    /// (terminals without bracketed paste); `None` keeps every key.
    coalesce_min_run: Option<usize>,
    queue: std::collections::VecDeque<InputEvent>,
    keyboard: KeyboardProtocol,
    background_report: Option<String>,
    clipboard: bool,
    last_input: Instant,
    left: bool,
    /// The last size the terminal reported: a zero-sized window (a pane
    /// being re-laid out) keeps it.
    last_size: std::cell::Cell<(u16, u16)>,
}

impl Terminal {
    /// Enters raw mode, negotiates the keyboard, arms bracketed paste and
    /// installs the exit guards. Nothing is pushed unless this succeeds.
    pub fn enter() -> Result<Self, TerminalError> {
        if !sys::is_tty(sys::STDIN) || !sys::is_tty(sys::STDOUT) {
            return Err(TerminalError::NotATerminal);
        }
        sys::enable_raw_mode()?;
        sys::install_signal_guard();
        install_panic_hook();

        let mut terminal = Self {
            writer: WriteThread::start(),
            decoder: input::Decoder::new(),
            last_input: Instant::now(),
            coalesce_min_run: Some(
                if cfg!(windows) || std::env::var_os("DIM_COALESCE_PASTE").is_some() {
                    input::COALESCE_MIN_RUN
                } else {
                    input::COALESCE_MIN_RUN_UNIX
                },
            ),
            queue: Default::default(),
            keyboard: KeyboardProtocol::Legacy,
            background_report: None,
            clipboard: false,
            left: false,
            last_size: std::cell::Cell::new((80, 24)),
        };
        match terminal.negotiate_keyboard() {
            Ok(()) => {}
            Err(error) => {
                sys::restore_terminal_once();
                return Err(error.into());
            }
        }
        sys::write_all(sys::STDOUT, Mode::BracketedPaste.enable_sequence())?;
        modes::mark(Mode::BracketedPaste);
        set_title()?;
        Ok(terminal)
    }

    /// Switches to the fullscreen interface's alternate screen with mouse
    /// reporting, or back to the shell's screen. Queued behind the frames
    /// already written, so they land on the screen they were drawn for.
    pub fn set_fullscreen(&mut self, on: bool) -> io::Result<()> {
        if on == modes::is_live(Mode::AltScreen) {
            return Ok(());
        }
        let mut bytes = Vec::new();
        if on {
            bytes.extend_from_slice(Mode::AltScreen.enable_sequence());
            bytes.extend_from_slice(Mode::Mouse.enable_sequence());
            modes::mark(Mode::AltScreen);
            modes::mark(Mode::Mouse);
        } else {
            bytes.extend_from_slice(Mode::Mouse.restore_sequence());
            bytes.extend_from_slice(Mode::AltScreen.restore_sequence());
            modes::clear(Mode::Mouse);
            modes::clear(Mode::AltScreen);
        }
        self.writer.send(bytes)
    }

    /// Writes a program status report (`ui::status`). From then on every
    /// way out, the restore of a panic or a signal included, clears the
    /// record; a suspend clears it too, so the next status is reported again.
    /// The mode is marked before the write, so a signal that lands during
    /// the write still clears it.
    pub fn report_status(&mut self, report: Vec<u8>) -> io::Result<()> {
        modes::mark(Mode::Status);
        self.writer.send(report)
    }

    /// Draws or removes the progress bar (`ui::progress`). From then on
    /// every way out, the restore of a panic or a signal included, removes
    /// it; a suspend does too, so the next status draws it again. The mode is
    /// marked before the write, so a signal that lands during the write still
    /// removes it.
    pub fn report_progress(&mut self, bar: &[u8]) -> io::Result<()> {
        modes::mark(Mode::Progress);
        self.writer.send(bar.to_vec())
    }

    /// Mouse reporting is on.
    pub fn mouse(&self) -> bool {
        modes::is_live(Mode::Mouse)
    }

    /// Turns mouse reporting on or off on the alternate screen; off hands
    /// selection and the wheel back to the terminal.
    pub fn set_mouse(&mut self, on: bool) -> io::Result<()> {
        if on == self.mouse() || !modes::is_live(Mode::AltScreen) {
            return Ok(());
        }
        if on {
            modes::mark(Mode::Mouse);
            self.writer.send(Mode::Mouse.enable_sequence().to_vec())
        } else {
            modes::clear(Mode::Mouse);
            self.writer.send(Mode::Mouse.restore_sequence().to_vec())
        }
    }

    /// The terminal's background colour report (`rgb:RRRR/GGGG/BBBB`) from the
    /// startup probe, when it answered one.
    pub fn background_report(&self) -> Option<&str> {
        self.background_report.as_deref()
    }

    /// The terminal reported the kitty clipboard protocol (OSC 5522) at the
    /// startup probe: it reads its own clipboard for this process.
    pub fn clipboard_protocol(&self) -> bool {
        self.clipboard
    }

    fn negotiate_keyboard(&mut self) -> io::Result<()> {
        sys::write_all(sys::STDOUT, keyboard::PROBE_QUERY)?;
        let deadline = Instant::now() + PROBE_TIMEOUT;
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        let mut scan = keyboard::scan_probe(&seen);
        while !scan.complete {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match sys::read_timeout(sys::STDIN, &mut buf, left) {
                sys::ReadOutcome::Data(n) => {
                    seen.extend_from_slice(&buf[..n]);
                    scan = keyboard::scan_probe(&seen);
                }
                sys::ReadOutcome::Timeout => break,
                sys::ReadOutcome::Closed => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "terminal closed",
                    ));
                }
            }
        }
        self.background_report = scan.background_report;
        self.clipboard = scan.complete && scan.clipboard;
        let mut carry = scan.typeahead;
        carry.extend_from_slice(&scan.rest);
        if !carry.is_empty() {
            self.ingest(&carry);
        }
        if scan.complete && scan.kitty {
            sys::write_all(sys::STDOUT, Mode::KittyKeyboard.enable_sequence())?;
            modes::mark(Mode::KittyKeyboard);
            self.keyboard = KeyboardProtocol::Kitty;
            if in_multiplexer() {
                sys::write_all(sys::STDOUT, Mode::ModifyOtherKeys.enable_sequence())?;
                modes::mark(Mode::ModifyOtherKeys);
            }
        } else if scan.complete {
            sys::write_all(sys::STDOUT, Mode::ModifyOtherKeys.enable_sequence())?;
            modes::mark(Mode::ModifyOtherKeys);
            self.keyboard = KeyboardProtocol::ModifyOtherKeys;
        }
        Ok(())
    }

    /// Writes terminal output. A write failure with a terminal-loss errno is
    /// the terminal telling us it is gone; the caller should treat `Err` from
    /// here as fatal and call [`Terminal::lost`].
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.send(bytes.to_vec())
    }

    /// Earlier output is still on its way to the terminal: a slow terminal
    /// (ssh, a busy multiplexer) is behind, and the next frame waits rather
    /// than queue behind it.
    pub fn writing(&self) -> bool {
        self.writer.pending.load(Ordering::SeqCst) > 0
    }

    /// True once a background write failed (terminal gone).
    pub fn write_failed(&self) -> bool {
        self.writer.lost.load(Ordering::SeqCst)
    }

    /// Restores everything it can and returns the exit status for a vanished
    /// terminal. Never returns control of a usable screen; the status is the
    /// conventional SIGHUP one.
    pub fn lost(&mut self) -> i32 {
        self.left = true;
        let deadline = Instant::now() + EXIT_BUDGET;
        self.writer.drain(Some(deadline));
        sys::restore_terminal_once_until(deadline);
        sys::remove_signal_guard();
        EXIT_TERMINAL_LOST
    }

    /// Waits for the next key or paste event.
    pub fn next_event(&mut self, timeout: Duration) -> Wait {
        if let Some(event) = self.queue.pop_front() {
            return Wait::Event(event);
        }
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 4096];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = if self.decoder.holds_reply() {
                REPLY_STALL
                    .saturating_sub(self.last_input.elapsed())
                    .min(remaining)
                    .max(Duration::from_millis(1))
            } else if self.decoder.has_pending() {
                ESCAPE_TIMEOUT.min(remaining.max(Duration::from_millis(1)))
            } else {
                remaining
            };
            match sys::read_timeout(sys::STDIN, &mut buf, wait) {
                sys::ReadOutcome::Data(n) => {
                    self.ingest(&buf[..n]);
                    if let Some(event) = self.queue.pop_front() {
                        return Wait::Event(event);
                    }
                }
                sys::ReadOutcome::Timeout => {
                    let stalled =
                        !self.decoder.holds_reply() || self.last_input.elapsed() >= REPLY_STALL;
                    if self.decoder.has_pending() && stalled {
                        self.queue.extend(self.decoder.flush_timeout());
                        if let Some(event) = self.queue.pop_front() {
                            return Wait::Event(event);
                        }
                    }
                    if remaining.is_zero() || Instant::now() >= deadline {
                        return Wait::Timeout;
                    }
                }
                sys::ReadOutcome::Closed => return Wait::Lost,
            }
        }
    }

    fn ingest(&mut self, bytes: &[u8]) {
        self.last_input = Instant::now();
        let events = self.decoder.feed(bytes);
        let events = match self.coalesce_min_run {
            Some(min_run) => input::coalesce_paste(events, min_run),
            None => events,
        };
        self.queue.extend(events);
    }

    /// Asks the terminal where its cursor is (`CSI 6n`): `(row, column)`,
    /// 0-based. The request goes through the writer queue, so every frame
    /// already sent has been drawn when the answer comes. Other input that
    /// arrives meanwhile is kept for the event loop.
    pub fn cursor_position(&mut self) -> Option<(u16, u16)> {
        if self.writer.send(b"\x1b[6n".to_vec()).is_err() {
            return None;
        }
        let deadline = Instant::now() + CURSOR_REPORT_TIMEOUT;
        let mut seen: Vec<u8> = Vec::new();
        let mut buf = [0u8; 256];
        let mut found = None;
        while found.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match sys::read_timeout(sys::STDIN, &mut buf, left) {
                sys::ReadOutcome::Data(n) => {
                    seen.extend_from_slice(&buf[..n]);
                    found = take_cursor_report(&mut seen);
                }
                sys::ReadOutcome::Timeout | sys::ReadOutcome::Closed => break,
            }
        }
        if !seen.is_empty() {
            self.ingest(&seen);
        }
        found
    }

    pub fn size(&self) -> (u16, u16) {
        match sys::window_size() {
            Some(size) => {
                self.last_size.set(size);
                size
            }
            None => self.last_size.get(),
        }
    }

    /// Hands the terminal back to the shell and stops the process (Ctrl+Z).
    /// Output already queued reaches the screen before the modes go; the
    /// writer is restarted because the process keeps writing. Returns once
    /// the job is continued, with the terminal entered again. A suspend is
    /// not an exit: it waits for its writer, and only the writes that hand
    /// the terminal back are bounded.
    pub fn suspend(&mut self) -> Result<(), TerminalError> {
        self.writer.drain(None);
        let deadline = Instant::now() + EXIT_BUDGET;
        self.writer = WriteThread::start();
        let fullscreen = modes::is_live(Mode::AltScreen);
        self.release_modes(deadline);
        stop_process();
        sys::rearm_restore_guard();
        sys::enable_raw_mode()?;
        sys::install_signal_guard();
        self.keyboard = KeyboardProtocol::Legacy;
        self.negotiate_keyboard()?;
        sys::write_all(sys::STDOUT, Mode::BracketedPaste.enable_sequence())?;
        modes::mark(Mode::BracketedPaste);
        set_title()?;
        if fullscreen {
            self.set_fullscreen(true)?;
        }
        Ok(())
    }

    /// Leaves normally: pops the keyboard protocol behind the DA1 fence, then
    /// writes the rest of the restore sequence and drops raw mode. The whole
    /// handing back shares one [`EXIT_BUDGET`] deadline.
    pub fn leave(mut self) {
        self.left = true;
        let deadline = Instant::now() + EXIT_BUDGET;
        self.writer.drain(Some(deadline));
        self.release_modes(deadline);
    }

    /// Undoes the modes this process turned on, keyboard first. Kitty is
    /// popped before draining to the DA1 fence, so no new reports start; the
    /// answer proves the pop applied.
    fn release_modes(&mut self, deadline: Instant) {
        if modes::is_live(Mode::KittyKeyboard) {
            let mut out = Vec::new();
            out.extend_from_slice(b"\x1b[<u");
            out.extend_from_slice(keyboard::FENCE_QUERY);
            sys::write_until(sys::STDOUT, &out, deadline);
            modes::clear(Mode::KittyKeyboard);
            self.drain_until_da1(deadline);
        }
        sys::restore_terminal_once_until(deadline);
        sys::remove_signal_guard();
        modes::clear_all();
    }

    fn drain_until_da1(&mut self, deadline: Instant) {
        let deadline = (Instant::now() + FENCE_TIMEOUT).min(deadline);
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match sys::read_timeout(sys::STDIN, &mut buf, left) {
                sys::ReadOutcome::Data(n) => {
                    tail.extend_from_slice(&buf[..n]);
                    if keyboard::da1_reply_len(&tail).is_some() {
                        return;
                    }
                    if tail.len() > 64 {
                        tail.drain(..tail.len() - 64);
                    }
                }
                sys::ReadOutcome::Timeout | sys::ReadOutcome::Closed => return,
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.left {
            let deadline = Instant::now() + EXIT_BUDGET;
            self.writer.drain(Some(deadline));
            sys::restore_terminal_once_until(deadline);
            sys::remove_signal_guard();
        }
    }
}

/// Stops the process for job control. There is none on Windows, so Ctrl+Z
/// only repaints there.
#[cfg(unix)]
fn stop_process() {
    sys::raise_stop();
}

#[cfg(not(unix))]
fn stop_process() {}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        sys::restore_terminal_once();
        previous(info);
    }));
}

/// Removes the first complete cursor position report (`ESC [ row ; col R`)
/// from `bytes` and returns it as 0-based `(row, column)`.
pub(crate) fn take_cursor_report(bytes: &mut Vec<u8>) -> Option<(u16, u16)> {
    let mut index = 0;
    while index + 5 < bytes.len() + 1 {
        if bytes.get(index) == Some(&0x1b) && bytes.get(index + 1) == Some(&b'[') {
            let mut cursor = index + 2;
            let row_start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if cursor > row_start && bytes.get(cursor) == Some(&b';') {
                let row = &bytes[row_start..cursor];
                cursor += 1;
                let col_start = cursor;
                while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                    cursor += 1;
                }
                if cursor > col_start && bytes.get(cursor) == Some(&b'R') {
                    let parse = |digits: &[u8]| -> u16 {
                        std::str::from_utf8(digits)
                            .ok()
                            .and_then(|t| t.parse::<u32>().ok())
                            .map_or(1, |v| v.min(u16::MAX as u32) as u16)
                    };
                    let report = (
                        parse(row).saturating_sub(1),
                        parse(&bytes[col_start..cursor]).saturating_sub(1),
                    );
                    bytes.drain(index..=cursor);
                    return Some(report);
                }
            }
        }
        index += 1;
    }
    None
}

/// The key as the composer's editor (crossterm events) reads it.
pub fn to_crossterm(key: KeyEvent) -> Option<crossterm::event::KeyEvent> {
    let code = match key.code {
        KeyCode::Char(c) => crossterm::event::KeyCode::Char(c),
        KeyCode::Enter => crossterm::event::KeyCode::Enter,
        KeyCode::Tab => crossterm::event::KeyCode::Tab,
        KeyCode::BackTab => crossterm::event::KeyCode::BackTab,
        KeyCode::Backspace => crossterm::event::KeyCode::Backspace,
        KeyCode::Delete => crossterm::event::KeyCode::Delete,
        KeyCode::Esc => crossterm::event::KeyCode::Esc,
        KeyCode::Up => crossterm::event::KeyCode::Up,
        KeyCode::Down => crossterm::event::KeyCode::Down,
        KeyCode::Left => crossterm::event::KeyCode::Left,
        KeyCode::Right => crossterm::event::KeyCode::Right,
        KeyCode::Home => crossterm::event::KeyCode::Home,
        KeyCode::End => crossterm::event::KeyCode::End,
        KeyCode::PageUp => crossterm::event::KeyCode::PageUp,
        KeyCode::PageDown => crossterm::event::KeyCode::PageDown,
        KeyCode::Insert => crossterm::event::KeyCode::Insert,
        KeyCode::F(n) => crossterm::event::KeyCode::F(n),
    };
    let mut modifiers = crossterm::event::KeyModifiers::NONE;
    if key.mods.contains(Modifiers::SHIFT) {
        modifiers |= crossterm::event::KeyModifiers::SHIFT;
    }
    if key.mods.contains(Modifiers::ALT) {
        modifiers |= crossterm::event::KeyModifiers::ALT;
    }
    if key.mods.contains(Modifiers::CTRL) {
        modifiers |= crossterm::event::KeyModifiers::CONTROL;
    }
    if key.mods.contains(Modifiers::SUPER) {
        modifiers |= crossterm::event::KeyModifiers::SUPER;
    }
    Some(crossterm::event::KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_report_is_taken_out_of_the_stream() {
        let mut bytes = b"ab\x1b[12;7Rcd".to_vec();
        assert_eq!(take_cursor_report(&mut bytes), Some((11, 6)));
        assert_eq!(bytes, b"abcd");
        let mut none = b"\x1b[?62c".to_vec();
        assert_eq!(take_cursor_report(&mut none), None);
        assert_eq!(none, b"\x1b[?62c");
    }
}
