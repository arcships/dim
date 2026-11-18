//! Start decisions over the scripted and the real daemon: one client at a
//! time decides, a daemon of another build is replaced only while it is idle,
//! and a busy one is kept with a notice (#1213).
use super::*;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many trailing lines of `<home>/rust/daemon.log` a failed client's
/// message carries.
const DAEMON_LOG_TAIL_LINES: usize = 40;

/// The facts a non-zero exit needs: `success()` is false for a signal as
/// well as for an exit code, and a client that ends without a word on stderr
/// leaves its evidence in stdout and the daemon log (#1577).
pub(super) fn describe_client(home: &Home, output: &Output) -> String {
    let status = match output.status.signal() {
        Some(signal) => format!("{:?} (signal: {signal})", output.status),
        None => format!("{:?}", output.status),
    };
    let log = match fs::read_to_string(home.0.join("rust/daemon.log")) {
        Ok(log) if log.trim().is_empty() => "daemon.log is empty".to_string(),
        Ok(log) => {
            let lines: Vec<&str> = log.lines().collect();
            let start = lines.len().saturating_sub(DAEMON_LOG_TAIL_LINES);
            format!(
                "daemon.log, last {DAEMON_LOG_TAIL_LINES} lines:\n{}",
                lines[start..].join("\n")
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "no daemon.log".to_string(),
        Err(error) => format!("daemon.log unreadable: {error}"),
    };
    format!(
        "status: {status}\nstdout:\n{}\nstderr:\n{}\n{log}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn old_hello(server: &str, features: Value) -> Value {
    json!({
        "instanceId": "old",
        "serverVersion": server,
        "protocolVersion": 4,
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
/// this build, then the command's next call. Returns the hello it read, for a
/// flow that checks what the successor was greeted with.
pub(super) fn serve_successor(successor: &UnixListener) -> Value {
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
            json!({"instanceId":"new","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}),
        );
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
        return hello;
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
        describe_client(&home, &output)
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
            "{case}:\n{}",
            describe_client(&home, &output)
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
            describe_client(&home, &output)
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
            describe_client(&home, &output)
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
        let first_pid = first.id();
        let second = spawn(&home);
        let second_pid = second.id();
        let (first, second) = (wait(first), wait(second));
        for (label, pid, output) in [
            ("first", first_pid, &first),
            ("second", second_pid, &second),
        ] {
            assert!(
                output.status.success(),
                "iteration {iteration}, {label}, pid={pid}:\n{}",
                describe_client(&home, output)
            );
        }
        daemon.join().unwrap();
        assert_eq!(
            shutdowns.load(Ordering::SeqCst),
            1,
            "iteration {iteration}: one client must decide the replacement"
        );
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
            "{label}:\n{}",
            describe_client(&home, output)
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
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            old_hello(
                "0.0.1-old",
                json!(["session.create","session.history","session.history.latestTurn","session.methods","session.summaries","daemon.shutdown.completed"]),
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
        describe_client(&home, &output)
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

/// A daemon of this same version and protocol is told apart by the artifact
/// stamp it reports, not by its version: another stamp, or none at all, is kept
/// with one notice and no shutdown, whether or not it is idle, and a matching
/// stamp says nothing. `DIM_TEST_BUILD_ID` is a debug-build stand-in for the
/// stamp a distributable build embeds, so this is a debug-build check.
#[cfg(debug_assertions)]
mod build_identity {
    use super::*;

    /// The hello a daemon of this version reports, with `build_id` when it has
    /// one.
    fn same_version_hello(build_id: Option<&str>) -> Value {
        let mut hello = json!({
            "instanceId": "same-version",
            "serverVersion": env!("CARGO_PKG_VERSION"),
            "protocolVersion": 4,
            "daemonProfile": {"kind": "product"},
            "features": [
                "session.create",
                "session.history",
                "session.summaries",
                "daemon.shutdown.completed",
                "daemon.shutdown.ifIdle"
            ],
        });
        if let Some(build_id) = build_id {
            hello["buildId"] = json!(build_id);
        }
        hello
    }

    /// A daemon of this version that serves one `models fixture` call and must
    /// never see a shutdown: not even `daemon.shutdown {ifIdle}`, which would
    /// replace it.
    fn daemon_of_build(home: &Home, build_id: Option<&'static str>) -> thread::JoinHandle<()> {
        home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello, same_version_hello(build_id));
            let catalog = wire.read("provider.catalog");
            wire.reply(&catalog, json!({"catalog": null}));
            assert_eq!(
                wire.trailing_request(),
                None,
                "a daemon of this version is kept, never stopped"
            );
        })
    }

    /// Runs `models fixture` as a client stamped `build_id`.
    fn run_client(home: &Home, build_id: &str) -> Output {
        home.command()
            .env("DIM_TEST_BUILD_ID", build_id)
            .args(["models", "fixture"])
            .output()
            .unwrap()
    }

    #[test]
    fn a_daemon_of_another_build_of_this_version_is_kept_with_a_notice() {
        let home = Home::new();
        let daemon = daemon_of_build(&home, Some("build-2026-11-16"));
        let output = run_client(&home, "build-2026-11-18");
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            describe_client(&home, &output)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("The App Server is still using a different build of"),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "`{} --home {} daemon stop`",
                env!("CARGO_BIN_EXE_dimcode"),
                home.0.display()
            )),
            "the notice names the command for this executable and home: {stderr}"
        );
    }

    #[test]
    fn a_daemon_that_reports_no_stamp_is_kept_as_unverified() {
        let home = Home::new();
        let daemon = daemon_of_build(&home, None);
        let output = run_client(&home, "build-2026-11-18");
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            describe_client(&home, &output)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("The App Server's build could not be verified as this client's."),
            "{stderr}"
        );
        assert!(stderr.contains("daemon stop"), "{stderr}");
    }

    #[test]
    fn the_build_notice_stop_command_targets_the_home_selected_by_environment() {
        let launch_dir = Home::new();
        let other_home = Home::new();
        for (relative_home, daemon_build) in [
            ("home;&|<>()*?[x]", Some("other-build")),
            ("home with a 'quote'", None),
        ] {
            let home = Home::at(launch_dir.0.join(relative_home));
            let native_home = home.0.join("rust");
            fs::create_dir_all(&native_home).unwrap();
            fs::set_permissions(&native_home, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(home.0.join("config.json"), r#"{"locale":"en"}"#).unwrap();

            let daemon = daemon_of_build(&home, daemon_build);
            let output = Command::new(env!("CARGO_BIN_EXE_dimcode"))
                .current_dir(&launch_dir.0)
                .env("DIMCODE_HOME", relative_home)
                .env("DIMCODE_UPDATE_CHECK", "0")
                .env("DIM_TEST_BUILD_ID", "client-build")
                .args(["models", "fixture"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                describe_client(&home, &output)
            );
            daemon.join().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            let command = stderr
                .split('`')
                .find(|part| part.ends_with(" daemon stop"))
                .unwrap_or_else(|| panic!("the notice must include a stop command: {stderr}"));

            fs::remove_file(native_home.join("daemon.sock")).unwrap();
            let daemon = home.daemon(move |wire| {
                let hello = wire.read("hello");
                wire.reply(&hello, same_version_hello(daemon_build));
                let shutdown = wire.read("daemon.shutdown");
                assert_eq!(shutdown["params"], json!({}));
                wire.reply(&shutdown, json!({}));
            });
            let output = Command::new("/bin/sh")
                .current_dir(&other_home.0)
                .env("DIMCODE_HOME", &other_home.0)
                .env("DIMCODE_UPDATE_CHECK", "0")
                .args(["-c", command])
                .output()
                .unwrap();
            assert!(
                output.status.success() && output.stdout == b"Daemon stopped.\n",
                "the pasted command {command:?} must stop the selected daemon: {}",
                describe_client(&home, &output)
            );
            daemon.join().unwrap();
        }
    }

    #[test]
    fn a_daemon_of_this_build_is_reused_silently() {
        let home = Home::new();
        let daemon = daemon_of_build(&home, Some("build-2026-11-18"));
        let output = run_client(&home, "build-2026-11-18");
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            describe_client(&home, &output)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("The App Server"),
            "a matching stamp says nothing: {stderr}"
        );
    }

    /// An explicitly named server is not this executable's daemon: its build is
    /// expected to differ, so the embedded-build notice does not apply.
    #[test]
    fn an_explicit_server_gets_no_embedded_build_notice() {
        let home = Home::new();
        let server = home.0.join("other-build-server");
        fs::write(
            &server,
            format!(
                "#!/bin/sh\nexec env DIM_TEST_BUILD_ID=server-build '{}' app-server \"$@\"\n",
                env!("CARGO_BIN_EXE_dimcode")
            ),
        )
        .unwrap();
        fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
        let output = home
            .command()
            .env("DIM_TEST_BUILD_ID", "client-build")
            .args(["--server", server.to_str().unwrap(), "models", "fixture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            describe_client(&home, &output)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("The App Server"),
            "an explicit server gets no embedded-build notice: {stderr}"
        );
    }
}
