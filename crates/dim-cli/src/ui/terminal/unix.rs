//! POSIX terminal plumbing: raw mode, timed reads, window size, and the
//! async-signal-safe exit path.
//!
//! The restore path is deliberately free of locks and allocation: the original
//! `termios` lives in a `OnceLock` (an atomic load to read) and the restore
//! sequence is built on the stack from the atomic mode set.

use std::io;
use std::os::fd::RawFd;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::modes::{self, MAX_RESTORE_LEN};

pub const STDIN: RawFd = 0;
pub const STDOUT: RawFd = 1;

static ORIGINAL: OnceLock<libc::termios> = OnceLock::new();
/// Raw mode is currently applied; the restore path skips tcsetattr otherwise.
static RAW_ON: AtomicBool = AtomicBool::new(false);
/// The restore sequence was already written by someone (exit, signal, panic).
static RESTORED: AtomicBool = AtomicBool::new(false);

pub fn is_tty(fd: RawFd) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// Switches the controlling input to raw mode, remembering the original
/// settings for every exit path.
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
    // Keep output post-processing: the renderer writes `\r\n` itself, but
    // stray `\n` from diagnostics should not stair-step.
    raw.c_oflag |= libc::OPOST;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    RAW_ON.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn disable_raw_mode() {
    if RAW_ON.swap(false, Ordering::SeqCst)
        && let Some(original) = ORIGINAL.get()
    {
        // SAFETY: restoring the exact settings read earlier.
        unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, original) };
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
                // A full pty buffer: wait for the reader to catch up.
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

/// Writes the mode restore sequence once and drops raw mode. Safe to call from
/// a signal handler: no locks, no allocation, only write(2)/tcsetattr(3).
pub fn restore_terminal_once() {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut buf = [0u8; MAX_RESTORE_LEN];
    let len = modes::restore_into(modes::live_bits(), &mut buf);
    // Best effort: the terminal may already be gone (EIO).
    // SAFETY: buf[..len] is initialized stack memory.
    unsafe { libc::write(STDOUT, buf.as_ptr().cast(), len) };
    disable_raw_mode();
}

/// Re-arms the once-guard (a TUI may be entered again in tests).
#[cfg(test)]
pub fn reset_for_test() {
    RESTORED.store(false, Ordering::SeqCst);
}

/// Lets the next exit path write the restore sequence again: the terminal was
/// handed back by a suspend and is now ours.
pub fn rearm_restore_guard() {
    RESTORED.store(false, Ordering::SeqCst);
}

/// Stops the process the way job control does, and returns once SIGCONT
/// continues it. No handler is installed for SIGTSTP, so the default action
/// (stop) applies.
pub fn raise_stop() {
    // SAFETY: raise only delivers a signal to this process.
    unsafe { libc::raise(libc::SIGTSTP) };
}

extern "C" fn on_fatal_signal(signal: libc::c_int) {
    restore_terminal_once();
    // SA_RESETHAND already restored the default action; with SA_NODEFER the
    // signal is not blocked, so this ends the process signal-shaped (130/129).
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
