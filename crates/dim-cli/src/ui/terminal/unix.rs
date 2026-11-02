//! POSIX terminal plumbing: raw mode, timed reads, window size, and the
//! async-signal-safe exit path.
//!
//! The restore path is deliberately free of locks and allocation: the original
//! `termios` lives in a `OnceLock` (an atomic load to read) and the restore
//! sequence is built on the stack from the atomic mode set.

use std::io;
use std::os::fd::RawFd;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use super::modes::{self, MAX_RESTORE_LEN};

pub const STDIN: RawFd = 0;
pub const STDOUT: RawFd = 1;

static ORIGINAL: OnceLock<libc::termios> = OnceLock::new();
/// Raw mode is currently applied; the restore path skips tcsetattr otherwise.
static RAW_ON: AtomicBool = AtomicBool::new(false);
/// The restore sequence was already written by someone (exit, signal, panic).
static RESTORED: AtomicBool = AtomicBool::new(false);
/// `UNKNOWN_FLAGS` until [`write_until`] first read stdout's flags, then the
/// flags before any bounded write: a signal that kills the process puts them
/// back, so the shell never shares a non-blocking terminal.
static STDOUT_FLAGS: AtomicI32 = AtomicI32::new(UNKNOWN_FLAGS);
const UNKNOWN_FLAGS: i32 = -1;

pub fn is_tty(fd: RawFd) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// Switches the controlling input to raw mode, remembering the original
/// settings for every exit path. Output post-processing stays on, so a stray
/// diagnostic `\n` still becomes CRLF; the renderer writes its own `\r\n`.
pub fn enable_raw_mode() -> io::Result<()> {
    // SAFETY: zeroed termios is a valid out-parameter for tcgetattr.
    let mut current: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(STDIN, &mut current) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let _ = ORIGINAL.set(current);
    let mut raw = current;
    // SAFETY: cfmakeraw only edits the struct.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_oflag |= libc::OPOST;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    RAW_ON.store(true, Ordering::SeqCst);
    Ok(())
}

/// Puts the settings read at entry back at once. `TCSADRAIN` would wait for
/// the terminal to consume everything already queued, which is the one thing
/// a terminal that stopped reading never does; output written before the
/// change was already processed under the old settings, so nothing needs the
/// drain.
pub fn disable_raw_mode() {
    if RAW_ON.swap(false, Ordering::SeqCst)
        && let Some(original) = ORIGINAL.get()
    {
        // SAFETY: restoring the exact settings read earlier.
        unsafe { libc::tcsetattr(STDIN, libc::TCSANOW, original) };
    }
}

/// Writes all of `bytes` to `fd`, retrying on EINTR/short writes.
pub fn write_all(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: pointer and length come from a live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    wait_fd(fd, libc::POLLOUT, 100);
                    continue;
                }
                _ => return Err(error),
            }
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

fn wait_fd(fd: RawFd, events: libc::c_short, timeout_ms: i32) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(&mut poll, 1, timeout_ms) };
    ready > 0
}

fn millis(duration: Duration) -> i32 {
    duration.as_millis().min(i32::MAX as u128) as i32
}

/// Writes all of `bytes` to `fd`, giving up at `deadline`.
///
/// The descriptor goes non-blocking for the duration and gets its own flags
/// back before this returns, whatever the outcome: the shell shares it, so
/// `O_NONBLOCK` must not outlive the call. A terminal that stopped reading
/// costs at most the time left to `deadline`. Safe to call from a signal
/// handler: only fcntl(2), write(2), poll(2) and the clock, no locks and no
/// allocation.
pub fn write_until(fd: RawFd, mut bytes: &[u8], deadline: Instant) {
    // SAFETY: fcntl only inspects the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return;
    }
    if fd == STDOUT {
        // The flags before any bounded write, kept for the fatal-signal path:
        // a signal can kill the process while this call, or another one, is
        // polling, and a management call must never leave the shell with a
        // non-blocking terminal. The first read wins; a later call may
        // already see O_NONBLOCK set by a call still running.
        let _ =
            STDOUT_FLAGS.compare_exchange(UNKNOWN_FLAGS, flags, Ordering::SeqCst, Ordering::SeqCst);
    }
    let restore = flags & libc::O_NONBLOCK == 0;
    if restore && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return;
    }
    while !bytes.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        // SAFETY: pointer and length come from a live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
            continue;
        }
        if n == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                wait_fd(fd, libc::POLLOUT, millis(left));
                continue;
            }
            _ => break,
        }
    }
    if restore {
        // SAFETY: putting back the flags read above.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    }
}

/// What a timed read produced.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    Data(usize),
    Timeout,
    /// EOF or a hangup/EIO: the terminal is gone.
    Closed,
}

pub fn read_timeout(fd: RawFd, buf: &mut [u8], timeout: Duration) -> ReadOutcome {
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(&mut poll, 1, millis) };
    if ready < 0 {
        return if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            ReadOutcome::Timeout
        } else {
            ReadOutcome::Closed
        };
    }
    if ready == 0 {
        return ReadOutcome::Timeout;
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
        && poll.revents & libc::POLLIN == 0
    {
        return ReadOutcome::Closed;
    }
    // SAFETY: buffer pointer and length come from a live slice.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    match n {
        0 => ReadOutcome::Closed,
        n if n > 0 => ReadOutcome::Data(n as usize),
        _ => match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => ReadOutcome::Timeout,
            _ => ReadOutcome::Closed,
        },
    }
}

/// Live window size from TIOCGWINSZ (`columns`, `rows`).
pub fn window_size() -> Option<(u16, u16)> {
    // SAFETY: zeroed winsize is a valid out-parameter.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::ioctl(STDOUT, libc::TIOCGWINSZ, &mut size) };
    (result == 0 && size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}

/// Writes the mode restore sequence once and drops raw mode, giving the write
/// the exit bound. Safe to call from a signal handler: no locks, no
/// allocation, only write(2)/fcntl(2)/poll(2)/tcsetattr(3).
pub fn restore_terminal_once() {
    restore_terminal_once_until(Instant::now() + super::EXIT_BUDGET);
}

/// [`restore_terminal_once`] sharing the exit's deadline: the restore of an
/// exit that already spent part of its bound does not start a new one.
pub fn restore_terminal_once_until(deadline: Instant) {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut buf = [0u8; MAX_RESTORE_LEN];
    let len = modes::restore_into(modes::live_bits(), &mut buf);
    write_until(STDOUT, &buf[..len], deadline);
    disable_raw_mode();
}

/// Lets the next exit path write the restore sequence again: the terminal was
/// handed to an external command and is now ours.
pub fn rearm_restore_guard() {
    RESTORED.store(false, Ordering::SeqCst);
}

/// Puts back the flags [`write_until`] recorded, when it recorded any: a
/// signal that kills the process while a bounded write is polling must not
/// leave the shell sharing a non-blocking terminal.
fn restore_flags() {
    let flags = STDOUT_FLAGS.load(Ordering::SeqCst);
    if flags != UNKNOWN_FLAGS {
        // SAFETY: putting back flags read from the same descriptor.
        unsafe { libc::fcntl(STDOUT, libc::F_SETFL, flags) };
    }
}

extern "C" fn on_fatal_signal(signal: libc::c_int) {
    restore_terminal_once();
    // Whether this handler's restore wrote anything or found it already done:
    // a write it interrupts is restored here, before the process dies.
    restore_flags();
    // SAFETY: raise is async-signal-safe.
    unsafe { libc::raise(signal) };
}

/// Installs handlers that restore the terminal and then die by the signal.
pub fn install_signal_guard() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        // SAFETY: zeroed sigaction then fully set before use.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fatal_signal as *const () as usize;
            action.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Puts the default dispositions back (used before returning to a shell-owned
/// state in tests and when the TUI leaves normally).
pub fn remove_signal_guard() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        // SAFETY: restoring the default action.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
}
