#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path = PathBuf::from("/tmp").join(format!("dim-cli-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("rust")).unwrap();
        fs::set_permissions(path.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn daemon(&self, flow: impl FnOnce(&mut Wire) + Send + 'static) -> thread::JoinHandle<()> {
        let path = self.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut wire = Wire(BufReader::new(stream));
            flow(&mut wire);
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dimcode"));
        command.args(["--home", self.0.to_str().unwrap()]);
        command
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Wire(BufReader<UnixStream>);
impl Wire {
    fn read(&mut self, method: &str) -> Value {
        let mut line = String::new();
        self.0.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["method"], method);
        value
    }
    fn send(&mut self, value: Value) {
        writeln!(self.0.get_mut(), "{value}").unwrap();
        self.0.get_mut().flush().unwrap();
    }
    fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
    }
    fn notice(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }
    fn start(&mut self, resume: bool) {
        let hello = self.read("hello");
        assert_eq!(hello["params"]["protocolVersion"], 3);
        self.reply(&hello, json!({"instanceId":"server","serverVersion":"0.1.0","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        if !resume {
            let create = self.read("session.create");
            self.reply(&create, json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
        }
        let attach = self.read("session.attach");
        self.reply(&attach, json!({}));
        let history = self.read("session.history");
        self.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let idle = self.read("session.idle");
        self.reply(&idle, json!({"sequence":8}));
    }
    fn event(&mut self, sequence: u64, name: &str, data: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":format!("event-{sequence}"),"sequence":sequence,"create_at":1,"type":"step","name":name,"data":data}}));
    }
    fn end(&mut self, outcome: Value) {
        self.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"end","sequence":25,"create_at":1,"type":"turn","name":"end","data":outcome}}));
    }
    fn detach(&mut self) {
        let detach = self.read("session.detach");
        self.reply(&detach, json!({}));
    }
}

fn wait(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(12);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("CLI did not settle");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn help_and_version_do_not_create_product_state() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for flag in ["--help", "--version"] {
        let output = home.command().arg(flag).output().unwrap();
        assert!(output.status.success());
    }
    assert!(!home.0.join("rust").exists());
}

#[test]
fn model_catalog_lists_safe_ids_and_availability_and_refreshes_explicitly() {
    for refresh in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let request = wire.read(if refresh { "provider.refresh" } else { "provider.catalog" });
            assert_eq!(request["params"], json!({"providerId":"fixture"}));
            let catalog = json!({"account":{"accessToken":"must-not-print"},"version":1,"disabledModelIds":["disabled"],"models":[
                {"id":"model\u{1b}[2J-id","name":"Model\nName\tvalue","unavailableReason":null},
                {"id":"disabled","name":"Disabled","unavailableReason":null},
                {"id":"missing","name":"Missing","unavailableReason":"private diagnostic must-not-print"}
            ]});
            wire.reply(&request, if refresh { catalog } else { json!({"catalog":catalog}) });
        });
        let mut command = home.command();
        command.args(["models", "fixture"]);
        if refresh {
            command.arg("--refresh");
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "model-id\tModel Name value\tavailable\ndisabled\tDisabled\tdisabled\nmissing\tMissing\tunavailable\n"
        );
        assert!(output.stderr.is_empty());
    }
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--refresh"));
}

#[test]
fn daemon_stop_without_an_endpoint_never_creates_state_or_spawns_a_server() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let output = home
        .command()
        .args([
            "--server",
            "/missing-app-server",
            "--model-config",
            "/missing-model-config",
            "daemon",
            "stop",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.0.join("rust").exists());
    fs::create_dir(home.0.join("rust")).unwrap();
    fs::set_permissions(home.0.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        home.command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn daemon_stop_waits_for_completion_and_closure_without_touching_a_successor() {
    let home = Home::new();
    fs::write(home.0.join("account-marker"), "retained").unwrap();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"configured","modelConfigSha256":"0".repeat(64)},"features":["session.create","session.history","daemon.shutdown.completed"]}));
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({}));
        thread::sleep(Duration::from_millis(100));
        fs::remove_file(&socket).unwrap();
        let _successor = UnixListener::bind(&socket).unwrap();
        wire.reply(&shutdown, json!({}));
        thread::sleep(Duration::from_millis(100));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
    });
    let started = Instant::now();
    let output = home.command().args(["daemon", "stop"]).output().unwrap();
    let elapsed = started.elapsed();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed >= Duration::from_millis(200));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert_eq!(
        fs::read_to_string(home.0.join("account-marker")).unwrap(),
        "retained"
    );
}

#[test]
fn daemon_stop_never_treats_old_protocol_eof_or_cleanup_failure_as_completion() {
    for case in ["old", "eof", "failed"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            let mut features = vec!["session.create", "session.history"];
            if case != "old" { features.push("daemon.shutdown.completed"); }
            wire.reply(&hello, json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":features}));
            if case == "old" {
                let mut line = String::new();
                assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "must not stop an incompatible daemon");
            } else {
                let request = wire.read("daemon.shutdown");
                if case == "failed" { wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"Daemon cleanup failed"}})); }
            }
        });
        let output = home.command().args(["daemon", "stop"]).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Daemon stopped."));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn headless_streams_unicode_once_and_waits_for_the_delivery_barrier() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert!(!submit["params"]["submissionId"].as_str().unwrap().is_empty());
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":0,"text":"你\u{1b}[2J好","thinking":"","thinkingOffset":0}));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"你\u{1b}[2J好"}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":6,"text":"世界","thinking":"","thinkingOffset":0}));
        wire.event(20,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"世界"}));
        wire.event(22,"model-output-end",json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--prompt", "hello"])
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "你好世界\n");
}

#[test]
fn noninteractive_approval_denies_and_drains_outstanding_responses_before_detach() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"do something","token":"never print me"}},"reason":"shell command"}));
        let approval = wire.read("approval.respond"); assert_eq!(approval["params"]["decision"],"deny");
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"denied"}));
        wire.reply(&stop,json!({"sequence":30}));
        wire.reply(&idle,json!({"sequence":30}));
        wire.reply(&approval,json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--prompt", "exec"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never print me"));
    assert!(output.stdout.is_empty());
}

#[test]
fn resume_advances_filtered_history_positions_and_does_not_reprint_old_output() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let first = wire.read("session.history"); assert_eq!(first["params"]["after"],0);
        wire.reply(&first,json!({"events":[{"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old-request","delta":"old output"}}],"through":10,"nextAfter":7,"done":false}));
        let next = wire.read("session.history");
        assert_eq!(next["params"]["after"],7); assert_eq!(next["params"]["through"],10);
        wire.reply(&next,json!({"events":[{"sequence":8,"create_at":2,"type":"step","name":"model-output-end","data":{"turn_id":"old-turn","request_id":"old-request","status":"completed","finish_reason":"end_turn"}}],"through":10,"nextAfter":10,"done":true}));
        // attach progress snapshots may predate the history response.
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"old-request","textOffset":0,"text":"old output","thinking":"","thinkingOffset":0}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":10}));
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"new-turn","request_id":"new-request","delta":"new output"}));
        wire.end(json!({"turn_id":"new-turn","status":"completed","reason":"done"}));
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--resume", "session", "--prompt", "continue"])
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
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "new output\n");
}

#[test]
fn incompatible_daemon_is_rejected_without_removing_its_endpoint() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"other","serverVersion":"1","protocolVersion":2,"features":[]}),
        );
    });
    let output = home.command().args(["--prompt", "hello"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn explicit_model_config_rejects_mock_and_mismatched_configured_profiles() {
    for profile in [
        json!({"kind":"mock"}),
        json!({"kind":"configured","modelConfigSha256":"0".repeat(64)}),
    ] {
        let home = Home::new();
        let config = home.0.join("model.json");
        fs::write(&config, "{}").unwrap();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":profile,"features":["session.create","session.history"]}));
        });
        let output = home
            .command()
            .args([
                "--model-config",
                config.to_str().unwrap(),
                "--prompt",
                "hello",
            ])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(home.0.join("rust/daemon.sock").exists());
        assert!(!home.0.join("rust/daemon.log").exists());
    }
}

#[test]
fn piped_prompt_runs_once_and_model_failure_has_a_distinct_exit_code() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "from stdin\n");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.end(json!({"turn_id":"turn","status":"failed","error":"credential should not print"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let mut child = home
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from stdin\n")
        .unwrap();
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("credential should not print"));
}

struct Terminal {
    master: fs::File,
    transcript: String,
    consumed: usize,
}
impl Terminal {
    fn launch(mut command: Command) -> (Self, Child) {
        let mut master = 0;
        let mut slave = 0;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        let child = command.spawn().unwrap();
        (
            Self {
                master,
                transcript: String::new(),
                consumed: 0,
            },
            child,
        )
    }
    fn until(&mut self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(index) = self.transcript[self.consumed..].find(expected) {
                self.consumed += index + expected.len();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Terminal did not show {expected:?}: {}",
                self.transcript
            );
            let mut poll = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 100) } == 0 {
                continue;
            }
            let mut bytes = [0u8; 4096];
            let size = self.master.read(&mut bytes).unwrap();
            assert!(size > 0, "Terminal closed before {expected}");
            self.transcript
                .push_str(&String::from_utf8_lossy(&bytes[..size]));
        }
    }
    fn write(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }
}

#[test]
fn interactive_default_deny_discards_typeahead_and_ctrl_c_settles_before_next_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"first-request","textOffset":0,"text":"working","thinking":"","thinkingOffset":0}));
        thread::sleep(Duration::from_millis(150));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"description":"Inspect workspace","command":"printf workspace-inspected","cwd":"/tmp/approval-fixture","secret":"never print me"}},"reason":"shell command"}));
        let answer = wire.read("approval.respond");
        assert_eq!(answer["params"]["decision"],"deny");
        wire.reply(&answer,json!({}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"denied"}));
        wire.reply(&idle,json!({"sequence":30}));
        let submit = wire.read("session.submit"); assert_eq!(submit["params"]["text"],"second");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"second-request","textOffset":0,"text":"partial","thinking":"","thinkingOffset":0}));
        let ended = json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":35,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"second-turn","status":"cancelled","reason":"interrupted"}}}}).to_string();
        wire.0.get_mut().write_all(&ended.as_bytes()[..10]).unwrap();
        wire.0.get_mut().flush().unwrap();
        let stop = wire.read("session.stop");
        // A cancelled next-frame wait must retain an already consumed prefix.
        writeln!(wire.0.get_mut(),"{}",&ended[10..]).unwrap();
        wire.reply(&stop,json!({"sequence":40}));
        thread::sleep(Duration::from_millis(100));
        wire.reply(&idle,json!({"sequence":40}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch({
        let mut command = home.command();
        command.arg("--no-tui");
        command
    });
    terminal.until("\n> ");
    terminal.write(b"first\n");
    terminal.until("working");
    terminal.write(b"yes\n");
    terminal.until("Allow once? [y/N] ");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"second\n");
    terminal.until("partial");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let output = wait(child);
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(
        terminal
            .transcript
            .contains("command: printf workspace-inspected"),
        "{}",
        terminal.transcript
    );
    assert!(
        terminal.transcript.contains("cwd: /tmp/approval-fixture"),
        "{}",
        terminal.transcript
    );
    assert!(!terminal.transcript.contains("never print me"));
}

#[test]
fn explicit_model_selection_uses_protocol_field_names_from_tui_and_line_client() {
    for tui in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
            let create = wire.read("session.create");
            assert_eq!(create["params"]["model"], json!({"provider":"fixture","modelId":"fixture-model"}));
            wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32602,"message":"fixture stops here"}}));
        });
        let mut command = home.command();
        command.args(["--provider", "fixture", "--model", "fixture-model"]);
        if tui {
            // A pty without --no-tui selects the TUI session host.
            let (_terminal, mut child) = Terminal::launch(command);
            let joined = daemon.join();
            child.kill().unwrap();
            child.wait().unwrap();
            joined.unwrap();
        } else {
            command
                .args(["--prompt", "hello"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let output = wait(command.spawn().unwrap());
            daemon.join().unwrap();
            assert!(!output.status.success());
        }
    }
}

#[test]
fn browser_login_hides_callback_and_uses_only_generic_account_rpc() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([{"providerId":"fixture","displayName":"Fixture","loginMethods":["browser"]}]));
        let begin = wire.read("account.login.browser.begin");
        assert_eq!(begin["params"]["directory"],"https://fixture.invalid");
        wire.reply(&begin,json!({"attemptId":"attempt","authorizationUrl":"urn:fixture:authorization","expiresAt":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() + 60_000}));
        let complete = wire.read("account.login.browser.complete");
        assert_eq!(complete["params"]["callbackUrl"],"http://localhost:1455/auth/callback?code=secret-code&state=state");
        wire.reply(&complete,json!({"credentialEpoch":"epoch","generation":1,"account":{"issuer":"fixture","subject":"test","grantedScopes":[]}}));
    });
    let mut command = home.command();
    command.args(["login", "fixture", "--directory", "https://fixture.invalid"]);
    let (mut terminal, child) = Terminal::launch(command);
    terminal.until("Callback URL: ");
    terminal.write(b"http://localhost:1455/auth/callback?code=secret-code&state=state\n");
    terminal.until("Signed in.");
    let output = wait(child);
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(
        !terminal.transcript.contains("secret-code"),
        "{}",
        terminal.transcript
    );
    let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.master.as_raw_fd(), attributes.as_mut_ptr()) },
        0
    );
    assert_ne!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
}

#[test]
fn device_login_polls_and_cancels_the_attempt_on_ctrl_c() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([{"providerId":"fixture","displayName":"Fixture","loginMethods":["device"]}]));
        let begin = wire.read("account.login.begin");
        wire.reply(&begin,json!({"attemptId":"device","verificationUri":"https://fixture.invalid/device","userCode":"TEST-CODE","expiresAt":999999}));
        let status = wire.read("account.login.status"); wire.reply(&status,json!({"status":"pending"}));
        let cancel = wire.read("account.login.cancel"); assert_eq!(cancel["params"]["attemptId"],"device"); wire.reply(&cancel,json!({}));
    });
    let mut command = home.command();
    command.args([
        "login",
        "fixture",
        "--directory",
        "https://fixture.invalid",
        "--device",
    ]);
    let (mut terminal, child) = Terminal::launch(command);
    terminal.until("TEST-CODE");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
}

#[test]
#[ignore = "requires DIM_TEST_NPM_WRAPPER pointing to npm/bin/dimcode.mjs"]
fn unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow() {
    let source = std::env::var("DIM_TEST_NPM_WRAPPER").expect("DIM_TEST_NPM_WRAPPER");
    let home = Home::new();
    let node_platform = Command::new("node")
        .args(["-p", "process.platform+'-'+process.arch"])
        .output()
        .unwrap();
    assert!(node_platform.status.success());
    let platform = String::from_utf8(node_platform.stdout).unwrap();
    let package = home
        .0
        .join(format!("wrapper/node_modules/dimcode-{}", platform.trim()));
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"dimcode-{}\"}}", platform.trim()),
    )
    .unwrap();
    fs::copy(env!("CARGO_BIN_EXE_dimcode"), package.join("bin/dimcode")).unwrap();
    fs::copy(source, home.0.join("wrapper/dimcode.mjs")).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"streaming","thinking":"","thinkingOffset":0}));
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        thread::sleep(Duration::from_millis(100));
        wire.reply(&stop,json!({"sequence":30})); wire.reply(&idle,json!({"sequence":30}));
        wire.detach();
    });
    let mut command = Command::new("node");
    command
        .arg(home.0.join("wrapper/dimcode.mjs"))
        .arg("--home")
        .arg(&home.0);
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let (mut terminal, child) = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"hello\n");
    terminal.until("streaming");
    assert_eq!(unsafe { libc::kill(-(child.id() as i32), libc::SIGINT) }, 0);
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let output = wait(child);
    daemon.join().unwrap();
    assert!(output.status.success(), "{}", terminal.transcript);
}

struct OwnedDaemon(PathBuf);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let Ok(stream) = UnixStream::connect(&self.0) else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut stream = BufReader::new(stream);
        for (id, method, params) in [
            (1, "hello", json!({"protocolVersion":3,"clientKind":"cli"})),
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

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn project_mcp_approval_commands_persist_and_reject_stale_configuration() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let write_config = |argument: &str| {
        fs::write(project.join(".mcp.json"), json!({"mcpServers":{"docs":{"command":"missing-mcp-program","args":[argument],"env":{"SECRET":"must-not-print"}}}}).to_string()).unwrap();
    };
    write_config("original");
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .arg("--model-config")
            .arg(&config)
            .arg("--cwd")
            .arg(&project)
            .args(args)
            .output()
            .unwrap()
    };
    let list = || {
        let output = run(&["mcp", "list"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(!output.contains("must-not-print"));
        output
    };
    let original = list();
    assert!(original.contains("\tproject\tstdio\tenabled\trequired\t"));
    let token = original.trim_end().split('\t').next_back().unwrap();
    assert_eq!(token.len(), 64);
    assert!(run(&["mcp", "approve", "docs", token]).status.success());
    assert!(list().contains("\tapproved\t"));
    assert!(run(&["daemon", "stop"]).status.success());
    assert!(list().contains("\tapproved\t"), "restart lost the grant");
    write_config("changed");
    assert!(list().contains("\trequired\t"));
    let rejected = run(&["mcp", "approve", "docs", token]);
    assert!(!rejected.status.success());
    assert!(list().contains("\trequired\t"));
    let current = list();
    let token = current.trim_end().split('\t').next_back().unwrap();
    assert!(run(&["mcp", "approve", "docs", token]).status.success());
    assert!(run(&["mcp", "revoke", "docs"]).status.success());
    assert!(list().contains("\trequired\t"));
    let id = "untrusted\u{9b}2J\u{202e}\nname";
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{(id):{"command":"missing-mcp-program"}}}).to_string(),
    )
    .unwrap();
    let escaped = list();
    assert!(!escaped.contains(['\u{9b}', '\u{202e}']));
    assert_eq!(escaped.lines().count(), 1);
    assert!(escaped.contains("untrusted\\u{9b}2J\\u{202e}\\nname"));
    assert!(run(&["daemon", "stop"]).status.success());
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn real_daemon_profile_conflicts_preserve_the_running_daemon_and_endpoint() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    for mock in [true, false] {
        let home = Home::new();
        let original = home.0.join("original.json");
        let requested = home.0.join("requested.json");
        let body = json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","models":[{"id":"fixture-model","config":{"maxTokens":128}}]}]}).to_string();
        fs::write(&original, &body).unwrap();
        fs::write(&requested, format!("{body}\n")).unwrap();
        let socket = home.0.join("rust/daemon.sock");
        let mut command = Command::new(&binary);
        command
            .arg("--unix")
            .arg(&socket)
            .arg("--home")
            .arg(home.0.join("rust"))
            .arg("--no-ws")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if mock {
            command.arg("--mock-model");
        } else {
            command.arg("--model-config").arg(&original);
        }
        let mut daemon = command.spawn().unwrap();
        let owned = OwnedDaemon(socket.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(daemon.try_wait().unwrap().is_none(), "Daemon did not start");
            assert!(Instant::now() < deadline, "Daemon did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
        let output = home
            .command()
            .args([
                "--server",
                &binary,
                "--model-config",
                requested.to_str().unwrap(),
                "--prompt",
                "must not run",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(socket.exists());
        assert!(daemon.try_wait().unwrap().is_none());
        fs::write(&requested, "invalid changed configuration").unwrap();
        let stopped = home
            .command()
            .args([
                "--server",
                &binary,
                "--model-config",
                requested.to_str().unwrap(),
                "daemon",
                "stop",
            ])
            .output()
            .unwrap();
        assert!(
            stopped.status.success(),
            "{}",
            String::from_utf8_lossy(&stopped.stderr)
        );
        assert!(!socket.exists());
        assert!(home.0.join("rust").exists());
        drop(owned);
        assert!(wait(daemon).status.success());
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn configured_api_key_model_uses_production_tools_and_restores_the_same_session() {
    let binary = std::env::var("DIM_TEST_APP_SERVER")
        .expect("Set DIM_TEST_APP_SERVER to the dim-app-server binary");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    fs::write(home.0.join("marker.txt"), "production-read-marker\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let model = thread::spawn(move || {
        for phase in 0..3 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Model request {phase} was not made"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = 0;
            let mut authorization = false;
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                if lower.trim() == "authorization: bearer fixture-secret" {
                    authorization = true;
                }
            }
            assert!(authorization, "API key environment reference was not used");
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let mut names: Vec<_> = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect();
            names.sort_unstable();
            assert_eq!(
                names,
                [
                    "agent", "branch", "edit", "exec", "glob", "grep", "read", "skill", "wait",
                    "write"
                ]
            );
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"fixture-response","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = if phase == 0 {
                frame(
                    json!({"tool_calls":[{"index":0,"id":"read-call","type":"function","function":{"name":"read","arguments":"{\"path\":\"marker.txt\"}"}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else {
                if phase == 1 {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("production-read-marker"))
                        ),
                        "The real read result did not reach the model"
                    );
                }
                frame(
                    json!({"content":if phase == 1 {"read completed"} else {"session continued"}}),
                    json!("stop"),
                )
            } + "data: [DONE]\n\n";
            write!(stream.get_mut(),"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config,serde_json::to_vec(&json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","apiKeyEnv":"DIM_CLI_TEST_API_KEY","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","config":{"maxTokens":128}}]}]})).unwrap()).unwrap();
    let mut first = home.command();
    first
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "--prompt",
            "read marker.txt",
        ])
        .env("DIM_CLI_TEST_API_KEY", "fixture-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(first.spawn().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "read completed\n"
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("[done] read"));
    assert!(!diagnostic.contains("fixture-secret"));
    let session = diagnostic
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap();
    let mut resumed = home.command();
    resumed
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "--resume",
            session,
            "--prompt",
            "continue",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = wait(resumed.spawn().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "session continued\n"
    );
    model.join().unwrap();
}
