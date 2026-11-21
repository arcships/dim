#![allow(
    dead_code,
    unused_imports,
    reason = "shared by cli_flow, cli_tui_flow and cli_session_flow; each uses the helpers it needs"
)]

#[path = "daemon.rs"]
pub mod daemon_cleanup;

pub use dim_agent_capabilities::images::{MAX_BASE64_BYTES, MAX_EDGE, base64_len};
pub use serde_json::{Value, json};
pub use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub struct Home(pub PathBuf, pub Arc<AtomicBool>);
impl Home {
    pub fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("dim-cli-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("rust")).unwrap();
        fs::set_permissions(path.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(path.join("config.json"), r#"{"locale":"en"}"#).unwrap();
        Self::at(path)
    }
    pub fn at(path: PathBuf) -> Self {
        Self(path, Arc::new(AtomicBool::new(false)))
    }
    /// A home whose saved send mode is `queue`, for the flows that pin the
    /// queue route: the TUI's default is now steer, so an unsaved value would
    /// steer instead.
    pub fn queued() -> Self {
        let home = Self::new();
        fs::write(
            home.0.join("config.json"),
            r#"{"locale":"en","tuiDefaultSendMode":"queue"}"#,
        )
        .unwrap();
        home
    }
    /// Ends the CLI itself, as the last step of a flow. From here on a closed
    /// connection is the end the flow expects; before it, `Wire` fails on one.
    pub fn kill(&self, terminal: &Terminal) {
        self.1.store(true, Ordering::SeqCst);
        terminal.signal(libc::SIGKILL);
    }
    pub fn daemon(&self, flow: impl FnOnce(&mut Wire) + Send + 'static) -> thread::JoinHandle<()> {
        // Each flow owns the deliberate-kill mark. A prior flow's `kill` must
        // not make this one accept an unexpected close, and a flow's own kill
        // is only set while it runs: the tests join an earlier daemon before
        // starting the next, so nothing is pending here.
        self.1.store(false, Ordering::SeqCst);
        let path = self.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let ending = Arc::clone(&self.1);
        thread::spawn(move || {
            // A client that fails before connecting must fail the test, not
            // hang the run.
            let deadline = Instant::now() + HANG_GUARD;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "no client connected within {HANG_GUARD:?}"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut wire = Wire(BufReader::new(stream), ending, false);
            if let Err(payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| flow(&mut wire)))
                && !payload.is::<PeerGone>()
            {
                std::panic::resume_unwind(payload);
            }
        })
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dimcode"));
        command.args(["--home", self.0.to_str().unwrap()]);
        command.env("DIMCODE_UPDATE_CHECK", "0");
        command
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        daemon_cleanup::stop_daemon(&self.0);
        daemon_cleanup::remove_directory(&self.0);
    }
}

/// How long a step that should succeed may take before the test fails. CI
/// shares one machine between jobs, and a debug CLI starting under that load
/// can take several seconds; a passing step never waits this long.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// Bounds a peer frame that never arrives, so a broken path fails instead of
/// hanging; it is not a step's budget. A frame is due when the CLI and the
/// fixture's daemon reach it, and a debug CLI starting on a loaded machine can
/// take tens of seconds, so ten times the 30 s step budget keeps machine load
/// out of the result: the scripted daemon's socket reads, the deadline
/// `Wire::frame` retries within, and the terminal waits for what the CLI draws
/// or accepts (#807).
pub const HANG_GUARD: Duration = Duration::from_secs(300);

/// The process id `path` records, once the script that writes it has finished.
/// A redirect creates the file before `echo` or `printf` writes into it, so a
/// read that lands in between sees nothing; the writer ends its write with a
/// newline, so only a whole, newline-terminated line counts. `patience` is the
/// caller's own deadline, so moving a wait here never loosens it.
pub fn recorded_pid(path: &std::path::Path, what: &str, patience: Duration) -> i32 {
    let deadline = Instant::now() + patience;
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && let Some(line) = text.strip_suffix('\n')
            && let Ok(pid) = line.parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "waited for {what} in {}, which never held a whole pid",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// The test ended the CLI itself: the flow stops where it would answer a CLI
/// that is gone. `Home::kill` sets that expectation before the signal; without
/// it a closed connection fails the flow, so a CLI that exits on its own still
/// fails the daemon thread. A CLI that stays connected and silent is not gone;
/// that still fails at `HANG_GUARD`.
pub struct PeerGone;

pub struct Wire(pub BufReader<UnixStream>, pub Arc<AtomicBool>, pub bool);
impl Wire {
    pub fn new(stream: UnixStream) -> Self {
        Self(
            BufReader::new(stream),
            Arc::new(AtomicBool::new(false)),
            false,
        )
    }
    pub fn read(&mut self, method: &str) -> Value {
        loop {
            let value = self.frame();
            if value["method"] != method && self.2 && self.empty_state(&value) {
                continue;
            }
            assert_eq!(value["method"], method);
            return value;
        }
    }
    pub fn empty_state(&mut self, request: &Value) -> bool {
        let branch = request["params"]
            .get("branchId")
            .cloned()
            .unwrap_or(json!("main"));
        let through = request["params"]
            .get("through")
            .cloned()
            .unwrap_or(json!(8));
        let result = match request["method"].as_str() {
            Some("session.methods") => json!({"methods": [
                {"name":"session.input.pending","mutating":false},
                {"name":"session.exec.pending","mutating":false},
                {"name":"session.turn.get","mutating":false}
            ]}),
            Some("session.question.pending") => {
                json!({"branchId":branch,"through":through,"batches":[],"next":null})
            }
            Some("session.input.pending") => {
                json!({"branchId":branch,"through":through,"inputs":[],"next":null})
            }
            Some("session.exec.pending") => {
                json!({"branchId":branch,"through":through,"tasks":[],"next":null})
            }
            Some("session.turn.get") => {
                json!({"branchId":branch,"through":through,"turn":null,"output":null})
            }
            _ => return false,
        };
        self.reply(request, result);
        true
    }
    /// The next request, whichever it is: for requests whose order the
    /// flow does not fix.
    pub fn read_any(&mut self) -> Value {
        loop {
            let value = self.frame();
            if !self.2 || !self.empty_state(&value) {
                return value;
            }
        }
    }
    pub fn frame(&mut self) -> Value {
        let deadline = Instant::now() + HANG_GUARD;
        let mut line = String::new();
        loop {
            match self.0.read_line(&mut line) {
                Ok(0) => self.closed(format!("the CLI closed before a frame: {line:?}")),
                Ok(_) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "no frame within {HANG_GUARD:?}: {line:?}"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                    self.closed(format!("the CLI reset before a frame: {line:?}"));
                }
                Err(error) => panic!("{error}"),
            }
        }
        serde_json::from_str(&line).unwrap()
    }
    /// The request that arrives after the flow settled, or `None` when the CLI
    /// closed first. A client killed with a reply still unread resets the
    /// connection instead of ending it; both mean no request arrived.
    pub fn trailing_request(&mut self) -> Option<String> {
        let deadline = Instant::now() + HANG_GUARD;
        let mut line = String::new();
        loop {
            match self.0.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    if self.2 && self.empty_state(&request) {
                        line.clear();
                        continue;
                    }
                    return Some(line);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "the CLI neither closed nor sent a request: {line:?}"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return None,
                Err(error) => panic!("{error}"),
            }
        }
    }
    pub fn send(&mut self, value: Value) {
        let written = writeln!(self.0.get_mut(), "{value}").and_then(|()| self.0.get_mut().flush());
        match written {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                self.closed(format!("the CLI closed while the flow replied: {value}"));
            }
            Err(error) => panic!("{error}"),
        }
    }
    /// A closed connection while the flow waits: the end of the flow only when
    /// the test ended the CLI with `Home::kill`.
    pub fn closed(&self, message: String) -> ! {
        if self.1.load(Ordering::SeqCst) {
            std::panic::panic_any(PeerGone);
        }
        panic!("{message}");
    }
    pub fn reply(&mut self, request: &Value, mut result: Value) {
        if request["method"] == "session.history"
            && (request["params"]["latestTurn"] == true
                || request["params"].get("before").is_some())
            && result["done"] == true
            && result["events"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
            && result.get("nextBefore").is_none()
        {
            result["nextBefore"] = json!(1);
        }
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
    }
    pub fn notice(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    pub fn quiet(&mut self, duration: Duration) {
        let previous = self.0.get_ref().read_timeout().unwrap();
        let deadline = Instant::now() + duration;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            self.0.get_ref().set_read_timeout(Some(remaining)).unwrap();
            let mut line = String::new();
            match self.0.read_line(&mut line) {
                Ok(0) => self.closed(format!("the CLI closed during the quiet window: {line:?}")),
                Ok(_) => {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    assert!(
                        self.2 && self.empty_state(&request),
                        "Unexpected request: {request}"
                    );
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => self.closed(format!("quiet window: {error}")),
            }
        }
        self.0.get_ref().set_read_timeout(previous).unwrap();
    }
    pub fn start(&mut self, resume: bool) {
        self.hello(&[]);
        if !resume {
            self.created();
        }
        self.attached();
    }
    /// Answers `hello`, advertising `extra` methods besides the Session basics.
    pub fn hello(&mut self, extra: &[&str]) {
        let hello = self.read("hello");
        let version = if hello["params"]["clientName"] == "AcpGateway" {
            3
        } else {
            4
        };
        assert_eq!(hello["params"]["protocolVersion"], version);
        let mut features = vec![
            "session.create",
            "session.history",
            "session.history.latestTurn",
            "session.input.pending.blockPresence",
            "provider.list",
            "provider.models.query",
        ];
        let empty_methods = !extra.contains(&"session.methods");
        self.2 = empty_methods || hello["params"]["clientName"] == "AcpGateway";
        if empty_methods {
            features.push("session.methods");
        }
        features.extend(extra);
        self.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":version,"daemonProfile":{"kind":"product"},"features":features}));
    }
    pub fn created(&mut self) -> Value {
        let create = self.read("session.create");
        self.reply(&create, json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        create
    }
    pub fn attached(&mut self) {
        let attach = self.read("session.attach");
        self.reply(&attach, attach_record(&attach));
        let history = self.read("session.history");
        self.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        self.no_pending_questions(8);
        let idle = self.read("session.idle");
        self.reply(&idle, json!({"sequence":8}));
    }
    pub fn no_pending_questions(&mut self, through: u64) {
        let request = self.read("session.question.pending");
        assert_eq!(request["params"]["branchId"], "main");
        self.reply(
            &request,
            json!({"branchId":"main","through":through,"batches":[],"next":null}),
        );
    }
    /// Answers the context reading the interface asks for when it enters a
    /// Session: this fixture reports the method as unknown, as a Session
    /// that does not contribute it would.
    pub fn no_context(&mut self) {
        let request = self.read("session.context.usage");
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32601,"message":"Unknown method: session.context.usage",
            "data":{"appError":"unknown_method"}}}));
    }
    pub fn event(&mut self, sequence: u64, name: &str, data: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":format!("event-{sequence}"),"sequence":sequence,"create_at":1,"type":"step","name":name,"data":data}}));
    }
    pub fn end(&mut self, outcome: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"end","sequence":25,"create_at":1,"type":"turn","name":"end","data":outcome}}));
    }
    pub fn detach(&mut self) {
        let detach = self.read("session.detach");
        self.reply(&detach, json!({}));
    }
    /// Holds the connection open until the client leaves, answering nothing:
    /// a flow the test drives with notices instead of replies.
    pub fn hold(&mut self) {
        let mut line = String::new();
        loop {
            line.clear();
            match self.0.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    if self.2 {
                        self.empty_state(&request);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    }
}

/// The record `session.attach` replies for the Session it names: that
/// Session's `SessionInfo`, whose model follows its latest adopted
/// configuration rather than the creation model `session.summaries` reports until
/// #795 lands.
pub fn attach_record(request: &Value) -> Value {
    json!({
        "sessionId": request["params"]["sessionId"],
        "cwd": "/workspace",
        "model": {"providerId":"test","modelId":"model"},
        "createdAt": 1,
        "running": false,
        "sequence": 8,
        "attachedClients": 1,
    })
}

/// The public `context/configured` event: the adopted model, and the
/// preferences the configuration carries when it carries any.
pub fn configured_event(sequence: u64, model_id: &str, preferences: Value) -> Value {
    let mut data = json!({
        "timeline":"main","revision":"revision-1",
        "provider_id":"test","model_id":model_id
    });
    if preferences
        .as_object()
        .is_some_and(|preferences| !preferences.is_empty())
    {
        data["model_preferences"] = preferences;
    }
    json!({"id":format!("configured-{sequence}"),"sequence":sequence,"create_at":1,"type":"context","name":"configured","data":data})
}

pub fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + HANG_GUARD;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("CLI did not settle");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

/// A process a flow starts and must end. A `Child` does not end when it is
/// dropped, so a flow that fails between its spawn and the step that ends it
/// would leave it running after the test home is gone; this ends it there.
pub struct OwnedChild(pub Option<Child>);
impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Self {
        Self(Some(command.spawn().unwrap_or_else(|error| {
            panic!(
                "could not start {:?} {:?}: {error}",
                command.get_program(),
                command.get_args().collect::<Vec<_>>()
            )
        })))
    }
    pub fn id(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }
    /// Waits for the process to end, so its output can be read.
    pub fn wait(mut self) -> Output {
        wait(self.0.take().unwrap())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        // A process that already exited reports its status here; one that
        // did not is killed. Neither may panic: this runs while unwinding.
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The label this build prints, without the command name. A flow whose
/// expected text names the build reads it from here, so the expectation
/// follows the label the binary reports instead of assuming the mark alone.
pub fn printed_label(home: &Home) -> String {
    let output = home.command().arg("--version").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .strip_prefix("dimcode ")
        .expect("--version prints the command name and the build label")
        .to_owned()
}

/// The command reports the version in its own package's manifest, so a flow can
/// read back which version an installation is on. It resolves the link it was
/// started through, as the installed command does.
pub const REPORTS_VERSION: &str = r#"#!/bin/sh
target=$0
while [ -L "$target" ]; do
  link=$(readlink "$target")
  case $link in
    /*) target=$link ;;
    *) target=$(dirname "$target")/$link ;;
  esac
done
directory=$(cd "$(dirname "$target")" && pwd) || exit 1
while [ ! -f "$directory/package.json" ] && [ "$directory" != / ]; do directory=$(dirname "$directory"); done
sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$directory/package.json"
"#;

pub fn image_fixture(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

pub fn import_image(wire: &mut Wire, media: &str) -> (Value, Vec<u8>) {
    let request = wire.read("attachment.import");
    assert_eq!(request["params"]["image"], true);
    let bytes = fs::read(request["params"]["path"].as_str().unwrap()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let source = json!({"type":"file","fileId":id,"mediaType":media});
    wire.reply(
        &request,
        json!({"attachmentId":id,"path":"/owned/image","bytes":bytes.len(),"image":source}),
    );
    (source, bytes)
}

/// Deterministic RGB noise of `width`×`height` pixels.
pub fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut step = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for _ in 0..width as usize * height as usize {
        let (red, green, blue) = (step(), step(), step());
        pixels.extend([(red >> 24) as u8, (green >> 24) as u8, (blue >> 24) as u8]);
    }
    pixels
}

/// Deterministic noise: JPEG quality 100 keeps it over 5 MiB, and its long
/// edge is over the shared edge limit, so an entry has to scale it.
pub fn noise_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 100);
    encoder
        .encode(
            &noise_rgb(width, height),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    bytes
}

/// `noise_jpeg`, with a minimal EXIF APP1 segment whose one IFD0 entry is the
/// orientation: `3` turns the image 180° and `6` 90° clockwise.
pub fn oriented_jpeg(width: u32, height: u32, orientation: u16) -> Vec<u8> {
    let mut bytes = noise_jpeg(width, height);
    let mut exif = vec![0xFF, 0xE1, 0x00, 0x22];
    exif.extend(b"Exif\0\0");
    exif.extend(b"II\x2A\x00");
    exif.extend(8u32.to_le_bytes());
    exif.extend(1u16.to_le_bytes());
    exif.extend(0x0112u16.to_le_bytes());
    exif.extend(3u16.to_le_bytes());
    exif.extend(1u32.to_le_bytes());
    exif.extend(orientation.to_le_bytes());
    exif.extend([0, 0]);
    exif.extend(0u32.to_le_bytes());
    bytes.splice(2..2, exif);
    bytes
}

/// Deterministic noise as PNG: within the shared edge limit while its base64
/// is over the byte limit, so it is sent as a JPEG at the same size.
pub fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut bytes),
        &noise_rgb(width, height),
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    bytes
}

/// `text` without its ANSI escape sequences, so a painted row reads as the
/// text the user sees.
pub fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut previous = '\0';
                for c in chars.by_ref() {
                    if c == '\u{07}' || (previous == '\u{1b}' && c == '\\') {
                        break;
                    }
                    previous = c;
                }
            }
            _ => {}
        }
    }
    plain
}

/// A CLI running on its own pseudo-terminal. Dropping it before the CLI has
/// exited, as a failed flow does, ends the CLI's whole process group.
pub struct Terminal {
    pub master: Option<fs::File>,
    pub child: Child,
    pub transcript: String,
    pub utf8_pending: Vec<u8>,
    pub consumed: usize,
}
impl Terminal {
    pub fn editor(&mut self) {
        self.until("\u{1b}[?2004h");
        self.until("Ctrl+J: newline");
    }
    /// Launches under a pty with the line client; a dimcode chat in a pty
    /// otherwise opens the terminal UI.
    pub fn launch(mut command: Command) -> Self {
        if command.get_program() == env!("CARGO_BIN_EXE_dimcode") {
            command.arg("--no-tui");
        }
        Self::launch_tui(command)
    }
    pub fn resume_tui(mut command: Command) -> Self {
        command.args(["--resume", "session"]);
        Self::launch_tui(command)
    }
    pub fn launch_tui(mut command: Command) -> Self {
        let master = unsafe {
            libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        };
        assert!(master >= 0, "{}", std::io::Error::last_os_error());
        let master = unsafe { fs::File::from_raw_fd(master) };
        let fd = master.as_raw_fd();
        assert_eq!(unsafe { libc::grantpt(fd) }, 0);
        assert_eq!(unsafe { libc::unlockpt(fd) }, 0);
        let mut name = [0; 128];
        #[cfg(target_os = "macos")]
        let named = unsafe { libc::ioctl(fd, libc::TIOCPTYGNAME as _, name.as_mut_ptr()) };
        #[cfg(not(target_os = "macos"))]
        let named = unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) };
        assert_eq!(named, 0);
        let slave = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        assert!(slave >= 0, "{}", std::io::Error::last_os_error());
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self {
            master: Some(master),
            child: command.spawn().unwrap(),
            transcript: String::new(),
            utf8_pending: Vec::new(),
            consumed: 0,
        }
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::ioctl(
                    self.master.as_ref().unwrap().as_raw_fd(),
                    libc::TIOCSWINSZ,
                    &size,
                )
            },
            0
        );
    }
    pub fn record(&mut self, bytes: &[u8]) {
        self.utf8_pending.extend_from_slice(bytes);
        let complete = match std::str::from_utf8(&self.utf8_pending) {
            Ok(text) => text.len(),
            Err(error) => {
                assert!(error.error_len().is_none(), "non-UTF-8 terminal output");
                error.valid_up_to()
            }
        };
        self.transcript
            .push_str(std::str::from_utf8(&self.utf8_pending[..complete]).unwrap());
        self.utf8_pending.drain(..complete);
    }
    pub fn signal(&self, signal: libc::c_int) {
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    /// Bytes the child wrote that the terminal has not consumed yet. A pty
    /// master reports the queue on its other side through `TIOCOUTQ` on
    /// macOS, while `FIONREAD` there always answers zero; Linux has no
    /// `TIOCOUTQ` for terminals and answers through `FIONREAD`.
    pub fn unread(&self) -> libc::c_int {
        #[cfg(target_os = "macos")]
        const WAITING_BYTES: libc::c_ulong = 0x4004_7473; // TIOCOUTQ
        #[cfg(not(target_os = "macos"))]
        const WAITING_BYTES: libc::c_ulong = libc::FIONREAD as libc::c_ulong;
        let mut waiting: libc::c_int = 0;
        let fd = self.master.as_ref().unwrap().as_raw_fd();
        assert_eq!(unsafe { libc::ioctl(fd, WAITING_BYTES, &mut waiting) }, 0);
        waiting
    }
    /// The terminal's local mode. Both ends of a pty share one termios, so
    /// the master reads what raw mode set on the slave.
    pub fn local_mode(&self) -> libc::tcflag_t {
        let mut mode = unsafe { std::mem::zeroed::<libc::termios>() };
        let fd = self.master.as_ref().unwrap().as_raw_fd();
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut mode) }, 0);
        mode.c_lflag
    }
    pub fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            assert!(Instant::now() < deadline, "CLI did not settle");
            let mut bytes = [0u8; 4096];
            match self.master.as_mut().unwrap().read(&mut bytes) {
                Ok(size) if size > 0 => {
                    self.record(&bytes[..size]);
                    continue;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error)
                    if cfg!(target_os = "linux") && error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => panic!("{error}"),
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    /// Waits for the child without reading its output: the terminal stopped
    /// consuming it, as a suspended application's or a stalled remote
    /// session's terminal does, and stays that way until the child is gone.
    pub fn wait_without_reading(&mut self) -> ExitStatus {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI did not exit with its terminal unread: {}",
                self.transcript
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    /// Waits until the terminal has shown `expected` anywhere, for a frame
    /// whose place among the frames already consumed the flow does not fix.
    pub fn until_anywhere(&mut self, expected: &str) {
        let consumed = std::mem::take(&mut self.consumed);
        self.until(expected);
        self.consumed = consumed;
    }
    pub fn until(&mut self, expected: &str) {
        self.wait_for(expected, |tail| {
            tail.find(expected).map(|index| index + expected.len())
        });
    }
    /// Waits until the text the user reads has shown `expected`. A row an
    /// attribute splits into runs, as a highlighted token is, does not hold
    /// its own text contiguously, so a wait on what the row says reads the
    /// transcript without its escape sequences.
    pub fn until_plain(&mut self, expected: &str) {
        let wanted = expected.to_owned();
        self.wait_for(expected, move |tail| {
            strip_ansi(tail).contains(&wanted).then_some(tail.len())
        });
    }
    /// Waits until the row labelled `label` shows `value`: a panel paints a
    /// row's value in the same write as its label, so the text that follows
    /// the label is the row's loaded state.
    pub fn until_row(&mut self, label: &str, value: &str) {
        let expected = format!("{label:?} showing {value:?}");
        self.wait_for(&expected, |tail| {
            let mut from = 0;
            while let Some(index) = tail[from..].find(label) {
                let end = from + index + label.len();
                if strip_ansi(&tail[end..]).trim_start().starts_with(value) {
                    return Some(end);
                }
                from = end;
            }
            None
        });
    }
    /// Reads until `found` names how far into the unconsumed transcript the
    /// wait is satisfied.
    pub fn wait_for(&mut self, expected: &str, found: impl Fn(&str) -> Option<usize>) {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            if let Some(end) = found(&self.transcript[self.consumed..]) {
                self.consumed += end;
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Terminal did not show {expected:?}: {}",
                self.transcript
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_ref().unwrap().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 100) } == 0 {
                continue;
            }
            let mut bytes = [0u8; 4096];
            let size = match self.master.as_mut().unwrap().read(&mut bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error)
                    if cfg!(target_os = "linux") && error.raw_os_error() == Some(libc::EIO) =>
                {
                    0
                }
                result => result.unwrap(),
            };
            assert!(
                size > 0,
                "Terminal closed before {expected}: {}",
                self.transcript
            );
            self.record(&bytes[..size]);
        }
    }
    pub fn write(&mut self, bytes: &[u8]) {
        self.raw(
            &bytes
                .iter()
                .map(|&byte| if byte == b'\n' { b'\r' } else { byte })
                .collect::<Vec<_>>(),
        );
    }
    pub fn raw(&mut self, mut bytes: &[u8]) {
        let deadline = Instant::now() + HANG_GUARD;
        while !bytes.is_empty() {
            assert!(Instant::now() < deadline, "Terminal did not accept input");
            match self.master.as_mut().unwrap().write(bytes) {
                Ok(0) => panic!("Terminal input closed"),
                Ok(size) => bytes = &bytes[size..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut output = [0u8; 4096];
                    match self.master.as_mut().unwrap().read(&mut output) {
                        Ok(0) => panic!("Terminal output closed while writing"),
                        Ok(size) => self.record(&output[..size]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("{error}"),
                    }
                    let mut poll = libc::pollfd {
                        fd: self.master.as_ref().unwrap().as_raw_fd(),
                        events: libc::POLLIN | libc::POLLOUT,
                        revents: 0,
                    };
                    assert!(unsafe { libc::poll(&mut poll, 1, 100) } >= 0);
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    pub fn paste(&mut self, text: &str) {
        self.raw(b"\x1b[200~");
        self.raw(text.as_bytes());
        self.raw(b"\x1b[201~");
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            drop(self.master.take());
            let _ = self.child.wait();
        }
    }
}

/// The command with the terminal already laid out `cols` wide by the time the
/// interface starts, so its first frame is drawn at that width.
pub fn sized(command: &mut Command, cols: u16, rows: u16) {
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(command, move || {
            let size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if libc::ioctl(0, libc::TIOCSWINSZ, &size) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub struct OwnedDaemon(pub PathBuf);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let Ok(stream) = UnixStream::connect(&self.0) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut stream = BufReader::new(stream);
        for (id, method, params) in [
            (1, "hello", json!({"protocolVersion":4,"clientKind":"cli"})),
            (2, "daemon.shutdown", json!({})),
        ] {
            if writeln!(
                stream.get_mut(),
                "{}",
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            )
            .is_err()
            {
                return;
            }
            loop {
                let mut line = String::new();
                if stream
                    .read_line(&mut line)
                    .ok()
                    .filter(|size| *size > 0)
                    .is_none()
                {
                    return;
                }
                if serde_json::from_str::<Value>(&line)
                    .ok()
                    .is_some_and(|reply| reply["id"] == id)
                {
                    break;
                }
            }
        }
    }
}

/// Reads until the CLI closes the connection, failing on any catalog read.
pub fn no_catalog(wire: &mut Wire) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        assert!(
            !line.contains("session.summaries"),
            "the open read the Session catalog: {line}"
        );
        line.clear();
    }
}

pub fn picker_page(provider_id: &str, provider_name: &str, snapshot: Value) -> Value {
    let catalog = &snapshot["catalog"];
    let models: Vec<_> = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| {
            json!({
                "providerId":provider_id,"providerName":provider_name,"model":model,
                "version":catalog["version"],"preferences":{},
                "disabled":catalog["disabledModelIds"].as_array().unwrap().contains(&model["id"])
            })
        })
        .collect();
    json!({"models":models,"next":null,"empty":[]})
}
