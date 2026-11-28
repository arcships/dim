use crate::Result;
use std::io::{self, IsTerminal, Write};

#[cfg(unix)]
pub struct Input {
    fd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    buffer: Vec<u8>,
}

#[cfg(unix)]
impl Input {
    pub fn new() -> Result<Self> {
        use std::os::fd::FromRawFd;
        if !io::stdin().is_terminal() {
            return Err("This operation requires an interactive terminal".into());
        }
        let mut name = [0; libc::PATH_MAX as usize];
        #[cfg(target_os = "macos")]
        if unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETPATH, name.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        #[cfg(not(target_os = "macos"))]
        {
            let error =
                unsafe { libc::ttyname_r(libc::STDIN_FILENO, name.as_mut_ptr(), name.len()) };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error).into());
            }
        }
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if fd == -1 {
            return Err(io::Error::last_os_error().into());
        }
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        // SAFETY: AsyncFd owns the descriptor; Input never closes or replaces it while registered.
        let fd = unsafe { tokio::io::unix::AsyncFd::register(fd) }.map_err(io::Error::from)?;
        Ok(Self {
            fd,
            buffer: Vec::new(),
        })
    }

    pub fn prompt(&mut self, text: &str) -> Result<()> {
        use std::os::fd::AsRawFd;
        self.buffer.clear();
        if unsafe { libc::tcflush(self.fd.get_ref().as_raw_fd(), libc::TCIFLUSH) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        print!("{text}");
        io::stdout().flush()?;
        Ok(())
    }

    pub async fn line(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8(self.buffer[..index].to_vec())
                    .map_err(|_| "Terminal input must be UTF-8")?;
                self.buffer.drain(..=index);
                return Ok(Some(line.trim_end_matches('\r').into()));
            }
            if self.buffer.len() > 1024 * 1024 {
                return Err("Terminal input exceeds 1 MiB".into());
            }
            match self.read_bytes().await? {
                None => {
                    return if self.buffer.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(
                            String::from_utf8(std::mem::take(&mut self.buffer))
                                .map_err(|_| "Terminal input must be UTF-8")?,
                        ))
                    };
                }
                Some(bytes) => self.buffer.extend(bytes),
            }
        }
    }

    pub async fn read_bytes(&mut self) -> Result<Option<Vec<u8>>> {
        use std::os::fd::AsRawFd;
        loop {
            let mut ready = self.fd.readable().await?;
            let read = ready.try_io(|fd| {
                let mut bytes = [0u8; 4096];
                let size = unsafe {
                    libc::read(
                        fd.get_ref().as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                if size < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok((bytes, size as usize))
                }
            });
            match read {
                Ok(Ok((_, 0))) => return Ok(None),
                Ok(Ok((bytes, size))) => return Ok(Some(bytes[..size].to_vec())),
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => {}
            }
        }
    }

    pub fn editor(&mut self) -> Result<EditorMode> {
        use std::os::fd::AsRawFd;
        self.buffer.clear();
        let fd = self.fd.get_ref().as_raw_fd();
        let mut previous = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut previous) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        let mut raw = previous;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INLCR | libc::IGNCR | libc::ISTRIP);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        let guard = EditorMode { fd, previous };
        if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        io::stdout().write_all(b"\x1b[?2004h")?;
        io::stdout().flush()?;
        Ok(guard)
    }

    pub fn size(&self) -> Result<(usize, usize)> {
        use std::os::fd::AsRawFd;
        let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
        if unsafe { libc::ioctl(self.fd.get_ref().as_raw_fd(), libc::TIOCGWINSZ, &mut size) } == -1
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok((
            if size.ws_col == 0 {
                80
            } else {
                size.ws_col as usize
            },
            if size.ws_row == 0 {
                24
            } else {
                size.ws_row as usize
            },
        ))
    }

    pub fn hide_echo(&self) -> Result<EchoGuard> {
        use std::os::fd::AsRawFd;
        let fd = self.fd.get_ref().as_raw_fd();
        let mut previous = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut previous) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        let mut hidden = previous;
        hidden.c_lflag &= !libc::ECHO;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(EchoGuard { fd, previous })
    }
}

#[cfg(unix)]
pub struct EditorMode {
    fd: std::os::fd::RawFd,
    previous: libc::termios,
}
#[cfg(unix)]
impl Drop for EditorMode {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.previous) };
        let _ = io::stdout().write_all(b"\x1b[?2004l");
        let _ = io::stdout().flush();
    }
}

#[cfg(unix)]
pub struct EchoGuard {
    fd: std::os::fd::RawFd,
    previous: libc::termios,
}
#[cfg(unix)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.previous);
        }
    }
}

#[cfg(windows)]
pub struct Input {
    handle: windows_sys::Win32::Foundation::HANDLE,
    mode: u32,
    buffer: Vec<u16>,
    echo: std::rc::Rc<std::cell::Cell<bool>>,
}
#[cfg(windows)]
impl Input {
    pub fn new() -> Result<Self> {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
            SetConsoleMode,
        };
        if !io::stdin().is_terminal() {
            return Err("This operation requires an interactive terminal".into());
        }
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0
            || unsafe { SetConsoleMode(handle, mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT)) }
                == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            handle,
            mode,
            buffer: Vec::new(),
            echo: std::rc::Rc::new(std::cell::Cell::new(true)),
        })
    }
    pub fn prompt(&mut self, text: &str) -> Result<()> {
        self.buffer.clear();
        if unsafe { windows_sys::Win32::System::Console::FlushConsoleInputBuffer(self.handle) } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        print!("{text}");
        io::stdout().flush()?;
        Ok(())
    }
    pub async fn line(&mut self) -> Result<Option<String>> {
        use windows_sys::Win32::System::Console::{
            GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
        };
        loop {
            let mut count = 0;
            if unsafe { GetNumberOfConsoleInputEvents(self.handle, &mut count) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            if count == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            let mut event = INPUT_RECORD::default();
            if unsafe { ReadConsoleInputW(self.handle, &mut event, 1, &mut count) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            if count == 0 || event.EventType != KEY_EVENT as u16 {
                continue;
            }
            let key = unsafe { event.Event.KeyEvent };
            if key.bKeyDown == 0 {
                continue;
            }
            let value = unsafe { key.uChar.UnicodeChar };
            for _ in 0..key.wRepeatCount {
                match value {
                    13 => {
                        if self.echo.get() {
                            println!();
                        }
                        return Ok(Some(
                            String::from_utf16(&std::mem::take(&mut self.buffer))
                                .map_err(|_| "Terminal input must be valid Unicode")?,
                        ));
                    }
                    4 | 26 if self.buffer.is_empty() => return Ok(None),
                    8 => {
                        if let Some(last) = self.buffer.pop() {
                            if (0xdc00..=0xdfff).contains(&last) {
                                self.buffer.pop();
                            }
                            if self.echo.get() {
                                print!("\u{8} \u{8}");
                                io::stdout().flush()?;
                            }
                        }
                    }
                    value if value >= 32 => {
                        if self.buffer.len() >= 1024 * 1024 {
                            return Err("Terminal input exceeds 1 MiB".into());
                        }
                        self.buffer.push(value);
                        if self.echo.get() && !(0xd800..=0xdbff).contains(&value) {
                            let start = if (0xdc00..=0xdfff).contains(&value) {
                                self.buffer.len().saturating_sub(2)
                            } else {
                                self.buffer.len() - 1
                            };
                            print!(
                                "{}",
                                crate::sanitize::safe(&String::from_utf16_lossy(
                                    &self.buffer[start..]
                                ))
                            );
                            io::stdout().flush()?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    pub fn hide_echo(&self) -> Result<EchoGuard> {
        self.echo.set(false);
        Ok(EchoGuard(self.echo.clone()))
    }

    pub async fn read_bytes(&mut self) -> Result<Option<Vec<u8>>> {
        use windows_sys::Win32::System::Console::{
            GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
            WINDOW_BUFFER_SIZE_EVENT,
        };
        loop {
            let mut count = 0;
            if unsafe { GetNumberOfConsoleInputEvents(self.handle, &mut count) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            if count == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            let mut records = [INPUT_RECORD::default(); 128];
            if unsafe {
                ReadConsoleInputW(
                    self.handle,
                    records.as_mut_ptr(),
                    records.len() as u32,
                    &mut count,
                )
            } == 0
            {
                return Err(io::Error::last_os_error().into());
            }
            let mut resized = false;
            for event in &records[..count as usize] {
                if event.EventType == WINDOW_BUFFER_SIZE_EVENT as u16 {
                    resized = true;
                } else if event.EventType == KEY_EVENT as u16 {
                    let key = unsafe { event.Event.KeyEvent };
                    if key.bKeyDown != 0 || key.wVirtualKeyCode == 0x12 {
                        let value = unsafe { key.uChar.UnicodeChar };
                        if value != 0 || key.wVirtualScanCode == 0 {
                            self.buffer
                                .extend(std::iter::repeat_n(value, key.wRepeatCount as usize));
                        }
                    }
                }
            }
            if !self.buffer.is_empty()
                && !self
                    .buffer
                    .last()
                    .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
            {
                return Ok(Some(
                    String::from_utf16(&std::mem::take(&mut self.buffer))
                        .map_err(|_| "Terminal input must be valid Unicode")?
                        .into_bytes(),
                ));
            }
            if resized {
                return Ok(Some(Vec::new()));
            }
        }
    }

    pub fn editor(&mut self) -> Result<EditorMode> {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
            FlushConsoleInputBuffer, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
            SetConsoleMode,
        };
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let (mut input_mode, mut output_mode) = (0, 0);
        if unsafe { GetConsoleMode(self.handle, &mut input_mode) } == 0
            || unsafe { GetConsoleMode(output, &mut output_mode) } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let guard = EditorMode {
            input: self.handle,
            input_mode,
            output,
            output_mode,
        };
        if unsafe {
            SetConsoleMode(
                self.handle,
                (input_mode | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT)
                    & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT),
            )
        } == 0
            || unsafe {
                SetConsoleMode(
                    output,
                    output_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | ENABLE_PROCESSED_OUTPUT,
                )
            } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        self.buffer.clear();
        if unsafe { FlushConsoleInputBuffer(self.handle) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        io::stdout().write_all(b"\x1b[?2004h")?;
        io::stdout().flush()?;
        Ok(guard)
    }

    pub fn size(&self) -> Result<(usize, usize)> {
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
        };
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok((
            (info.srWindow.Right - info.srWindow.Left + 1) as usize,
            (info.srWindow.Bottom - info.srWindow.Top + 1) as usize,
        ))
    }
}

#[cfg(windows)]
pub struct EditorMode {
    input: windows_sys::Win32::Foundation::HANDLE,
    input_mode: u32,
    output: windows_sys::Win32::Foundation::HANDLE,
    output_mode: u32,
}
#[cfg(windows)]
impl Drop for EditorMode {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleMode(self.input, self.input_mode);
            windows_sys::Win32::System::Console::SetConsoleMode(self.output, self.output_mode);
        }
        let _ = io::stdout().write_all(b"\x1b[?2004l");
        let _ = io::stdout().flush();
    }
}
#[cfg(windows)]
pub struct EchoGuard(std::rc::Rc<std::cell::Cell<bool>>);
#[cfg(windows)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        self.0.set(true);
    }
}
#[cfg(windows)]
impl Drop for Input {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleMode(self.handle, self.mode);
        }
    }
}

#[cfg(all(test, windows))]
mod windows_flow {
    use super::*;
    use crate::composer::{self, Draft};
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::{
        io::Read,
        sync::mpsc,
        time::{Duration, Instant},
    };

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

    #[test]
    fn conpty_keyboard_and_paste_keep_the_editor_contract() {
        if std::env::var_os("DIM_CLI_CONSOLE_CHILD").is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut interrupts = tokio::signal::windows::ctrl_c().unwrap();
                let mut input = Input::new().unwrap();
                let mut draft = Draft::default();
                for (stage, expected) in [(1, "ABC"), (2, "after paste"), (3, "after clear")] {
                    println!("CONSOLE_STAGE_{stage}");
                    assert!(
                        composer::edit(
                            &mut input,
                            &mut draft,
                            None,
                            &std::env::temp_dir(),
                            None,
                            &mut interrupts
                        )
                        .await
                        .unwrap()
                    );
                    assert_eq!(draft.plain(), Some(expected));
                    draft.clear();
                }
                println!("CONSOLE_COMPLETE");
            });
            return;
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "input::windows_flow::conpty_keyboard_and_paste_keep_the_editor_contract",
            "--nocapture",
        ]);
        command.env("DIM_CLI_CONSOLE_CHILD", "1");
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
        until(&receive, &mut output, &mut consumed, "\x1b[6n");
        writer.write_all(b"\x1b[1;1R").unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, "CONSOLE_STAGE_1");
        until(&receive, &mut output, &mut consumed, "Ctrl+J: newline");
        writer
            .write_all("A中e\u{301}👩‍💻🇨🇳\x7f\x7f\x7f\x1b[D\x1b[3~B\x1b[CC\r".as_bytes())
            .unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, "CONSOLE_STAGE_2");
        until(&receive, &mut output, &mut consumed, "Ctrl+J: newline");
        writer
            .write_all(b"\x1b[200~one\r\n/exit\n\x1b[201~")
            .unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, "[Paste 1: 2 lines]");
        writer.write_all(b"\x7fafter paste\r").unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, "CONSOLE_STAGE_3");
        until(&receive, &mut output, &mut consumed, "Ctrl+J: newline");
        writer.write_all(b"one\ntwo").unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, "two");
        writer.write_all(b"\x03after clear\r").unwrap();
        writer.flush().unwrap();
        until(&receive, &mut output, &mut consumed, "CONSOLE_COMPLETE");
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
