//! A client and the App Server disagreeing about a protocol version.
//!
//! #1390: a client of a newer protocol reaches, replaces or stops its own
//! daemon of an older one. The daemon refuses a `hello` it cannot serve with a
//! typed `protocol_version` error and closes; only that refusal steps the
//! client one version down, no lower than v3.
//!
//! #1459: a Session the daemon refuses for its protocol version is shown with
//! the daemon's own text and no command to rerun, while a Session recorded at
//! an older version opens.
use super::daemon_replace::serve_successor;
use super::*;

/// The daemon's socket in `home`.
fn endpoint(home: &Home) -> PathBuf {
    home.0.join("rust/daemon.sock")
}

fn listen(home: &Home) -> UnixListener {
    let path = endpoint(home);
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

/// The next connection the CLI opens, or a failure naming the wait.
fn accept(listener: &UnixListener) -> Wire {
    let deadline = Instant::now() + HANG_GUARD;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
                return Wire::new(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "the CLI never connected");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

/// The connections the CLI opened that the flow never answered. Called after
/// the CLI exited, so a retry it made is already waiting.
fn no_connection_left(listener: &UnixListener) {
    match listener.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(_) => panic!("the CLI opened another connection"),
        Err(error) => panic!("{error}"),
    }
}

/// The refusal the transport sends a `hello` of another version, then the
/// close that ends the connection.
fn refuse(wire: &mut Wire) {
    let hello = wire.read("hello");
    wire.send(json!({"jsonrpc":"2.0","id":hello["id"],"error":{
        "code":-32000,"message":"Unsupported protocol version",
        "data":{"appError":"protocol_version"}}}));
    wire.0
        .get_mut()
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
}

/// The build of `alpha.21`, the daemon older than this client.
const ALPHA_21: &str = "0.1.0-alpha.21";

/// What a v3 daemon of `version` answers a `hello` it admits, with
/// `features`.
fn v3_hello(wire: &mut Wire, version: &str, features: &[&str]) {
    let hello = wire.read("hello");
    assert_eq!(hello["params"]["protocolVersion"], 3);
    wire.reply(
        &hello,
        json!({"instanceId":"alpha.21","serverVersion":version,"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}),
    );
}

/// The features a v3 daemon advertises that this client needs.
const V3_FEATURES: [&str; 5] = [
    "session.create",
    "session.history",
    "session.summaries",
    "daemon.shutdown.completed",
    "daemon.shutdown.ifIdle",
];

/// An idle daemon of an older protocol is reached, asked to shut down, and
/// replaced by this build's daemon.
#[test]
fn an_idle_v3_daemon_is_replaced_after_negotiating() {
    let home = Home::new();
    let listener = listen(&home);
    let socket = endpoint(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        drop(wire);
        let mut wire = accept(&listener);
        v3_hello(&mut wire, ALPHA_21, &V3_FEATURES);
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(
            shutdown["params"],
            json!({"ifIdle": true}),
            "a v3 daemon that offers the idle shutdown is asked only while idle"
        );
        fs::remove_file(&socket).unwrap();
        let successor = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        successor.set_nonblocking(true).unwrap();
        wire.reply(&shutdown, json!({}));
        wire.0
            .get_mut()
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        serve_successor(&successor);
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("The App Server is still"),
        "a replaced daemon says nothing: {stderr}"
    );
}

/// A busy daemon of an older protocol is not used: one notice, exit 1, and no
/// request of any kind after the refused idle shutdown.
#[test]
fn a_busy_v3_daemon_gets_one_notice_and_no_session() {
    let home = Home::new();
    let listener = listen(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        drop(wire);
        let mut wire = accept(&listener);
        v3_hello(&mut wire, ALPHA_21, &V3_FEATURES);
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({"ifIdle": true}));
        wire.send(json!({"jsonrpc":"2.0","id":shutdown["id"],"error":{
            "code":-32000,"message":"The App Server is in use",
            "data":{"appError":"daemon_busy","connections":2,"attachedSessions":1,"running":1}}}));
        assert_eq!(
            wire.trailing_request(),
            None,
            "a daemon of an older protocol must not serve a Session"
        );
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let expected = format!(
        "Error: The App Server is still 0.1.0-alpha.21 because other clients are using it \
         (connections: 2, attached Sessions: 1, running turns: 1), and it speaks protocol v3, \
         which this {} client cannot use. No Session was started. Run `dimcode daemon stop` when \
         they are done, and the next start runs {}.",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), expected);
    assert!(output.stdout.is_empty());
}

/// A daemon built from this version can still be older in protocol: the
/// version string alone does not make it this client's daemon.
#[test]
fn a_v3_daemon_of_this_version_is_not_reused() {
    let home = Home::new();
    let listener = listen(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        drop(wire);
        let mut wire = accept(&listener);
        v3_hello(&mut wire, env!("CARGO_PKG_VERSION"), &V3_FEATURES);
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({"ifIdle": true}));
        wire.send(json!({"jsonrpc":"2.0","id":shutdown["id"],"error":{
            "code":-32000,"message":"The App Server is in use",
            "data":{"appError":"daemon_busy","connections":2,"attachedSessions":1,"running":1}}}));
        assert_eq!(
            wire.trailing_request(),
            None,
            "a daemon of this version at an older protocol must not serve a Session"
        );
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let expected = format!(
        "Error: The App Server is still {} because other clients are using it \
         (connections: 2, attached Sessions: 1, running turns: 1), and it speaks protocol v3, \
         which this {} client cannot use. No Session was started. Run `dimcode daemon stop` when \
         they are done, and the next start runs {}.",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), expected);
    assert!(output.stdout.is_empty());
}

/// An idle daemon of this version at an older protocol is shut down and
/// replaced too: the successor speaks v4 and serves the command silently.
#[test]
fn an_idle_v3_daemon_of_this_version_is_replaced() {
    let home = Home::new();
    let listener = listen(&home);
    let socket = endpoint(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        drop(wire);
        let mut wire = accept(&listener);
        v3_hello(&mut wire, env!("CARGO_PKG_VERSION"), &V3_FEATURES);
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({"ifIdle": true}));
        fs::remove_file(&socket).unwrap();
        let successor = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        successor.set_nonblocking(true).unwrap();
        wire.reply(&shutdown, json!({}));
        wire.0
            .get_mut()
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        serve_successor(&successor);
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("The App Server is still"),
        "a replaced daemon says nothing: {stderr}"
    );
}

/// Serves a v3 daemon that refuses v4, admits v3, answers one plain
/// `daemon.shutdown {}` and closes, then runs `dimcode <arguments>` against
/// it and checks that the stop succeeded.
fn stop_a_v3_daemon(home: &Home, arguments: &[&str]) {
    let listener = listen(home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        drop(wire);
        let mut wire = accept(&listener);
        v3_hello(&mut wire, ALPHA_21, &V3_FEATURES);
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(
            shutdown["params"],
            json!({}),
            "an explicit stop is not conditional on the daemon being idle"
        );
        wire.reply(&shutdown, json!({}));
        wire.0
            .get_mut()
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        listener
    });
    let output = home.command().args(arguments).output().unwrap();
    let listener = daemon.join().unwrap();
    no_connection_left(&listener);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Daemon stopped.\n");
}

/// `daemon stop` reaches a daemon of an older protocol and asks it to stop
/// outright, not only while idle.
#[test]
fn daemon_stop_reaches_a_v3_daemon() {
    stop_a_v3_daemon(&Home::new(), &["daemon", "stop"]);
}

/// `daemon stop` never reads `--server`: the action addresses the home's
/// daemon, so a flag naming another binary cannot decide the outcome.
#[test]
fn daemon_stop_with_a_server_flag_reaches_a_v3_daemon() {
    let home = Home::new();
    let server = home.0.join("dim-app-server");
    fs::write(&server, "").unwrap();
    stop_a_v3_daemon(
        &home,
        &["--server", server.to_str().unwrap(), "daemon", "stop"],
    );
}

/// A refusal that is not the typed protocol one is final: the daemon keeps its
/// connection and the client reports what it said.
#[test]
fn a_refusal_that_is_not_protocol_version_stays_fatal() {
    let home = Home::new();
    let listener = listen(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        let hello = wire.read("hello");
        assert_eq!(hello["params"]["protocolVersion"], 4);
        wire.send(json!({"jsonrpc":"2.0","id":hello["id"],"error":{
            "code":-32000,"message":"The App Server is reloading",
            "data":{"appError":"service_reloading"}}}));
        assert_eq!(wire.trailing_request(), None, "no second hello");
        listener
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    let listener = daemon.join().unwrap();
    no_connection_left(&listener);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        "Error: The App Server is reloading (RPC -32000, service_reloading)"
    );
    assert_eq!(output.status.code(), Some(1));
}

/// The step-down stops at v3: a daemon that refuses v3 too is reported, and no
/// v2 attempt is made.
#[test]
fn negotiation_stops_at_the_floor() {
    let home = Home::new();
    let listener = listen(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        drop(wire);
        let mut wire = accept(&listener);
        refuse(&mut wire);
        listener
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    let listener = daemon.join().unwrap();
    no_connection_left(&listener);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        "Error: Unsupported protocol version (RPC -32000, protocol_version)"
    );
    assert_eq!(output.status.code(), Some(1));
}

/// A `--server` daemon is the operator's own binary: it is greeted once and a
/// refusal is reported as it is.
#[test]
fn a_server_daemon_at_a_lower_protocol_is_not_negotiated() {
    let home = Home::new();
    let server = home.0.join("dim-app-server");
    fs::write(&server, "").unwrap();
    let listener = listen(&home);
    let daemon = thread::spawn(move || {
        let mut wire = accept(&listener);
        refuse(&mut wire);
        listener
    });
    let output = home
        .command()
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let listener = daemon.join().unwrap();
    no_connection_left(&listener);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        "Error: Unsupported protocol version (RPC -32000, protocol_version)"
    );
    assert_eq!(output.status.code(), Some(1));
}

/// The refusal `authorize_version` sends for a Session the daemon will not
/// open for its protocol version.
fn refuse_session_version(wire: &mut Wire, request: &Value, message: &str) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
        "code":-32000,"message":message,"data":{"appError":"session_protocol_version"}}}));
}

#[test]
fn a_resumed_session_of_a_newer_protocol_names_both_versions_and_offers_no_retry() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.permission.get", "session.permission.update"]);
        let get = wire.read("session.permission.get");
        refuse_session_version(
            wire,
            &get,
            "Session was created by protocol version 5; this connection speaks 4",
        );
        assert_eq!(
            wire.trailing_request(),
            None,
            "a refused Session must not be adopted, attached or submitted"
        );
    });
    let mut command = home.command();
    command.args(["--permission", "read-only"]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("Session was created by protocol version 5; this connection speaks 4");
    terminal.until("Cannot open this Session; draft kept.");
    let status = terminal.wait();
    daemon.join().unwrap();
    let shown = strip_ansi(&terminal.transcript);
    assert_eq!(status.code(), Some(1), "{shown}");
    assert!(
        shown.contains("Session was created by protocol version 5; this connection speaks 4"),
        "{shown}"
    );
    assert!(
        !shown.contains("so this client cannot open it"),
        "the refusal must not lead with a claim of its own: {shown}"
    );
    assert!(
        !shown.contains("Cannot set the permission preset"),
        "{shown}"
    );
    assert!(!shown.contains("--resume"), "{shown}");
}

/// A Session recorded at v3, the version the previous build wrote, opens from
/// this build's CLI: `--resume` attaches it and shows the Session, where
/// equality refused it.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn a_resumed_session_of_an_older_protocol_opens() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let _daemon = OwnedDaemon(endpoint(&home));
    // The daemon runs the built App Server with its mock model, so a seed turn
    // needs no provider. The CLI finds it already serving and reuses it.
    let mut daemon = Command::new(&binary);
    daemon
        .args(["--mock-model", "--unix"])
        .arg(endpoint(&home))
        .args(["--no-ws", "--home"])
        .arg(home.0.join("rust"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = daemon.spawn().unwrap();
    let deadline = Instant::now() + PATIENCE;
    while !endpoint(&home).exists() {
        assert!(Instant::now() < deadline, "the mock daemon started serving");
        thread::sleep(Duration::from_millis(20));
    }
    let command = || {
        let mut command = home.command();
        command.args(["--server", &binary, "--cwd", home.0.to_str().unwrap()]);
        command
    };
    // A Session created by this build at v4, over the mock daemon's socket, so
    // the seed needs no provider and no second daemon.
    let stream = UnixStream::connect(endpoint(&home)).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = BufReader::new(stream);
    let mut rpc = |id: u64, method: &str, params: Value| {
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        wire.get_mut().flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(wire.read_line(&mut line).unwrap() > 0, "{method}: closed");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    };
    let hello = rpc(1, "hello", json!({"protocolVersion":4,"clientKind":"cli"}));
    assert!(hello.get("result").is_some(), "{hello}");
    let created = rpc(
        2,
        "session.create",
        json!({"cwd":home.0,"tools":[],"permissionPolicy":{"allowedTools":[]}}),
    );
    let session = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    drop(wire);
    let _ = daemon.kill();
    let _ = daemon.wait();
    let database = home.0.join("rust/dimcode.sqlite");
    let database = rusqlite::Connection::open(&database).unwrap();
    // The owner row a v3 build wrote: from here the Session is one this
    // build's protocol is newer than.
    database
        .execute(
            "UPDATE session_owners SET protocol_version=3 WHERE session_id=?1",
            [&session],
        )
        .unwrap();
    drop(database);
    let mut resume = command();
    resume.args(["--resume", &session]);
    let mut terminal = Terminal::launch_tui(resume);
    terminal.until("Ask anything...");
    let shown = strip_ansi(&terminal.transcript);
    assert!(
        !shown.contains("Session was created by protocol version"),
        "the resumed Session opens, so the daemon's refusal is not shown: {shown}"
    );
    assert!(!shown.contains("Cannot open this Session"), "{shown}");
    home.kill(&terminal);
    terminal.wait();
}
