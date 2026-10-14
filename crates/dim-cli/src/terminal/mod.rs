//! The terminal layer of the scrollback interface (design §6.6, §6.10).
//!
//! Owns what must be handed back: raw mode, the negotiated keyboard protocol,
//! bracketed paste, cursor visibility, progress. It decodes input into key and
//! paste events and guarantees the restore sequence runs on every way out —
//! normal return, panic, fatal signal, and a vanished terminal.

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

pub use input::{InputEvent, KeyCode, KeyEvent, Modifiers};
pub use keyboard::KeyboardProtocol;
use modes::Mode;

/// Exit status when the terminal disappears under the process (SIGHUP shape).
pub const EXIT_TERMINAL_LOST: i32 = 129;

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const FENCE_TIMEOUT: Duration = Duration::from_millis(300);
/// How long a cursor position report may take.
const CURSOR_REPORT_TIMEOUT: Duration = Duration::from_millis(250);
/// How long a lone ESC waits for the rest of a sequence.
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(30);

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
}

impl WriteThread {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let lost = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&lost);
        let handle = std::thread::Builder::new()
            .name("dim-terminal-writer".into())
            .spawn(move || {
                for chunk in receiver {
                    if flag.load(Ordering::SeqCst) {
                        continue;
                    }
                    if sys::write_all(sys::STDOUT, &chunk).is_err() {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
            })
            .expect("spawn terminal writer");
        Self {
            sender: Some(sender),
            handle: Some(handle),
            lost,
        }
    }

    fn send(&self, bytes: Vec<u8>) -> io::Result<()> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        match &self.sender {
            Some(sender) => sender
                .send(bytes)
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe)),
            None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        }
    }

    /// Closes the queue and waits until everything queued was written.
    fn drain(&mut self) {
        self.sender = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
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
    left: bool,
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
            left: false,
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
        Ok(terminal)
    }

    pub fn keyboard(&self) -> KeyboardProtocol {
        self.keyboard
    }

    /// The terminal's background colour report (`rgb:RRRR/GGGG/BBBB`) from the
    /// startup probe, when it answered one.
    pub fn background_report(&self) -> Option<&str> {
        self.background_report.as_deref()
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
        // Whatever the user typed meanwhile is still theirs.
        let mut carry = scan.typeahead;
        carry.extend_from_slice(&scan.rest);
        if !carry.is_empty() {
            self.ingest(&carry);
        }
        if scan.complete && scan.kitty {
            sys::write_all(sys::STDOUT, Mode::KittyKeyboard.enable_sequence())?;
            modes::mark(Mode::KittyKeyboard);
            self.keyboard = KeyboardProtocol::Kitty;
        } else if scan.complete {
            // Answered DA1 but not kitty: ask xterm-style terminals for
            // modifyOtherKeys; terminals that ignore it stay in legacy.
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

    /// True once a background write failed (terminal gone).
    pub fn write_failed(&self) -> bool {
        self.writer.lost.load(Ordering::SeqCst)
    }

    /// True when `error` means the terminal vanished (EIO/EPIPE/EBADF).
    pub fn is_loss(error: &io::Error) -> bool {
        #[cfg(unix)]
        {
            matches!(
                error.raw_os_error(),
                Some(libc::EIO) | Some(libc::EPIPE) | Some(libc::EBADF)
            )
        }
        #[cfg(not(unix))]
        {
            matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionAborted
            )
        }
    }

    /// Restores everything it can and returns the exit status for a vanished
    /// terminal. Never returns control of a usable screen; the status is the
    /// conventional SIGHUP one.
    pub fn lost(&mut self) -> i32 {
        self.left = true;
        self.writer.drain();
        sys::restore_terminal_once();
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
            let wait = if self.decoder.has_pending() {
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
                    if self.decoder.has_pending() {
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
        sys::window_size().unwrap_or((80, 24))
    }

    /// Hands the terminal back to the shell and stops the process (Ctrl+Z).
    /// Returns once the job is continued, with the terminal entered again.
    pub fn suspend(&mut self) -> Result<(), TerminalError> {
        // Output already queued reaches the screen before the modes go; the
        // writer is started again because the process keeps writing.
        self.writer.drain();
        self.writer = WriteThread::start();
        self.release_modes();
        stop_process();
        sys::rearm_restore_guard();
        sys::enable_raw_mode()?;
        sys::install_signal_guard();
        self.keyboard = KeyboardProtocol::Legacy;
        self.negotiate_keyboard()?;
        sys::write_all(sys::STDOUT, Mode::BracketedPaste.enable_sequence())?;
        modes::mark(Mode::BracketedPaste);
        Ok(())
    }

    /// Leaves normally: pops the keyboard protocol behind the DA1 fence, then
    /// writes the rest of the restore sequence and drops raw mode.
    pub fn leave(mut self) {
        self.left = true;
        // Everything queued reaches the screen before modes are undone.
        self.writer.drain();
        self.release_modes();
    }

    /// Undoes the modes this process turned on, keyboard first.
    fn release_modes(&mut self) {
        if modes::is_live(Mode::KittyKeyboard) {
            // Pop first so no new reports start, then drain what the terminal
            // already emitted: its DA1 answer proves the pop was applied.
            let mut out = Vec::new();
            out.extend_from_slice(b"\x1b[<u");
            out.extend_from_slice(keyboard::FENCE_QUERY);
            let _ = sys::write_all(sys::STDOUT, &out);
            modes::clear(Mode::KittyKeyboard);
            self.drain_until_da1();
        }
        sys::restore_terminal_once();
        sys::remove_signal_guard();
        modes::clear_all();
    }

    fn drain_until_da1(&mut self) {
        let deadline = Instant::now() + FENCE_TIMEOUT;
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
                    // Only the end can complete a reply.
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
            // Unwinding or an early return: still hand the terminal back,
            // after whatever was already queued.
            self.writer.drain();
            sys::restore_terminal_once();
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
        // Restore before the report so the message lands on a sane screen.
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
