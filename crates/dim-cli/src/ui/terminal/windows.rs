//! Windows console plumbing. The console input and output modes outlive the
//! process (SetConsoleMode state is not reset on exit), so both are saved and
//! put back on every exit path, including Ctrl+C/Ctrl+Break and window close.
//!
//! Console text crosses as UTF-16 through the W console functions
//! (`WriteConsoleW` and `ReadConsoleW`), so the console code pages, which
//! belong to the console and are shared with every process attached to it,
//! are never read or changed. The Windows job runs the console flow
//! (`windows_flow::a_legacy_code_page_console_exchanges_frames_and_keys_as_utf8`).

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WRAP_AT_EOL_OUTPUT, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
    ReadConsoleW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
    WriteConsoleW,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

use super::modes::{self, MAX_RESTORE_LEN};

pub type RawFd = i32;
pub const STDIN: RawFd = 0;
pub const STDOUT: RawFd = 1;

/// Most UTF-16 units one `WriteConsoleW` call receives.
const CONSOLE_WRITE_UNITS: usize = 8192;
/// Most UTF-16 units one `ReadConsoleW` call asks for.
const CONSOLE_READ_UNITS: usize = 1024;

static ORIGINAL: OnceLock<(u32, u32)> = OnceLock::new();
static RAW_ON: AtomicBool = AtomicBool::new(false);
static RESTORED: AtomicBool = AtomicBool::new(false);
/// A high surrogate that ended a console read, waiting for its low half. A
/// read can end between the halves of a pair, so the half waits here for the
/// next read. `read_timeout` runs only on the TUI's event-loop thread, so one
/// slot is enough; the atomic keeps it sound without `static mut`. Zero when
/// no half is pending.
static SURROGATE: AtomicU32 = AtomicU32::new(0);

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

/// The handle is a console's, so the wide-character console functions apply.
fn is_console(handle: HANDLE) -> bool {
    let mut mode = 0u32;
    // SAFETY: valid out pointer.
    unsafe { GetConsoleMode(handle, &mut mode) != 0 }
}

pub fn is_tty(fd: RawFd) -> bool {
    is_console(handle(fd))
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
        if SetConsoleMode(input, raw_in) == 0 {
            return Err(io::Error::last_os_error());
        }
        if SetConsoleMode(output, raw_out) == 0 {
            let error = io::Error::last_os_error();
            SetConsoleMode(input, in_mode);
            return Err(error);
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

pub fn write_all(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let handle = handle(fd);
    if is_console(handle) {
        return write_console(handle, bytes);
    }
    let mut bytes = bytes;
    while !bytes.is_empty() {
        let mut written = 0u32;
        // SAFETY: pointer/length from a live slice.
        let ok = unsafe {
            WriteFile(
                handle,
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

/// Writes console text as UTF-16.
///
/// A console decodes bytes written with `WriteFile` with its output code page.
/// That page belongs to the console and is shared with every process attached
/// to it, so `WriteConsoleW`, which takes UTF-16, leaves it alone. Bytes that
/// are not UTF-8 become U+FFFD: a frame that fails to write reads as a lost
/// terminal, and a frame is sanitized text, not a byte stream a reader parses.
fn write_console(handle: HANDLE, bytes: &[u8]) -> io::Result<()> {
    let text = String::from_utf8_lossy(bytes);
    let mut chunk = [0u16; CONSOLE_WRITE_UNITS];
    let mut len = 0;
    for character in text.chars() {
        let mut pair = [0u16; 2];
        let encoded = character.encode_utf16(&mut pair);
        if len + encoded.len() > chunk.len() {
            write_units(handle, &chunk[..len])?;
            len = 0;
        }
        chunk[len..len + encoded.len()].copy_from_slice(encoded);
        len += encoded.len();
    }
    if len > 0 {
        write_units(handle, &chunk[..len])?;
    }
    Ok(())
}

/// Writes `units`, continuing after a short write. A chunk leaves room for the
/// two units of a surrogate pair, so a write never starts between its halves;
/// a console that stops between them anyway gets the low half alone, and this
/// returns its error.
fn write_units(handle: HANDLE, units: &[u16]) -> io::Result<()> {
    let mut start = 0;
    while start < units.len() {
        let mut written = 0u32;
        // SAFETY: pointer/length from a live slice.
        let ok = unsafe {
            WriteConsoleW(
                handle,
                units[start..].as_ptr(),
                (units.len() - start).min(u32::MAX as usize) as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        start += written as usize;
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
    if is_console(input) {
        return read_console(input, buf, Instant::now() + timeout);
    }
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

/// Reads console input as UTF-16 and returns it as UTF-8.
///
/// Each unit converts to at most three bytes, so `(buf.len() - 1) / 3` units
/// leave room for every one of them and for the one carried surrogate. A read
/// that yields only a high surrogate carries it and reads again within the
/// remaining timeout, so the caller never sees `Data(0)`.
fn read_console(input: HANDLE, buf: &mut [u8], deadline: Instant) -> ReadOutcome {
    debug_assert!(
        buf.len() >= 4,
        "a console read needs room for a carried surrogate"
    );
    let mut units = [0u16; CONSOLE_READ_UNITS];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let millis = left.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: waiting on the console input handle.
        if unsafe { WaitForSingleObject(input, millis) } != WAIT_OBJECT_0 {
            return ReadOutcome::Timeout;
        }
        let want = (buf.len().saturating_sub(1) / 3).clamp(1, CONSOLE_READ_UNITS);
        let mut read = 0u32;
        // SAFETY: pointer/length from a live slice.
        let ok = unsafe {
            ReadConsoleW(
                input,
                units.as_mut_ptr().cast(),
                want as u32,
                &mut read,
                std::ptr::null(),
            )
        };
        if ok == 0 || read == 0 {
            return ReadOutcome::Closed;
        }
        let len = console_text(&units[..read as usize], buf);
        if len > 0 {
            return ReadOutcome::Data(len);
        }
    }
}

/// Writes the UTF-8 of `units` into `buf` and returns its length.
///
/// A high surrogate at the end of the read waits in [`SURROGATE`] for the next
/// read, because a read can end between the halves of a pair; an unpaired
/// surrogate becomes U+FFFD. The unit bound in [`read_console`] leaves room
/// for every character, and a character that would not fit is dropped rather
/// than written past the buffer.
fn console_text(units: &[u16], buf: &mut [u8]) -> usize {
    let carried = SURROGATE.swap(0, Ordering::SeqCst) as u16;
    let mut end = units.len();
    if units.last().is_some_and(|unit| is_high_surrogate(*unit)) {
        SURROGATE.store(units[end - 1] as u32, Ordering::SeqCst);
        end -= 1;
    }
    let mut len = 0;
    let pending = std::iter::once(carried).filter(|unit| *unit != 0);
    for character in char::decode_utf16(pending.chain(units[..end].iter().copied())) {
        let character = character.unwrap_or(char::REPLACEMENT_CHARACTER);
        let mut encoded = [0u8; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        if len + bytes.len() <= buf.len() {
            buf[len..len + bytes.len()].copy_from_slice(bytes);
            len += bytes.len();
        }
    }
    len
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
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

#[cfg(all(test, windows))]
mod windows_flow {
    use super::*;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::{
        io::{Read, Write},
        sync::mpsc,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::System::Console::{
        GetConsoleCP, GetConsoleOutputCP, SetConsoleCP, SetConsoleOutputCP,
    };

    /// The OEM code page a US Windows console runs.
    const LEGACY_CODE_PAGE: u32 = 437;
    /// A frame line: box drawing, the separator the interface paints, CJK and
    /// an emoji outside the basic plane.
    const FRAME: &str = "╭─ provider · effort ─╮ 中文 👩‍💻";
    /// The frame's single-width runs. The parent matches these and not the
    /// emoji: ConPTY renders a zero-width joiner cluster for its own screen
    /// buffer and can lay it out differently across Windows builds, while the
    /// conversion treats every character alike.
    const FRAME_LABEL: &str = "╭─ provider · effort ─╮";
    const FRAME_CJK: &str = "中文";
    /// A second line, written through the bounded writer.
    const MARKER: &str = "· marker 中文 👩‍💻";
    /// The marker line's single-width run, matched for the same reason.
    const MARKER_LABEL: &str = "· marker";
    /// What the parent types twice: one non-BMP code point with no joiner, so
    /// its surrogate pair crosses the child's one-unit reads.
    const TYPED: &str = "é中🦀";
    const READY: &str = "CONSOLE_READY";
    const PART_ONE: &str = "CONSOLE_PART_ONE";
    const DONE: &str = "CONSOLE_DONE";
    const CHILD: &str = "DIM_CLI_CONSOLE_CODE_PAGE_CHILD";
    const FILTER: &str = concat!(
        "ui::terminal::windows::windows_flow::",
        "a_legacy_code_page_console_exchanges_frames_and_keys_as_utf8"
    );

    struct Child(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn until(
        receiver: &mpsc::Receiver<Vec<u8>>,
        output: &mut Vec<u8>,
        consumed: &mut usize,
        text: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(index) = output[*consumed..]
                .windows(text.len())
                .position(|bytes| bytes == text.as_bytes())
            {
                *consumed += index + text.len();
                return;
            }
            let bytes = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "Console did not show {text:?}: {error}; {}",
                        String::from_utf8_lossy(output)
                    )
                });
            output.extend(bytes);
        }
    }

    /// A frame and typed keys cross a console that runs a legacy OEM code page
    /// unchanged, and the console keeps that page throughout. The parent
    /// matches the frame's single-width runs; the emoji is written but not
    /// asserted, because ConPTY lays a joiner cluster out for itself.
    #[test]
    fn a_legacy_code_page_console_exchanges_frames_and_keys_as_utf8() {
        if std::env::var_os(CHILD).is_some() {
            child();
            return;
        }
        parent();
    }

    fn child() {
        // SAFETY: the console is attached; these set the private ConPTY
        // console's code pages to the ones this test wants to keep.
        unsafe {
            SetConsoleCP(LEGACY_CODE_PAGE);
            SetConsoleOutputCP(LEGACY_CODE_PAGE);
        }
        enable_raw_mode().expect("raw mode");
        write_all(STDOUT, FRAME.as_bytes()).expect("frame");
        write_until(
            STDOUT,
            format!("\r\n{MARKER}").as_bytes(),
            Instant::now() + Duration::from_secs(10),
        );
        say(READY);
        assert_eq!(read_text(4), TYPED, "one unit per read");
        say(PART_ONE);
        assert_eq!(read_text(256), TYPED, "many units per read");
        disable_raw_mode();
        // SAFETY: the console is attached; reading its own code pages.
        let pages = unsafe { (GetConsoleCP(), GetConsoleOutputCP()) };
        assert_eq!(
            pages,
            (LEGACY_CODE_PAGE, LEGACY_CODE_PAGE),
            "the console kept its code pages"
        );
        say(DONE);
    }

    fn say(marker: &str) {
        write_all(STDOUT, format!("\r\n{marker}\r\n").as_bytes()).expect("marker");
    }

    /// The keys typed since the last read, read in a buffer of `size` bytes.
    fn read_text(size: usize) -> String {
        let mut buf = vec![0u8; size];
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while seen.len() < TYPED.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match read_timeout(STDIN, &mut buf, left) {
                ReadOutcome::Data(n) => seen.extend_from_slice(&buf[..n]),
                ReadOutcome::Timeout => panic!(
                    "Console did not deliver {TYPED:?}: got {:?}",
                    String::from_utf8_lossy(&seen)
                ),
                ReadOutcome::Closed => panic!(
                    "Console input closed before {TYPED:?}: got {:?}",
                    String::from_utf8_lossy(&seen)
                ),
            }
        }
        String::from_utf8(seen).expect("the console text is UTF-8")
    }

    fn parent() {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args(["--exact", FILTER, "--nocapture"]);
        command.env(CHILD, "1");
        let mut child = Child(pair.slave.spawn_command(command).unwrap());
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (send, receive) = mpsc::channel();
        let reading = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        if send.send(bytes[..size].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let (mut output, mut consumed) = (Vec::new(), 0);
        // portable-pty creates the pseudoconsole with
        // PSUEDOCONSOLE_INHERIT_CURSOR, so conhost first asks this terminal for
        // the cursor position and renders nothing until the report arrives.
        until(&receive, &mut output, &mut consumed, "\x1b[6n");
        writer.write_all(b"\x1b[1;1R").unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, FRAME_LABEL);
        until(&receive, &mut output, &mut consumed, FRAME_CJK);
        until(&receive, &mut output, &mut consumed, MARKER_LABEL);
        until(&receive, &mut output, &mut consumed, READY);
        for marker in [PART_ONE, DONE] {
            writer.write_all(TYPED.as_bytes()).unwrap();
            writer.flush().unwrap();
            until(&receive, &mut output, &mut consumed, marker);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "Console child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(writer);
        drop(pair.master);
        reading.join().unwrap();
        assert!(status.success());
    }
}
