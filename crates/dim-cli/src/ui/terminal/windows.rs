//! Windows console plumbing. The console input and output modes outlive the
//! process (SetConsoleMode state is not reset on exit), so both are saved and
//! put back on every exit path, including Ctrl+C/Ctrl+Break and window close.
//!
//! Compile-verified only (see the stage-2 acceptance notes).

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

use super::modes::{self, MAX_RESTORE_LEN};

pub type RawFd = i32;
pub const STDIN: RawFd = 0;
pub const STDOUT: RawFd = 1;

static ORIGINAL: OnceLock<(u32, u32)> = OnceLock::new();
static RAW_ON: AtomicBool = AtomicBool::new(false);
static RESTORED: AtomicBool = AtomicBool::new(false);

fn handle(fd: RawFd) -> HANDLE {
    // SAFETY: GetStdHandle has no preconditions.
    unsafe {
        GetStdHandle(if fd == STDIN {
            STD_INPUT_HANDLE
        } else {
            STD_OUTPUT_HANDLE
        })
    }
}

pub fn is_tty(fd: RawFd) -> bool {
    let mut mode = 0u32;
    // SAFETY: valid out pointer.
    unsafe { GetConsoleMode(handle(fd), &mut mode) != 0 }
}

pub fn enable_raw_mode() -> io::Result<()> {
    let (input, output) = (handle(STDIN), handle(STDOUT));
    let (mut in_mode, mut out_mode) = (0u32, 0u32);
    // SAFETY: valid out pointers.
    unsafe {
        if GetConsoleMode(input, &mut in_mode) == 0 || GetConsoleMode(output, &mut out_mode) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let _ = ORIGINAL.set((in_mode, out_mode));
    let raw_in = (in_mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
        | ENABLE_VIRTUAL_TERMINAL_INPUT;
    let raw_out = out_mode
        | ENABLE_PROCESSED_OUTPUT
        | ENABLE_WRAP_AT_EOL_OUTPUT
        | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    // SAFETY: handles from GetStdHandle.
    unsafe {
        if SetConsoleMode(input, raw_in) == 0 || SetConsoleMode(output, raw_out) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    RAW_ON.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn disable_raw_mode() {
    if RAW_ON.swap(false, Ordering::SeqCst)
        && let Some(&(in_mode, out_mode)) = ORIGINAL.get()
    {
        // SAFETY: restoring the modes read earlier.
        unsafe {
            SetConsoleMode(handle(STDIN), in_mode);
            SetConsoleMode(handle(STDOUT), out_mode);
        }
    }
}

pub fn write_all(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let mut written = 0u32;
        // SAFETY: pointer/length from a live slice.
        let ok = unsafe {
            WriteFile(
                handle(fd),
                bytes.as_ptr().cast(),
                bytes.len().min(u32::MAX as usize) as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

/// Writes all of `bytes` to the console, giving up at `deadline`.
///
/// A console write cannot be interrupted from the writing thread, so the
/// write runs on a helper thread and this returns at the deadline; a write
/// still blocked there ends with the process, as the writer thread's does on
/// Unix.
pub fn write_until(fd: RawFd, bytes: &[u8], deadline: Instant) {
    let bytes = bytes.to_vec();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = write_all(fd, &bytes);
        let _ = done.send(());
    });
    let _ = finished.recv_timeout(deadline.saturating_duration_since(Instant::now()));
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    Data(usize),
    Timeout,
    Closed,
}

pub fn read_timeout(fd: RawFd, buf: &mut [u8], timeout: Duration) -> ReadOutcome {
    let input = handle(fd);
    let millis = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
    // SAFETY: waiting on the console input handle.
    let wait = unsafe { WaitForSingleObject(input, millis) };
    if wait != WAIT_OBJECT_0 {
        return ReadOutcome::Timeout;
    }
    let mut read = 0u32;
    // SAFETY: pointer/length from a live slice.
    let ok = unsafe {
        ReadFile(
            input,
            buf.as_mut_ptr().cast(),
            buf.len().min(u32::MAX as usize) as u32,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || read == 0 {
        ReadOutcome::Closed
    } else {
        ReadOutcome::Data(read as usize)
    }
}

pub fn window_size() -> Option<(u16, u16)> {
    // SAFETY: zeroed struct is a valid out-parameter.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(handle(STDOUT), &mut info) } == 0 {
        return None;
    }
    let columns = (info.srWindow.Right - info.srWindow.Left + 1) as u16;
    let rows = (info.srWindow.Bottom - info.srWindow.Top + 1) as u16;
    (columns > 0 && rows > 0).then_some((columns, rows))
}

/// Lets the next exit path write the restore sequence again (see unix.rs).
pub fn rearm_restore_guard() {
    RESTORED.store(false, Ordering::SeqCst);
}

/// Writes the mode restore sequence once and drops raw mode, giving the write
/// the exit bound (see unix.rs).
pub fn restore_terminal_once() {
    restore_terminal_once_until(Instant::now() + super::EXIT_BUDGET);
}

/// [`restore_terminal_once`] sharing the exit's deadline.
pub fn restore_terminal_once_until(deadline: Instant) {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut buf = [0u8; MAX_RESTORE_LEN];
    let len = modes::restore_into(modes::live_bits(), &mut buf);
    write_until(STDOUT, &buf[..len], deadline);
    disable_raw_mode();
}

unsafe extern "system" fn on_ctrl(_kind: u32) -> i32 {
    restore_terminal_once();
    0
}

pub fn install_signal_guard() {
    // SAFETY: registering a plain handler.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
}

pub fn remove_signal_guard() {
    // SAFETY: removing the handler registered above.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 0) };
}
