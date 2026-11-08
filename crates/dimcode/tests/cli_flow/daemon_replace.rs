//! Start decisions over the scripted and the real daemon: one client at a
//! time decides, a daemon of another build is replaced only while it is idle,
//! and a busy one is kept with a notice (#1213).
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn old_hello(server: &str, features: Value) -> Value {
    json!({
        "instanceId": "old",
        "serverVersion": server,
        "protocolVersion": 3,
        "daemonProfile": {"kind": "product"},
        "features": features,
    })
}

/// What a daemon that advertises the idle shutdown reports.
fn new_style_features() -> Value {
    json!([
        "session.create",
        "session.history",
        "session.summaries",
        "daemon.shutdown.completed",
        "daemon.shutdown.ifIdle"
    ])
}

/// Serves one connection that the CLI makes after a replacement: hello at
/// this build, then the command's next call.
fn serve_successor(successor: &UnixListener) {
    let deadline = Instant::now() + HANG_GUARD;
    loop {
        let stream = match successor.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "the CLI never reconnected");
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => panic!("{error}"),
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
        let mut wire = Wire::new(stream);
        let Some(line) = wire.trailing_request() else {
            continue;
        };
        let hello: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(hello["method"], "hello");
        wire.reply(
            &hello,
            json!({"instanceId":"new","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}),
        );
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
        return;
    }
}

/// An idle daemon of another build, judged by `session.summaries`, is stopped
/// once and replaced by this build's daemon.
#[test]
fn an_idle_daemon_of_another_build_is_replaced() {
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            old_hello(
                "0.0.1-old",
                json!([
                    "session.create",
                    "session.history",
                    "session.summaries",
                    "daemon.shutdown.completed"
                ]),
            ),
        );
        let summaries = wire.read("session.summaries");
        wire.reply(&summaries, json!([]));
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(
            shutdown["params"],
            json!({}),
            "a daemon without daemon.shutdown.ifIdle gets the plain shutdown"
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
        "an idle replacement says nothing: {stderr}"
    );
}

/// A daemon of another build that serves an attached Session, a running turn,
/// or an unreadable summary set is kept: no shutdown, and the command runs
/// against it with one notice on stderr.
#[test]
fn a_daemon_of_another_build_in_use_is_kept() {
    for case in ["attached", "running", "unreadable"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(
                &hello,
                old_hello(
                    "0.0.1-old",
                    json!([
                        "session.create",
                        "session.history",
                        "session.summaries",
                        "daemon.shutdown.completed"
                    ]),
                ),
            );
            let summaries = wire.read("session.summaries");
            match case {
                "attached" => wire.reply(
                    &summaries,
                    json!([{"sessionId":"busy","attachedClients":1,"running":false}]),
                ),
                "running" => wire.reply(
                    &summaries,
                    json!([{"sessionId":"busy","attachedClients":0,"running":true}]),
                ),
                _ => wire.send(json!({"jsonrpc":"2.0","id":summaries["id"],"error":{
                    "code":-32000,"message":"Summaries unavailable",
                    "data":{"appError":"provider_failed"}}})),
            }
            let catalog = wire.read("provider.catalog");
            wire.reply(&catalog, json!({"catalog":null}));
            assert_eq!(
                wire.trailing_request(),
                None,
                "a busy daemon must not be stopped ({case})"
            );
        });
        let output = home.command().args(["models", "fixture"]).output().unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("The App Server is still 0.0.1-old because other clients are using it"),
            "{case}: {stderr}"
        );
        assert!(stderr.contains("dimcode daemon stop"), "{case}: {stderr}");
    }
}

/// A daemon that advertises `daemon.shutdown.ifIdle` decides for itself: a
/// `daemon_busy` refusal keeps it (the notice carries its counts), and an
/// accepted idle shutdown lets the client replace it.
#[test]
fn a_new_style_daemon_decides_whether_it_is_idle() {
    {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello, old_hello("0.0.1-old", new_style_features()));
            let shutdown = wire.read("daemon.shutdown");
            assert_eq!(shutdown["params"], json!({"ifIdle": true}));
            wire.send(json!({"jsonrpc":"2.0","id":shutdown["id"],"error":{
                "code":-32000,"message":"The App Server is in use",
                "data":{"appError":"daemon_busy","connections":2,"attachedSessions":1,"running":1}}}));
            let catalog = wire.read("provider.catalog");
            wire.reply(&catalog, json!({"catalog":null}));
            assert_eq!(wire.trailing_request(), None, "the refusal keeps it");
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
            stderr.contains("connections: 2, attached Sessions: 1, running turns: 1"),
            "{stderr}"
        );
        assert!(
            stderr.contains("The App Server is still 0.0.1-old"),
            "{stderr}"
        );
    }
    {
        let home = Home::new();
        let socket = home.0.join("rust/daemon.sock");
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello, old_hello("0.0.1-old", new_style_features()));
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
            "an accepted idle shutdown says nothing: {stderr}"
        );
    }
}

/// Two clients starting together against an idle daemon of another build
/// replace it once: one shutdown, one spawn, both clients served.
#[test]
fn two_starting_clients_replace_an_idle_daemon_once() {
    for iteration in 0..10 {
        let home = Home::new();
        let socket = home.0.join("rust/daemon.sock");
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let counted = shutdowns.clone();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(
                &hello,
                old_hello(
                    "0.0.1-old",
                    json!([
                        "session.create",
                        "session.history",
                        "session.summaries",
                        "daemon.shutdown.completed"
                    ]),
                ),
            );
            let summaries = wire.read("session.summaries");
            wire.reply(&summaries, json!([]));
            let shutdown = wire.read("daemon.shutdown");
            counted.fetch_add(1, Ordering::SeqCst);
            fs::remove_file(&socket).unwrap();
            wire.reply(&shutdown, json!({}));
            wire.0
                .get_mut()
                .shutdown(std::net::Shutdown::Write)
                .unwrap();
        });
        let spawn = |home: &Home| {
            home.command()
                .args(["models", "fixture"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let first = spawn(&home);
        let second = spawn(&home);
        let (first, second) = (wait(first), wait(second));
        daemon.join().unwrap();
        assert_eq!(
            shutdowns.load(Ordering::SeqCst),
            1,
            "iteration {iteration}: one client must decide the replacement"
        );
        for (label, output) in [("first", &first), ("second", &second)] {
            assert!(
                output.status.success(),
                "iteration {iteration}, {label}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let log = fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default();
        assert!(
            !log.contains("another process owns the SQLite database"),
            "iteration {iteration}: two daemons raced for the store: {log}"
        );
    }
}

/// Two clients starting together with no daemon start one and both connect.
#[test]
fn two_starting_clients_with_no_daemon_start_one() {
    let home = Home::new();
    let spawn = |home: &Home| {
        home.command()
            .args(["models", "fixture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn(&home);
    let second = spawn(&home);
    let (first, second) = (wait(first), wait(second));
    for (label, output) in [("first", &first), ("second", &second)] {
        assert!(
            output.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let log = fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default();
    assert!(
        !log.contains("another process owns the SQLite database"),
        "two daemons raced for the store: {log}"
    );
}

/// The notice a kept daemon produces never reaches `exec --json`'s stdout:
/// every line there stays JSON.
#[test]
fn the_daemon_notice_never_reaches_json_stdout() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            old_hello(
                "0.0.1-old",
                json!(["session.create","session.history","session.summaries","daemon.shutdown.completed"]),
            ),
        );
        let summaries = wire.read("session.summaries");
        wire.reply(
            &summaries,
            json!([{"sessionId":"busy","attachedClients":1,"running":false}]),
        );
        wire.created();
        wire.attached();
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(2, "model-output-end", json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":3}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--json", "--prompt", "go"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("The App Server is still 0.0.1-old"),
        "{stderr}"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("The App Server is still"),
        "the notice reached stdout: {stdout}"
    );
    for line in stdout.lines() {
        serde_json::from_str::<Value>(line)
            .unwrap_or_else(|error| panic!("stdout line is not JSON: {line:?}: {error}"));
    }
}
