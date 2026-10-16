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
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":0,"text":"你\u{1b}[2J好"}));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"你\u{1b}[2J好"}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":6,"text":"世界"}));
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
            .args(["exec", "--json", "--prompt", "exec"])
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
    let notices: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0]["method"], "approval.request");
    assert_eq!(notices[1]["method"], "session.event");
}

#[test]
fn resume_advances_filtered_history_positions_and_does_not_reprint_old_output() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","provider.list"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let first = wire.read("session.history"); assert_eq!(first["params"]["after"],0);
        wire.reply(&first,json!({"events":[{"sequence":4,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old-request","delta":"old output"}}],"through":10,"nextAfter":7,"done":false}));
        let next = wire.read("session.history");
        assert_eq!(next["params"]["after"],7); assert_eq!(next["params"]["through"],10);
        wire.reply(&next,json!({"events":[{"sequence":8,"type":"step","name":"model-output-end","data":{"turn_id":"old-turn","request_id":"old-request","status":"completed","finish_reason":"end_turn"}}],"through":10,"nextAfter":10,"done":true}));
        // attach progress snapshots may predate the history response.
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"old-request","textOffset":0,"text":"old output"}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":10}));
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"new-turn","request_id":"new-request","delta":"new output"}));
        wire.end(json!({"turn_id":"new-turn","status":"completed","reason":"done"}));
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "--prompt", "continue"])
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
fn exec_json_preserves_public_notifications_and_only_observes_the_selected_session() {
    let home = Home::new();
    let notices = vec![
        json!({"jsonrpc":"2.0","method":"session.plugin.notice","params":{"sessionId":"session","branchId":"child","source":"fixture","future":{"control":"\u{1b}[2J\n"}}}),
        json!({"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"Hello","newField":true}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":12,"type":"step","name":"text","data":{"turn_id":"turn","request_id":"request","delta":"Hello","newField":"preserved"}}}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":25,"type":"turn","name":"end","data":{"turn_id":"turn","status":"failed","error":"fixture"}}}}),
    ];
    let emitted = notices.clone();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[{"sequence":4,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old","delta":"history-only","newField":"preserved"}}],"through":8,"nextAfter":8,"done":true}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "continue"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.closed",json!({"sessionId":"other"}));
        wire.notice("session.model.progress",json!({"sessionId":"other","text":"other-session"}));
        for notice in emitted { wire.send(notice); }
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "--json", "-p", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let received: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(received, notices);
    assert!(!stdout.contains("history-only"));
    assert!(!stdout.contains("workspace:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("session: session"));
}

#[test]
fn f6_argument_errors_do_not_connect_or_create_product_state() {
    let home = Home::new();
    for args in [
        vec!["exec", "--prompt", "one", "-p", "two"],
        vec!["exec", "resume", "one", "--resume", "two", "-p", "text"],
        vec!["--resume", "one", "exec", "resume", "two", "-p", "text"],
        vec!["--json", "-p", "text"],
        vec!["session", "export", "session"],
        vec!["session", "export", "session", "--format", "xml"],
        vec!["tui"],
        vec!["exec"],
    ] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(!home.0.join("rust/daemon.sock").exists());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn exec_json_interrupt_drains_the_stop_barrier_without_human_stdout() {
    let home = Home::new();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        entered.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["exec", "-p", "wait", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["method"], "session.event");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Waiting for resource settlement"));
}

#[test]
fn session_export_preserves_the_fixed_main_prefix_and_does_not_replace_files() {
    for mode in ["stdout-json", "markdown", "existing", "history-error"] {
        let home = Home::new();
        let cli_dir = home.0.join("client");
        let workspace = home.0.join("workspace");
        fs::create_dir(&cli_dir).unwrap();
        fs::create_dir(&workspace).unwrap();
        let path = cli_dir.join("Session notes.md");
        if mode == "existing" {
            fs::write(&path, b"keep original bytes").unwrap();
        }
        let events = vec![
            json!({"sequence":1,"type":"extension","name":"future","data":{"unknown":[1,2]},"extra":"preserved"}),
            json!({"sequence":2,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"Keep ```code``` intact"},{"type":"image","source":{"type":"url","mediaType":"image/png","url":"https://invalid.example/not-fetched"}}]}}}),
            json!({"sequence":3,"type":"step","name":"thinking","data":{"turn_id":"t","request_id":"r","delta":"A reason"}}),
            json!({"sequence":4,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"Hello "}}),
            json!({"sequence":5,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"world"}}),
            json!({"sequence":6,"type":"step","name":"tool-use","data":{"turn_id":"t","request_id":"r","call":{"id":"call","name":"read","input":{"path":"file"}}}}),
            json!({"sequence":7,"type":"step","name":"tool-completed","data":{"turn_id":"t","request_id":"r","call_event_id":"dispatch","call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}}),
            json!({"sequence":8,"type":"step","name":"tool-result","data":{"turn_id":"t","request_id":"r","results":[{"call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}]}}),
            json!({"sequence":9,"type":"branch","name":"create","data":{"branch":"child","at":{"kind":"create"},"source":{"kind":"host","operation_id":"export-fixture"},"prompt":"child contents are elsewhere"}}),
            json!({"sequence":10,"type":"branch","name":"create-failed","data":{"status":"failed","branch":"child","createEvent":9,"error":"fixture"}}),
            json!({"sequence":15,"type":"branch","name":"create-failed","data":{"status":"cancelled","branch":"cancelled-child","createEvent":14,"reason":"fixture"}}),
        ];
        let returned = events.clone();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
            let first = wire.read("session.history");
            assert_eq!(first["params"],json!({"sessionId":"session","branchId":"main","after":0,"limit":256}));
            wire.reply(&first,json!({"events":&returned[..10],"through":20,"nextAfter":12,"done":false}));
            let second = wire.read("session.history");
            assert_eq!(second["params"]["after"],12); assert_eq!(second["params"]["through"],20);
            if mode == "history-error" {
                wire.send(json!({"jsonrpc":"2.0","id":second["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
            } else {
                wire.reply(&second,json!({"events":&returned[10..],"through":20,"nextAfter":20,"done":true}));
            }
        });
        let mut command = home.command();
        command.current_dir(&cli_dir).args([
            "session",
            "export",
            "session",
            "--format",
            if mode == "stdout-json" {
                "json"
            } else {
                "markdown"
            },
            "--cwd",
            workspace.to_str().unwrap(),
        ]);
        if mode != "stdout-json" {
            command.args(["--output", "Session notes.md"]);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        match mode {
            "stdout-json" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let exported: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(
                    exported,
                    json!({"sessionId":"session","branchId":"main","through":20,"events":events})
                );
            }
            "markdown" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stdout.is_empty());
                let markdown = fs::read_to_string(&path).unwrap();
                for text in [
                    "Keep ```code``` intact",
                    "[Image attachment: image/png]",
                    "### Reasoning",
                    "Hello world",
                    "### Tool call: read (call)",
                    "### Tool result: call",
                    "```````json",
                    "Child branch child (contents not exported).",
                    "Child branch child was not created.",
                    "Child branch cancelled-child was not created.",
                ] {
                    assert!(markdown.contains(text), "{text}: {markdown}");
                }
                assert_eq!(markdown.matches("# must remain inside fence").count(), 1);
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            "existing" => {
                assert!(!output.status.success());
                assert_eq!(fs::read(&path).unwrap(), b"keep original bytes");
                assert!(!String::from_utf8_lossy(&output.stderr).contains("Exported"));
            }
            _ => {
                assert!(!output.status.success());
                assert!(!path.exists());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("fixture history failure")
                );
            }
        }
        assert!(!workspace.join("Session notes.md").exists());
    }
}

#[test]
fn interactive_exports_keep_literal_paths_and_errors_out_of_model_input() {
    let home = Home::new();
    let cli_dir = home.0.join("client");
    let workspace = home.0.join("workspace");
    fs::create_dir_all(cli_dir.join("~")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        for _ in 0..2 {
            let history = wire.read("session.history");
            wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        }
        let history = wire.read("session.history");
        wire.send(json!({"jsonrpc":"2.0","id":history["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
        wire.detach();
    });
    let mut command = home.command();
    command
        .current_dir(&cli_dir)
        .args(["tui", "--cwd", workspace.to_str().unwrap()]);
    let (mut terminal, child) = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"/export markdown\n");
    terminal.until("~ is literal.");
    terminal.until("\n> ");
    terminal.write(b"/export yaml bad.md\n");
    terminal.until("Export format must be markdown or json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    terminal.until("Exported JSON: ~/Session notes.json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    terminal.until("Error:");
    terminal.until("\n> ");
    terminal.write(b"/export markdown failure.md\n");
    terminal.until("fixture history failure");
    terminal.until("\n> ");
    terminal.write(b"/exit extra\n");
    terminal.until("Usage: /exit");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    daemon.join().unwrap();
    let path = cli_dir.join("~/Session notes.json");
    let data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(data["events"], json!([]));
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!cli_dir.join("failure.md").exists());
    assert!(!workspace.join("~").exists());
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
    for explicit_exec in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], "!!literal prompt\n");
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.end(
                json!({"turn_id":"turn","status":"failed","error":"credential should not print"}),
            );
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"!!literal prompt\n")
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("credential should not print"));
    }
    for explicit_exec in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            // Rejection leaves the connection usable for ordinary cleanup.
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&vec![b'"'; 8 * 1024 * 1024])
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("App Server request exceeds 16 MiB after JSON encoding")
        );
    }
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
fn interactive_shell_prefixes_use_session_methods_and_keep_failures_out_of_model_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        // Bare prefixes print usage; they never become model input.
        for _ in 0..2 { let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":8})); }
        let mut identities = std::collections::HashSet::new();
        for (mode, command) in [("record", " printf 'recorded' "), ("private", "printf private"), ("record", "broken"), ("record", "busy"), ("record", "unknown"), ("record", "opaque"), ("private", "!nested")] {
            let run = wire.read("shell.run");
            assert_eq!(run["params"]["sessionId"], "session");
            assert_eq!(run["params"]["branchId"], "main");
            assert_eq!(run["params"]["mode"], mode);
            assert_eq!(run["params"]["command"], command);
            let id = run["params"]["runId"].as_str().unwrap();
            assert!(uuid::Uuid::parse_str(id).is_ok());
            assert!(identities.insert(id.to_owned()));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":8}));
            if command == "unknown" || command == "opaque" {
                wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32601,"message":"Unavailable","data":if command == "unknown" {json!({"details":"opaque-private"})} else {json!("opaque-private")}}}));
                continue;
            }
            if command == "busy" {
                wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32000,"message":"Already running","data":{"appError":"shell_busy"}}}));
                continue;
            }
            let mut reply = json!({"runId":id,"status":"completed","exitCode":0,"output":"final-\u{1b}[31mresult\u{1b}[0m\u{202e}","truncated":false,"durationMs":1});
            if mode == "record" {
                reply["recorded"] = json!({"sequence":9});
            }
            if command == "broken" {
                reply["status"] = "failed".into();
                reply["reason"] = "read failed".into();
                reply["truncated"] = true.into();
                reply["recorded"] = json!({"error":{"appError":"branch_closed","message":"Branch closed"}});
            }
            wire.reply(&run, reply);
        }
        let submit=wire.read("session.submit"); assert_eq!(submit["params"]["text"],"  !literal"); wire.reply(&submit,json!({"event":{}}));
        let idle=wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"!\n");
    terminal.until("Usage: !command");
    terminal.until("\n> ");
    terminal.write(b"!!\n");
    terminal.until("Usage: !command");
    terminal.until("\n> ");
    terminal.write(b"! printf 'recorded' \n");
    terminal.until("[shell recorded] sequence: 9");
    terminal.until("\n> ");
    terminal.write(b"!!printf private\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"!broken\n");
    terminal.until("[shell recording failed] Branch closed (branch_closed)");
    terminal.until("\n> ");
    terminal.write(b"!busy\n");
    terminal.until("[shell error] Already running (RPC -32000, shell_busy)");
    terminal.until("\n> ");
    for command in ["!unknown", "!opaque"] {
        terminal.write(format!("{command}\n").as_bytes());
        terminal.until("[shell error] Unavailable (RPC -32601)");
        terminal.until("\n> ");
    }
    terminal.write(b"!!!nested\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"  !literal\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("final-result"));
    assert!(terminal.transcript.contains("(output truncated)"));
    assert_eq!(terminal.transcript.matches("[shell recorded]").count(), 1);
    assert!(!terminal.transcript.contains("opaque-private"));
    assert!(!terminal.transcript.contains('\u{1b}'));
    assert!(!terminal.transcript.contains('\u{202e}'));
}

#[test]
fn interactive_shell_runs_during_a_turn_preserve_approval_focus_and_two_stage_interrupts() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "work");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working"}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["mode"], "record");
        assert_eq!(run["params"]["command"], "inspect");
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":null}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"li\u{1b}[31mve","skipped":7}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":" out\u{202e}put"}}));
        wire.notice("management.progress", json!({"id":99999,"method":"shell.run","data":{"text":"foreign progress"}}));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"inspect"}},"reason":"review"}));
        // Completion must not print into the approval answer prompt.
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"authoritative output","truncated":false,"durationMs":1,"recorded":{"sequence":12}}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"late progress must be ignored"}}));
        let answer = wire.read("approval.respond");
        assert_eq!(answer["params"]["decision"], "deny");
        wire.reply(&answer, json!({}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["mode"], "private");
        assert_eq!(run["params"]["command"], "wait");
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"waiting"}}));
        let shell_stop = wire.read("shell.stop");
        assert_eq!(shell_stop["params"]["runId"], run["params"]["runId"]);
        let stop = wire.read("session.stop");
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false,"durationMs":0}));
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&stop, json!({"sequence":40}));
        wire.reply(&idle, json!({"sequence":40}));
        // The CLI must consume the late shell-stop response before another input.
        wire.reply(&shell_stop, json!({"stopped":false}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "next");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":40}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"work\n");
    terminal.until("model working");
    terminal.write(b"ordinary typeahead\n");
    terminal.until("[busy] Wait for the prompt");
    terminal.write(b"!inspect\n");
    terminal.until("Allow once? [y/N] ");
    assert!(!terminal.transcript.contains("authoritative output"));
    terminal.write(b"!never-execute-this-answer\n");
    terminal.until("[shell recorded] sequence: 12");
    terminal.write(b"!!wait\n");
    terminal.until("waiting");
    terminal.write(b"!another\n");
    terminal.until("[shell busy]");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    terminal.until("[shell stopping] Waiting for command settlement");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    terminal.until("[stopping] Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(b"next\n");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(wait(child).status.success());
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("skipped 7 bytes"));
    assert!(!terminal.transcript.contains("foreign progress"));
    assert_eq!(terminal.transcript.matches("[shell started]").count(), 2);
    assert_eq!(
        terminal
            .transcript
            .matches("[shell progress] (incomplete)")
            .count(),
        2
    );
    assert!(terminal.transcript.contains("live output"));
    assert!(terminal.transcript.contains("[shell progress error]"));
    assert!(
        !terminal
            .transcript
            .contains("late progress must be ignored")
    );
    assert!(!terminal.transcript.contains('\u{1b}'));
    assert!(!terminal.transcript.contains('\u{202e}'));
}

#[test]
fn interactive_shell_eof_stops_an_unanswered_run_and_detaches_after_both_replies() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let run = wire.read("shell.run");
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":8}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"before admission"}}));
        let stop = wire.read("shell.stop");
        assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false,"durationMs":0}));
        wire.reply(&stop, json!({"stopped":false}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"!wait\n");
    terminal.until("before admission");
    terminal.write(&[4]);
    assert!(wait(child).status.success());
    daemon.join().unwrap();
}

#[test]
fn interactive_default_deny_discards_typeahead_and_ctrl_c_settles_before_next_input() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"first-request","textOffset":0,"text":"working"}));
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
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"second-request","textOffset":0,"text":"partial"}));
        let ended = json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":35,"type":"turn","name":"end","data":{"turn_id":"second-turn","status":"cancelled","reason":"interrupted"}}}}).to_string();
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
    let (mut terminal, child) = Terminal::launch(home.command());
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
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"streaming"}));
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
fn project_mcp_entries_start_disabled_and_the_user_enables_them() {
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
    // The project file asks to be enabled; only the user can enable it.
    fs::write(project.join(".mcp.json"), json!({"mcpServers":{"docs":{"command":"missing-mcp-program","args":["original"],"env":{"SECRET":"must-not-print"},"enabled":true}}}).to_string()).unwrap();
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
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\n");
    let written = fs::read(project.join(".mcp.json")).unwrap();
    let enabled = run(&["mcp", "enable", "docs"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        String::from_utf8(enabled.stdout).unwrap(),
        "Enabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&enabled.stderr).contains(".mcp.json"));
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tenabled\n");
    // The choice belongs to this user and home, not the shared project file.
    assert_eq!(fs::read(project.join(".mcp.json")).unwrap(), written);
    assert!(run(&["daemon", "stop"]).status.success());
    assert_eq!(
        list(),
        "\"docs\"\tproject\tstdio\tenabled\n",
        "restart lost the choice"
    );
    assert!(run(&["mcp", "disable", "docs"]).status.success());
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\n");
    // An entry that leaves the file can still be disabled, and comes back off.
    assert!(run(&["mcp", "enable", "docs"]).status.success());
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{}}).to_string(),
    )
    .unwrap();
    let unlisted = run(&["mcp", "disable", "docs"]);
    assert!(unlisted.status.success());
    assert!(String::from_utf8_lossy(&unlisted.stderr).contains("\"docs\" is not in mcp list"));
    fs::write(project.join(".mcp.json"), &written).unwrap();
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\n");
    // Disabling does not need a readable .mcp.json.
    assert!(run(&["mcp", "enable", "docs"]).status.success());
    fs::write(project.join(".mcp.json"), "{broken").unwrap();
    assert!(!run(&["mcp", "list"]).status.success());
    let unreadable = run(&["mcp", "disable", "docs"]);
    assert!(
        unreadable.status.success(),
        "{}",
        String::from_utf8_lossy(&unreadable.stderr)
    );
    assert_eq!(
        String::from_utf8(unreadable.stdout).unwrap(),
        "Disabled \"docs\" (project).\n"
    );
    assert!(String::from_utf8_lossy(&unreadable.stderr).contains("could not be read"));
    fs::write(project.join(".mcp.json"), &written).unwrap();
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\n");
    // A configuration the daemon cannot apply is reported on stderr.
    fs::write(
        project.join(".mcp.json"),
        json!({"mcpServers":{"remote":{"type":"http","url":"not a url"}}}).to_string(),
    )
    .unwrap();
    assert!(run(&["mcp", "enable", "remote"]).status.success());
    let reported = (0..50).any(|_| {
        let output = run(&["mcp", "list"]);
        let reported = String::from_utf8_lossy(&output.stderr)
            .contains("project MCP configuration: MCP configuration is not active");
        if !reported {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        reported
    });
    assert!(
        reported,
        "mcp list did not report the inactive configuration"
    );
    assert!(run(&["mcp", "disable", "remote"]).status.success());
    fs::write(project.join(".mcp.json"), &written).unwrap();
    let missing = run(&["mcp", "enable", "absent"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No MCP server \"absent\""));
    for removed in [
        &["mcp", "approve", "docs", "token"][..],
        &["mcp", "revoke", "docs"],
    ] {
        let output = run(removed);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("mcp enable ID"));
    }
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
                } else {
                    let messages = request["messages"].to_string();
                    assert!(messages.contains("dim-shell-input"));
                    assert!(messages.contains("cli-recorded-marker"));
                    assert!(!messages.contains("cli-private-marker"));
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
            "exec",
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
    let mut interactive = home.command();
    interactive.args([
        "--server",
        &binary,
        "--model-config",
        config.to_str().unwrap(),
        "--cwd",
        home.0.to_str().unwrap(),
        "--resume",
        session,
    ]);
    let (mut terminal, child) = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.write(b"!printf cli-recorded-marker\n");
    terminal.until("[shell recorded] sequence:");
    terminal.until("\n> ");
    terminal.write(b"!!printf cli-private-marker\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    let mut resumed = home.command();
    resumed
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
            "exec",
            "resume",
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
    let exported = home
        .command()
        .args([
            "--server",
            &binary,
            "--model-config",
            config.to_str().unwrap(),
            "session",
            "export",
            session,
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let conversation: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(conversation["sessionId"], session);
    assert_eq!(conversation["branchId"], "main");
    assert_eq!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["blocks"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("<dim-shell-input ")))
            .count(),
        1
    );
    assert!(!conversation.to_string().contains("cli-private-marker"));
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["name"] == "text" && event["data"]["delta"] == "session continued")
    );
    assert!(
        !String::from_utf8(exported.stdout)
            .unwrap()
            .contains("fixture-secret")
    );
    model.join().unwrap();
}

#[test]
fn initial_attach_eof_settles_a_shell_started_before_the_first_prompt() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":"0.1","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach, json!({}));
        let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"restored","textOffset":0,"text":"initial model pending"}));
        let run = wire.read("shell.run");
        wire.reply(&idle, json!({"sequence":8}));
        wire.notice("management.progress", json!({"id":run["id"],"method":"shell.run","data":{"text":"initial shell pending"}}));
        let stop = wire.read("shell.stop"); assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"stopped","output":"","truncated":false}));
        wire.reply(&stop, json!({"stopped":true}));
        wire.detach();
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let (mut terminal, child) = Terminal::launch(command);
    terminal.until("initial model pending");
    terminal.write(b"!!wait\n");
    terminal.until("initial shell pending");
    terminal.write(&[4]);
    assert!(wait(child).status.success());
    daemon.join().unwrap();
}

#[test]
fn interrupted_shell_reply_retains_its_run_for_explicit_stop_and_settlement_feedback() {
    let home = Home::new();
    let (release, finish) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        for malformed in [false, true] {
            let run = wire.read("shell.run");
            let idle = wire.read("session.idle"); wire.reply(&idle, json!({"sequence":8}));
            if malformed { wire.reply(&run, json!({"runId":"wrong","status":"completed","output":"","truncated":false})); }
            else { wire.send(json!({"jsonrpc":"2.0","id":run["id"],"error":{"code":-32000,"message":"Reloaded","data":{"appError":"operation_interrupted"}}})); }
            let stop = wire.read("shell.stop"); assert_eq!(stop["params"]["runId"], run["params"]["runId"]);
            if !malformed { finish.recv_timeout(Duration::from_secs(10)).unwrap(); }
            if !malformed {
                wire.send(json!({"jsonrpc":"2.0","id":stop["id"],"error":{"code":-32000,"message":"Reloaded","data":{"appError":"operation_interrupted"}}}));
                let retry=wire.read("shell.stop"); assert_ne!(retry["id"], stop["id"]); assert_eq!(retry["params"]["runId"], run["params"]["runId"]);
                wire.reply(&retry,json!({"stopped":false}));
            } else { wire.reply(&stop, json!({"stopped":false})); }
        }
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    for malformed in [false, true] {
        terminal.write(b"!held\n");
        terminal.until("[shell interrupted] Run");
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
        terminal.until("[shell stopping] Waiting for command settlement");
        if !malformed {
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
            terminal.until("Still waiting for command settlement");
            release.send(()).unwrap();
            terminal.until("Stop was not confirmed");
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
            terminal.until("[shell stopping] Waiting for command settlement");
        }
        terminal.until("\n> ");
    }
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    daemon.join().unwrap();
}

#[test]
fn approval_interrupt_stops_the_turn_instead_of_a_deferred_completed_shell() {
    let home = Home::new();
    let (release, finish) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"working","textOffset":0,"text":"model running"}));
        let run = wire.read("shell.run");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"inspect"}},"reason":"review"}));
        wire.reply(&run,json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"deferred output","truncated":false}));
        let stop=wire.read("session.stop");
        finish.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.notice("approval.cancelled",json!({"sessionId":"session","requestId":"approval"}));
        wire.reply(&idle,json!({"sequence":40})); wire.reply(&stop,json!({"sequence":40}));
        let next = wire.read("session.submit");
        assert_eq!(next["params"]["text"], "after interrupts");
        wire.reply(&next,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle,json!({"sequence":40}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"work\n");
    terminal.until("model running");
    terminal.write(b"!inspect\n");
    terminal.until("Allow once? [y/N] ");
    assert!(!terminal.transcript.contains("deferred output"));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    terminal.until("[stopping] Waiting for resource settlement");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    terminal.until("[stopping] Still waiting for resource settlement");
    release.send(()).unwrap();
    terminal.until("\n> ");
    terminal.write(b"after interrupts\n");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("[shell stopping]"));
    assert!(
        terminal.transcript.find("deferred output").unwrap()
            > terminal.transcript.find("[stopping]").unwrap()
    );
}
