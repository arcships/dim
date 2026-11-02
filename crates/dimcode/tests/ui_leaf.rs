#![cfg(unix)]
//! End-to-end evidence for UI leaves (Tui design §3.6): the interactive
//! `dimcode` process is a rutis kernel in session with the daemon, running
//! its own `Tui`. It keeps the shell's terminal until it leaves and returns
//! the TUI's exit code; `ui.close` and daemon shutdown close the session,
//! the process restores the terminal and exits. A `Tui` that fails to load
//! ends the process nonzero, and with it the session.

#[path = "support/daemon.rs"]
#[allow(dead_code)]
mod daemon_cleanup;

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::{
        fd::{FromRawFd, OwnedFd, RawFd},
        unix::{
            fs::{DirBuilderExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Pty {
    master: RawFd,
    slave: RawFd,
}

impl Pty {
    fn new() -> Self {
        let mut master = -1;
        let mut slave = -1;
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(master, libc::TIOCSWINSZ, &size);
        }
        Self { master, slave }
    }

    fn write(&self, bytes: &[u8]) {
        let written = unsafe {
            libc::write(
                self.master,
                bytes.as_ptr().cast::<libc::c_void>(),
                bytes.len(),
            )
        };
        assert_eq!(written, bytes.len() as isize, "write terminal");
    }

    fn local_flags(&self) -> libc::tcflag_t {
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave, &mut termios) },
            0,
            "tcgetattr"
        );
        termios.c_lflag
    }

    /// Reads whatever the terminal shows for `duration`, so a TUI never
    /// blocks on a full buffer while the test waits for something else.
    fn drain(&self, duration: Duration) -> String {
        let deadline = Instant::now() + duration;
        let mut output = Vec::new();
        while Instant::now() < deadline {
            let mut poll = libc::pollfd {
                fd: self.master,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 20) } > 0 {
                let mut buffer = [0u8; 8192];
                let read = unsafe {
                    libc::read(
                        self.master,
                        buffer.as_mut_ptr().cast::<libc::c_void>(),
                        buffer.len(),
                    )
                };
                if read > 0 {
                    output.extend_from_slice(&buffer[..read as usize]);
                }
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    fn read_until(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            let mut poll = libc::pollfd {
                fd: self.master,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 50) };
            if ready > 0 {
                let mut buffer = [0u8; 8192];
                let read = unsafe {
                    libc::read(
                        self.master,
                        buffer.as_mut_ptr().cast::<libc::c_void>(),
                        buffer.len(),
                    )
                };
                if read > 0 {
                    output.extend_from_slice(&buffer[..read as usize]);
                    let text = String::from_utf8_lossy(&output);
                    if text.contains(needle) {
                        return text.into_owned();
                    }
                }
            }
        }
        panic!(
            "terminal output never contained {needle:?}: {}",
            String::from_utf8_lossy(&output)
        );
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.master);
            libc::close(self.slave);
        }
    }
}

struct Rpc {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Rpc {
    fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("daemon socket");
        let reader = BufReader::new(stream.try_clone().expect("socket clone"));
        Self {
            stream,
            reader,
            next: 0,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        writeln!(self.stream, "{request}").expect("write request");
        self.stream.flush().expect("flush request");
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read response");
            assert!(!line.is_empty(), "daemon closed while waiting for {method}");
            let message: Value = serde_json::from_str(&line).expect("json response");
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return message;
            }
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let response = self.call(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} failed: {response}"
        );
        response["result"].clone()
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn wait_socket(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("daemon socket did not appear at {}", path.display());
}

/// A product home with the offline model configuration and a Web build.
struct Home {
    root: PathBuf,
    home: PathBuf,
    assets: PathBuf,
}

impl Home {
    fn new(name: &str) -> Self {
        Self::in_root(PathBuf::from("/tmp").join(format!("dim-{name}-{}", uuid::Uuid::new_v4())))
    }

    /// A home whose canonical `rust/daemon.sock` is exactly `length` bytes.
    fn with_socket_length(length: usize) -> Self {
        let base = PathBuf::from("/tmp").join(format!(
            "dim-long-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&base).unwrap();
        let canonical = std::fs::canonicalize(&base).unwrap();
        let fixed = canonical.as_os_str().len() + "/".len() + "/home/rust/daemon.sock".len();
        assert!(
            length > fixed,
            "the temporary directory is already too long"
        );
        Self::in_root(base.join("p".repeat(length - fixed)))
    }

    fn in_root(root: PathBuf) -> Self {
        let home = root.join("home");
        private_dir(&home).unwrap();
        let assets = root.join("web");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(
            assets.join("index.html"),
            "<!doctype html><title>Dim</title>",
        )
        .unwrap();
        std::fs::write(
            assets.join("dimcode-web.json"),
            r#"{"version":"0.0.1","protocolVersion":3,"sourceCommit":"test"}"#,
        )
        .unwrap();
        Self { root, home, assets }
    }

    fn socket(&self) -> PathBuf {
        self.home.join("rust/daemon.sock")
    }

    /// Runs `dimcode` on `pty` through a shell that prints its exit code.
    fn dimcode(&self, pty: &Pty) -> Child {
        let binary = Path::new(env!("CARGO_BIN_EXE_dimcode"));
        let model_config =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../dim-agent-aimux/examples/offline.json");
        let stdio = || {
            let fd = unsafe { libc::dup(pty.slave) };
            assert!(fd >= 0, "dup terminal");
            Stdio::from(unsafe { OwnedFd::from_raw_fd(fd) })
        };
        let script = "\"$0\" --model-config \"$1\"; code=$?; printf 'SHELL-RETURNED:%s\\n' \"$code\"; exit \"$code\"";
        Command::new("sh")
            .args([
                "-c",
                script,
                binary.to_str().unwrap(),
                model_config.to_str().unwrap(),
            ])
            .env("DIMCODE_HOME", &self.home)
            .env("DIMCODE_WEB_ASSETS", &self.assets)
            .env("DIMCODE_OAUTH_MANUAL", "1")
            .env("DIMCODE_UPDATE_CHECK", "0")
            .env("LC_ALL", "C")
            .env("TERM", "xterm-256color")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio())
            .spawn()
            .expect("dimcode")
    }

    fn rpc(&self) -> Rpc {
        wait_socket(&self.socket());
        let mut rpc = Rpc::connect(&self.socket());
        rpc.ok(
            "hello",
            json!({"protocolVersion": 3, "clientKind": "cli", "clientName": "ui-leaf-test"}),
        );
        rpc
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        daemon_cleanup::stop_daemon(&self.home);
        daemon_cleanup::remove_directory(&self.root);
        if let Some(parent) = self.root.parent()
            && parent
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("dim-long-"))
        {
            daemon_cleanup::remove_directory(parent);
        }
    }
}

fn instances(rpc: &mut Rpc) -> Vec<Value> {
    rpc.ok("ui.status", json!({}))["instances"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Waits for an online leaf other than `known`, reading `pty` meanwhile.
fn wait_new_online(rpc: &mut Rpc, pty: &Pty, known: &[String]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        pty.drain(Duration::from_millis(25));
        if let Some(found) = instances(rpc)
            .into_iter()
            .find(|i| i["state"] == "online" && !known.iter().any(|id| i["uiId"] == id.as_str()))
        {
            return found;
        }
        assert!(Instant::now() < deadline, "no new leaf came online");
    }
}

fn wait_unregistered(rpc: &mut Rpc, pty: &Pty, ui_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while instances(rpc).iter().any(|i| i["uiId"] == ui_id) {
        pty.drain(Duration::from_millis(25));
        assert!(Instant::now() < deadline, "leaf {ui_id} stayed registered");
    }
}

/// Waits for the shell around `dimcode` to exit, reading the terminal so
/// no write blocks, and returns what it showed.
fn wait_exit(child: &mut Child, pty: &Pty) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut shown = String::new();
    while child.try_wait().unwrap().is_none() {
        shown.push_str(&pty.drain(Duration::from_millis(25)));
        assert!(Instant::now() < deadline, "dimcode did not exit: {shown:?}");
    }
    shown.push_str(&pty.drain(Duration::from_millis(100)));
    shown
}

fn assert_restored(pty: &Pty) {
    let flags = pty.local_flags();
    assert_ne!(
        flags & libc::ICANON as libc::tcflag_t,
        0,
        "terminal left raw"
    );
    assert_ne!(flags & libc::ECHO as libc::tcflag_t, 0, "echo not restored");
    assert_ne!(flags & libc::ISIG, 0, "signals not restored");
}

fn assert_raw(pty: &Pty) {
    assert_eq!(
        pty.local_flags() & libc::ICANON as libc::tcflag_t,
        0,
        "TUI did not enter raw mode"
    );
}

#[test]
fn normal_dimcode_is_the_leaf_until_it_leaves() {
    let home = Home::new("normal-leaf");
    let pty = Pty::new();
    let mut child = home.dimcode(&pty);
    let welcome = pty.read_until("Welcome to DIM");
    assert!(
        !welcome.contains("SHELL-RETURNED"),
        "the shell returned early"
    );
    let mut rpc = home.rpc();
    let leaf = wait_new_online(&mut rpc, &pty, &[]);
    let ui_id = leaf["uiId"].as_str().unwrap().to_owned();
    let first_session = leaf["sessionId"].as_str().unwrap().to_owned();
    assert!(ui_id.starts_with("ui-"), "{leaf}");
    pty.write(b"leaf-input-proof");
    pty.read_until("leaf-input-proof");
    pty.write(b"\x15");

    for want in [true, false] {
        pty.write(b"/web\r");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pty.drain(Duration::from_millis(25));
            let status = rpc.ok("web.status", json!({}));
            if status["enabled"] == want && status["open"] == want {
                break;
            }
            assert!(Instant::now() < deadline, "Web switch: {status}");
        }
    }
    // `/new` goes home; the first message starts the Session the leaf moves to.
    pty.write(b"/new\r");
    pty.read_until("new session");
    pty.write(b"leaf-switch-proof\r");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pty.drain(Duration::from_millis(25));
        let listed = instances(&mut rpc);
        assert_eq!(
            listed.len(),
            1,
            "a Session switch added an interface: {listed:?}"
        );
        if listed[0]["uiId"] == ui_id.as_str()
            && listed[0]["sessionId"].as_str() != Some(first_session.as_str())
        {
            break;
        }
        assert!(Instant::now() < deadline, "Session switch did not reattach");
    }
    pty.drain(Duration::from_millis(300));
    pty.write(b"/exit");
    pty.read_until("/exit");
    pty.write(b"\r");
    let shown = wait_exit(&mut child, &pty);
    assert!(shown.contains("SHELL-RETURNED:0"), "{shown:?}");
    assert_restored(&pty);
    wait_unregistered(&mut rpc, &pty, &ui_id);
    let _ = rpc.call("daemon.shutdown", json!({}));
}

#[test]
fn the_daemon_closes_leaves_and_restores_their_terminals() {
    let home = Home::new("ui-leaf");

    let first_pty = Pty::new();
    let mut first = home.dimcode(&first_pty);
    first_pty.read_until("Welcome to DIM");
    let mut rpc = home.rpc();
    let first_id = wait_new_online(&mut rpc, &first_pty, &[])["uiId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_raw(&first_pty);
    rpc.ok(
        "ui.close",
        json!({"uiId": first_id, "reason": "closed by test"}),
    );
    let shown = wait_exit(&mut first, &first_pty);
    assert!(shown.contains("SHELL-RETURNED:0"), "{shown:?}");
    assert_restored(&first_pty);
    assert!(instances(&mut rpc).is_empty());

    let second_pty = Pty::new();
    let mut second = home.dimcode(&second_pty);
    second_pty.read_until("Welcome to DIM");
    let second_id = wait_new_online(&mut rpc, &second_pty, std::slice::from_ref(&first_id))["uiId"]
        .as_str()
        .unwrap()
        .to_owned();
    let pid = second.id() as libc::pid_t;
    let dimcode = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .expect("pgrep");
    let dimcode: libc::pid_t = String::from_utf8_lossy(&dimcode.stdout)
        .trim()
        .parse()
        .expect("dimcode under the shell");
    unsafe { libc::kill(dimcode, libc::SIGSTOP) };
    rpc.ok("ui.close", json!({"uiId": second_id}));
    assert!(instances(&mut rpc).is_empty());
    unsafe { libc::kill(dimcode, libc::SIGCONT) };
    wait_exit(&mut second, &second_pty);
    assert_restored(&second_pty);

    let third_pty = Pty::new();
    let mut third = home.dimcode(&third_pty);
    third_pty.read_until("Welcome to DIM");
    let mut rpc = home.rpc();
    wait_new_online(&mut rpc, &third_pty, &[first_id, second_id]);
    assert_raw(&third_pty);
    let _ = rpc.call("daemon.shutdown", json!({}));
    wait_exit(&mut third, &third_pty);
    assert_restored(&third_pty);
}

#[test]
fn a_home_the_daemon_fits_also_fits_its_leaf() {
    let limit = std::mem::size_of::<libc::sockaddr_un>()
        - std::mem::offset_of!(libc::sockaddr_un, sun_path)
        - 1;
    let home = Home::with_socket_length(limit);
    let pty = Pty::new();
    let mut child = home.dimcode(&pty);
    pty.read_until("Welcome to DIM");
    let mut rpc = home.rpc();
    wait_new_online(&mut rpc, &pty, &[]);
    pty.drain(Duration::from_millis(300));
    pty.write(b"/exit");
    pty.read_until("/exit");
    pty.write(b"\r");
    let shown = wait_exit(&mut child, &pty);
    assert!(shown.contains("SHELL-RETURNED:0"), "{shown:?}");
    let _ = rpc.call("daemon.shutdown", json!({}));
}

/// A daemon that opens the leaf's session and replies to `ui.open`, then
/// drops the connection the leaf's `Tui` opens, so that `Tui` fails to load.
/// Returns whether the leaf's session ended.
async fn refusing_daemon(
    listener: std::os::unix::net::UnixListener,
    model_config: PathBuf,
) -> bool {
    use rutis::host::{CtxHost, FiberState};
    use rutis_bridge::{LocalId, PeerId, Session};
    use sha2::Digest;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    struct Shut(std::os::unix::net::UnixStream);
    impl rutis_bridge::channel::Closer for Shut {
        fn close(&self, _: rutis_bridge::Close) {
            let _ = self.0.shutdown(std::net::Shutdown::Both);
        }
    }

    let profile = format!(
        "{:x}",
        sha2::Sha256::digest(std::fs::read(model_config).unwrap())
    );
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::UnixListener::from_std(listener).unwrap();
    let root = rutis::Ctx::root().unwrap();
    root.provide(LocalId(PeerId::new("dim-daemon").unwrap()))
        .unwrap();
    let (read, mut write) = listener.accept().await.unwrap().0.into_split();
    let mut lines = tokio::io::BufReader::new(read).lines();
    let session = loop {
        let line = lines.next_line().await.unwrap().expect("a request");
        let request: Value = serde_json::from_str(&line).unwrap();
        let mut session = None;
        let result = match request["method"].as_str().unwrap() {
            "hello" => json!({"instanceId": "fake", "serverVersion": env!("CARGO_PKG_VERSION"),
                "protocolVersion": 3,
                "daemonProfile": {"kind": "configured", "modelConfigSha256": profile},
                "features": ["session.create", "session.history", "ui.attach", "ui.leaf.2"]}),
            "session.create" => json!({"sessionId": "session", "sequence": 0}),
            "session.attach" => json!({"sessionId": "session", "cwd": "/workspace", "createdAt": 1,
                "running": false, "sequence": 0, "attachedClients": 1}),
            "ui.open" => {
                let path = request["params"]["address"].as_str().unwrap();
                let stream =
                    std::os::unix::net::UnixStream::connect(path.strip_prefix("unix:").unwrap())
                        .unwrap();
                let reader = stream.try_clone().unwrap();
                let closer = std::sync::Arc::new(Shut(stream.try_clone().unwrap()));
                let channel = rutis_bridge::pipe::framed(reader, stream, closer);
                let greeted = Session::handshake(&root, channel).await.unwrap();
                assert_eq!(greeted.id(), request["params"]["peer"]);
                let view = root.plugin(greeted);
                (&view).await.unwrap();
                session = Some(view);
                json!({"uiId": request["params"]["peer"], "generation": 1, "token": "token"})
            }
            other => panic!("unexpected request {other}"),
        };
        let reply = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
        write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
        if let Some(session) = session {
            break session;
        }
    };
    drop(listener.accept().await.unwrap());
    let mut state = session.watch();
    let ended = tokio::time::timeout(
        Duration::from_secs(15),
        state.wait_for(|s| matches!(s.state, FiberState::Disposed | FiberState::Failed)),
    )
    .await
    .is_ok();
    let _ = root.shutdown().await;
    ended
}

#[test]
fn a_leaf_whose_tui_fails_to_load_exits_nonzero_and_ends_its_session() {
    let home = Home::new("tui-fails");
    private_dir(&home.home.join("rust")).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(home.socket()).unwrap();
    std::fs::set_permissions(home.socket(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let model_config =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../dim-agent-aimux/examples/offline.json");
    let daemon = std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(refusing_daemon(listener, model_config))
    });
    let pty = Pty::new();
    let mut child = home.dimcode(&pty);
    let shown = wait_exit(&mut child, &pty);
    assert!(
        shown.contains("SHELL-RETURNED:") && !shown.contains("SHELL-RETURNED:0"),
        "{shown:?}"
    );
    assert_restored(&pty);
    assert!(daemon.join().unwrap(), "the leaf's session did not end");
}
