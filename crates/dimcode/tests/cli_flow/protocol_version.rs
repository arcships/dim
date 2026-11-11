//! #1390: a client of a newer protocol reaches, replaces or stops its own
//! daemon of an older one. The daemon refuses a `hello` it cannot serve with a
//! typed `protocol_version` error and closes; only that refusal steps the
//! client one version down, no lower than v3.
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

/// What alpha.21 answers a `hello` it admits, with `features`.
fn v3_hello(wire: &mut Wire, features: &[&str]) {
    let hello = wire.read("hello");
    assert_eq!(hello["params"]["protocolVersion"], 3);
    wire.reply(
        &hello,
        json!({"instanceId":"alpha.21","serverVersion":"0.1.0-alpha.21","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}),
    );
}

/// The features alpha.21 advertises that this client needs.
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
        v3_hello(&mut wire, &V3_FEATURES);
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
        v3_hello(&mut wire, &V3_FEATURES);
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
         (connections: 2, attached Sessions: 1, running turns: 1), and this {} client cannot \
         use that build. No Session was started. Run `dimcode daemon stop` when they are done, \
         and the next start runs {}.",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), expected);
    assert!(output.stdout.is_empty());
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
        v3_hello(&mut wire, &V3_FEATURES);
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
