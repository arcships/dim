//! Permission controls: the startup preset, `/approvals` and "always allow",
//! against a scripted daemon and against the built App Server.
use super::*;

const PRESET_METHODS: [&str; 2] = ["session.permission.get", "session.permission.update"];
const RULE_METHODS: [&str; 2] = ["permission.rules", "permission.set_rules"];

fn fail(wire: &mut Wire, request: &Value, app_error: &str) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":format!("fixture {app_error}"),"data":{"appError":app_error}}}));
}

fn policy(preset: &str, allowed: &[&str], disallowed: &[&str]) -> Value {
    json!({"preset":preset,"allowedTools":allowed,"disallowedTools":disallowed})
}

fn branch() -> Value {
    json!({"sessionId":"session","branchId":"main"})
}

/// One submitted turn that completes without tool calls.
fn quiet_turn(wire: &mut Wire) {
    let submit = wire.read("session.submit");
    wire.reply(&submit, json!({"event":{}}));
    let idle = wire.read("session.idle");
    wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
    wire.reply(&idle, json!({"sequence":25}));
    wire.detach();
}

#[test]
fn permission_flag_selects_the_new_and_resumed_preset_with_version_checks() {
    // A new Session receives the preset in its creation policy.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&PRESET_METHODS);
        let create = wire.created();
        assert_eq!(
            create["params"]["permissionPolicy"],
            json!({"preset":"read-only"})
        );
        wire.attached();
        quiet_turn(wire);
    });
    let output = home
        .command()
        .args(["exec", "--permission", "read-only", "-p", "text"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("permission: read-only"));

    // A resumed Session changes main; a conflict reads again and keeps the tool lists.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&PRESET_METHODS);
        let get = wire.read("session.permission.get");
        assert_eq!(get["params"], branch());
        wire.reply(
            &get,
            json!({"version":4,"policy":policy("workspace-write", &["fixture-tool"], &["blocked"])}),
        );
        let update = wire.read("session.permission.update");
        assert_eq!(
            update["params"],
            json!({"sessionId":"session","branchId":"main","expectedVersion":4,
                "policy":policy("full-access", &["fixture-tool"], &["blocked"])})
        );
        fail(wire, &update, "permission_conflict");
        let get = wire.read("session.permission.get");
        wire.reply(
            &get,
            json!({"version":6,"policy":policy("read-only", &[], &[])}),
        );
        let update = wire.read("session.permission.update");
        assert_eq!(update["params"]["expectedVersion"], 6);
        assert_eq!(update["params"]["policy"], policy("full-access", &[], &[]));
        wire.reply(
            &update,
            json!({"version":9,"policy":policy("full-access", &[], &[])}),
        );
        wire.attached();
        quiet_turn(wire);
    });
    let output = home
        .command()
        .args([
            "exec",
            "resume",
            "session",
            "--permission",
            "full-access",
            "-p",
            "text",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("permission: full-access"));

    // The preset already in force is not written again.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&PRESET_METHODS);
        let get = wire.read("session.permission.get");
        wire.reply(
            &get,
            json!({"version":2,"policy":policy("read-only", &[], &[])}),
        );
        wire.attached();
        quiet_turn(wire);
    });
    let output = home
        .command()
        .args([
            "--resume",
            "session",
            "--permission",
            "read-only",
            "-p",
            "text",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Without the methods the resumed Session is never attached or submitted to.
    let home = Home::new();
    let daemon = home.daemon(|wire| wire.hello(&[]));
    let output = home
        .command()
        .args([
            "exec",
            "resume",
            "session",
            "--permission",
            "read-only",
            "-p",
            "text",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot change a Session's permission")
    );
}

#[test]
fn interactive_always_allow_writes_rules_retries_conflicts_and_falls_back_to_once() {
    let home = Home::new();
    let (reading, wait_reading) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&RULE_METHODS);
        wire.created();
        wire.attached();
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        let approval = |wire: &mut Wire, id: &str, command: &str| {
            wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":id,"call":{"name":"exec","input":{"command":command}},"reason":"fixture review"}));
        };
        let answered = |wire: &mut Wire, id: &str, decision: &str| {
            let respond = wire.read("approval.respond");
            assert_eq!(respond["params"]["requestId"], id);
            assert_eq!(respond["params"]["decision"], decision);
            wire.reply(&respond, json!({}));
        };
        let deny_push = json!({"command":"git push","match":"prefix","decision":"deny"});
        let allow_test = json!({"command":"cargo test","match":"prefix","decision":"allow"});
        // A prefix rule; the first write conflicts and the rule joins the newer list.
        approval(wire, "first", "cargo test -p fixture");
        let rules = wire.read("permission.rules");
        wire.reply(&rules, json!({"revision":"r1","commandRules":[]}));
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r1","commandRules":[allow_test]})
        );
        fail(wire, &write, "permission_rules_conflict");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r2","commandRules":[deny_push]}),
        );
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r2","commandRules":[deny_push, allow_test]})
        );
        wire.reply(&write, json!({"revision":"r3"}));
        answered(wire, "first", "allow");
        // A failed write still lets the user answer once.
        approval(wire, "second", "cargo build --release");
        let rules = wire.read("permission.rules");
        fail(wire, &rules, "permission_rules_failed");
        answered(wire, "second", "allow");
        // A command with a separator is never offered "always".
        approval(wire, "third", "cargo test && rm -rf target");
        answered(wire, "third", "deny");
        // Other words re-ask; an existing rule is not written again.
        approval(wire, "fourth", "cargo fmt");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r3","commandRules":[deny_push, allow_test,
                {"command":"cargo  fmt","match":"exact","decision":"allow"}]}),
        );
        answered(wire, "fourth", "allow");
        // Ctrl+C during the write abandons the rule and answers nothing.
        approval(wire, "fifth", "cargo clippy");
        let rules = wire.read("permission.rules");
        reading.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.reply(&rules, json!({"revision":"r3","commandRules":[]}));
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&stop, json!({"sequence":25}));
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"run the checks\n");
    terminal.until("command: cargo test -p fixture");
    terminal.until("Allow once, always, or deny? [y/a/N] ");
    terminal.write(b"a\n");
    terminal.until(
        "Always allow `cargo test -p fixture` (exact). Press Enter, or type its leading words for a prefix rule: ",
    );
    terminal.write(b"cargo test\n");
    terminal.until("[rule saved] allow prefix `cargo test`");
    terminal.until("Allow once, always, or deny? [y/a/N] ");
    terminal.write(b"always\n");
    terminal.until("Always allow `cargo build --release` (exact).");
    terminal.write(b"\n");
    terminal.until("[rule not saved] fixture permission_rules_failed");
    terminal.until("command: cargo build --release");
    terminal.until("Allow once? [y/N] ");
    terminal.write(b"y\n");
    terminal.until("command: cargo test && rm -rf target");
    terminal.until("Allow once? [y/N] ");
    terminal.write(b"a\n");
    terminal.until("command: cargo fmt");
    terminal.until("Allow once, always, or deny? [y/a/N] ");
    terminal.write(b"a\n");
    terminal.until("Always allow `cargo fmt` (exact).");
    terminal.write(b"rustfmt\n");
    terminal.until("Type the command's leading words, or press Enter for the exact rule: ");
    terminal.write(b"\n");
    terminal.until("[rule present] allow exact `cargo fmt`");
    terminal.until("Allow once, always, or deny? [y/a/N] ");
    terminal.write(b"a\n");
    terminal.until("Always allow `cargo clippy` (exact).");
    terminal.write(b"\n");
    wait_reading.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    terminal.until("[rule not saved] the turn was stopped before the rule was written");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    daemon.join().unwrap();
}

#[test]
fn interactive_approvals_list_switch_and_edit_rules_with_version_checks() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = PRESET_METHODS.to_vec();
        methods.extend(RULE_METHODS);
        wire.hello(&methods);
        wire.created();
        // Interactive start shows the preset.
        let get = wire.read("session.permission.get");
        wire.reply(
            &get,
            json!({"version":0,"policy":policy("workspace-write", &[], &["blocked-tool"])}),
        );
        wire.attached();
        let state = |wire: &mut Wire, version: u64, preset: &str| {
            let get = wire.read("session.permission.get");
            assert_eq!(get["params"], branch());
            wire.reply(
                &get,
                json!({"version":version,"policy":policy(preset, &[], &["blocked-tool"])}),
            );
        };
        let allow_test = json!({"command":"cargo test","match":"exact","decision":"allow"});
        let deny_push = json!({"command":"git push","match":"prefix","decision":"deny"});
        let allow_make = json!({"command":"make","match":"exact","decision":"allow"});
        let allow_check = json!({"command":"cargo check","match":"exact","decision":"allow"});
        // /approvals
        state(wire, 3, "workspace-write");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r1","commandRules":[allow_test, deny_push]}),
        );
        // /approvals full-access, declined
        state(wire, 3, "workspace-write");
        // /approvals full-access, confirmed but changed elsewhere
        state(wire, 3, "workspace-write");
        let update = wire.read("session.permission.update");
        assert_eq!(
            update["params"],
            json!({"sessionId":"session","branchId":"main","expectedVersion":3,
                "policy":policy("full-access", &[], &["blocked-tool"])})
        );
        fail(wire, &update, "permission_conflict");
        state(wire, 5, "read-only");
        // /approvals read-only
        state(wire, 5, "read-only");
        // /permissions workspace-write
        state(wire, 5, "read-only");
        let update = wire.read("session.permission.update");
        assert_eq!(update["params"]["expectedVersion"], 5);
        assert_eq!(
            update["params"]["policy"],
            policy("workspace-write", &[], &["blocked-tool"])
        );
        wire.reply(
            &update,
            json!({"version":10,"policy":policy("workspace-write", &[], &["blocked-tool"])}),
        );
        // /approvals remove 1, against the listing
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r1","commandRules":[deny_push]})
        );
        wire.reply(&write, json!({"revision":"r2"}));
        // /approvals remove 1, changed since the listing
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r2","commandRules":[]})
        );
        fail(wire, &write, "permission_rules_conflict");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r4","commandRules":[deny_push, allow_make]}),
        );
        // /approvals deny prefix git push
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r4","commandRules":[deny_push, allow_make]}),
        );
        // /approvals allow exact cargo check
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r4","commandRules":[deny_push, allow_make]}),
        );
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r4","commandRules":[deny_push, allow_make, allow_check]})
        );
        wire.reply(&write, json!({"revision":"r5"}));
        // /approvals remove 3 refers to the listing extended by the addition.
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r5","commandRules":[deny_push, allow_make]})
        );
        wire.reply(&write, json!({"revision":"r6"}));
        // Two removals in a row: the second number refers to the list the
        // first one printed.
        let deny = |command: &str| json!({"command":command,"match":"exact","decision":"deny"});
        state(wire, 10, "workspace-write");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r7","commandRules":[deny("alpha"), deny("beta"), deny("gamma")]}),
        );
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r7","commandRules":[deny("beta"), deny("gamma")]})
        );
        wire.reply(&write, json!({"revision":"r8"}));
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r8","commandRules":[deny("beta")]})
        );
        wire.reply(&write, json!({"revision":"r9"}));
        // Changes during a turn: this client's own write is already reported.
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "after");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        for (sequence, preset) in [(10, "workspace-write"), (11, "full-access")] {
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":format!("permission-{sequence}"),"sequence":sequence,"create_at":1,"type":"permission","name":"changed","data":{"policy":policy(preset, &[], &[])}}}));
        }
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":25}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("permission: workspace-write");
    terminal.until("\n> ");
    terminal.write(b"/approvals\n");
    terminal.until("Preset (main branch): workspace-write");
    terminal.until("  read-only        reads run;");
    terminal.until("* workspace-write  reads and file writes run;");
    terminal.until("  full-access      every tool call runs without asking");
    terminal.until("  disallowed tools: blocked-tool");
    terminal.until("Command rules (user scope, every Session):");
    terminal.until("  1  allow  exact   cargo test");
    terminal.until("  2  deny   prefix  git push");
    terminal.until("Usage: /approvals");
    terminal.until("\n> ");
    terminal.write(b"/approvals full-access\n");
    terminal.until("Switch the main branch to full-access? [y/N] ");
    terminal.write(b"\n");
    terminal.until("Preset unchanged: workspace-write");
    terminal.until("\n> ");
    terminal.write(b"/approvals full-access\n");
    terminal.until("Switch the main branch to full-access? [y/N] ");
    terminal.write(b"y\n");
    terminal.until("The preset changed elsewhere and is now read-only; nothing was changed.");
    terminal.until("\n> ");
    terminal.write(b"/approvals read-only\n");
    terminal.until("Preset (main branch) is already read-only");
    terminal.until("\n> ");
    terminal.write(b"/permissions workspace-write\n");
    terminal.until("Preset (main branch): workspace-write. Child branches created later inherit it; agent members keep the Session's starting preset.");
    terminal.until("\n> ");
    // Every write prints the numbered list that `remove N` refers to next.
    terminal.write(b"/approvals remove 1\n");
    terminal.until("Rule removed: allow exact `cargo test`");
    terminal.until("Command rules (user scope, every Session):");
    terminal.until("  1  deny   prefix  git push");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove 1\n");
    terminal.until("Command rules changed since they were listed; nothing was removed.");
    terminal.until("  1  deny   prefix  git push");
    terminal.until("  2  allow  exact   make");
    terminal.until("\n> ");
    terminal.write(b"/approvals deny prefix git push\n");
    terminal.until("Rule already present: deny prefix `git push`");
    terminal.until("\n> ");
    terminal.write(b"/approvals allow exact cargo check\n");
    terminal.until("Rule added: allow exact `cargo check`");
    terminal.until("  3  allow  exact   cargo check");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove 3\n");
    terminal.until("Rule removed: allow exact `cargo check`");
    terminal.until("  2  allow  exact   make");
    terminal.until("\n> ");
    terminal.write(b"/approvals\n");
    terminal.until("  1  deny   exact   alpha");
    terminal.until("  2  deny   exact   beta");
    terminal.until("  3  deny   exact   gamma");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove 1\n");
    terminal.until("Rule removed: deny exact `alpha`");
    terminal.until("  1  deny   exact   beta");
    terminal.until("  2  deny   exact   gamma");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove 2\n");
    terminal.until("Rule removed: deny exact `gamma`");
    terminal.until("  1  deny   exact   beta");
    terminal.until("\n> ");
    // Rejected before any call.
    terminal.write(b"/approvals allow exact cargo test; rm -rf target\n");
    terminal.until("Error: A command rule needs a simple command");
    terminal.until("\n> ");
    terminal.write(b"/approvals admin\n");
    terminal.until("Error: Unknown permission preset");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove\n");
    terminal.until("Error: Usage: /approvals");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove 9\n");
    terminal.until("Error: No such rule in the last /approvals listing");
    terminal.until("\n> ");
    terminal.write(b"after\n");
    terminal.until("[permission] main branch preset: full-access");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    daemon.join().unwrap();
    assert!(
        !terminal
            .transcript
            .contains("[permission] main branch preset: workspace-write"),
        "{}",
        terminal.transcript
    );
}

/// An OpenAI-compatible model for the built App Server: a user message
/// `exec: COMMAND` asks for that `exec` call, and a tool result is answered
/// with `tool said: CONTENT`. Replies depend only on the request, so a turn
/// stopped after a denial may or may not ask again.
struct Model {
    address: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Model {
    fn start() -> Self {
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => Self::answer(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }
    fn answer(stream: std::net::TcpStream) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let last = request["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        let text = |content: &Value| match content {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
            other => other.to_string(),
        };
        let frame = |delta: Value, finish: Value| {
            format!(
                "data: {}\n\n",
                json!({"id":"fixture-response","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
            )
        };
        let data = if last["role"] == "tool" {
            frame(
                json!({"content":format!("tool said: {}", text(&last["content"]))}),
                json!("stop"),
            )
        } else if let Some(command) = text(&last["content"]).strip_prefix("exec: ") {
            let arguments = json!({"command":command}).to_string();
            frame(
                json!({"tool_calls":[{"index":0,"id":format!("call-{}", uuid::Uuid::new_v4()),"type":"function","function":{"name":"exec","arguments":arguments}}]}),
                Value::Null,
            ) + &frame(json!({}), json!("tool_calls"))
        } else {
            frame(json!({"content":"nothing to run"}), json!("stop"))
        } + "data: [DONE]\n\n";
        write!(
            stream.get_mut(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
            data.len()
        )
        .unwrap();
        stream.get_mut().flush().unwrap();
    }
    fn config(&self, home: &Home) -> PathBuf {
        let config = home.0.join("model.json");
        fs::write(&config, json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","apiKeyEnv":"DIM_CLI_TEST_API_KEY","baseUrl":format!("http://{}/v1", self.address),"models":[{"id":"fixture-model","config":{"maxTokens":128}}]}]}).to_string()).unwrap();
        config
    }
}
impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let joined = self.thread.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}

/// The built App Server with a fixture model, in its own home and workspace.
struct RealDaemon {
    home: Home,
    binary: String,
    config: PathBuf,
    work: PathBuf,
    _model: Model,
}
impl RealDaemon {
    fn start() -> Self {
        let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
        let home = Home::new();
        let model = Model::start();
        let config = model.config(&home);
        let work = home.0.join("work");
        fs::create_dir(&work).unwrap();
        Self {
            home,
            binary,
            config,
            work,
            _model: model,
        }
    }
    fn command(&self) -> Command {
        let mut command = self.home.command();
        command
            .arg("--server")
            .arg(&self.binary)
            .arg("--model-config")
            .arg(&self.config)
            .arg("--cwd")
            .arg(&self.work)
            .env("DIM_CLI_TEST_API_KEY", "fixture-secret");
        command
    }
    fn run(&self, args: &[&str]) -> (Option<i32>, String, String) {
        let output = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }
}
impl Drop for RealDaemon {
    fn drop(&mut self) {
        let _ = self.run(&["daemon", "stop"]);
        let _ = OwnedDaemon(self.home.0.join("rust/daemon.sock"));
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn real_daemon_exec_presets_decide_process_tools_without_automatic_approval() {
    let daemon = RealDaemon::start();
    // The default preset still asks, so headless exec denies and exits 3.
    let (code, _, stderr) = daemon.run(&["exec", "-p", "exec: printf default-ran"]);
    assert_eq!(code, Some(3), "{stderr}");
    assert!(stderr.contains("Interactive approval required"), "{stderr}");
    assert!(!stderr.contains("permission:"), "{stderr}");
    let session = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap()
        .to_owned();
    // full-access runs the process tool without a question.
    let (code, stdout, stderr) = daemon.run(&[
        "exec",
        "--permission",
        "full-access",
        "-p",
        "exec: printf full-ran",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("tool said:") && stdout.contains("full-ran"),
        "{stdout}"
    );
    assert!(stderr.contains("permission: full-access"), "{stderr}");
    assert!(stderr.contains("[done] exec"), "{stderr}");
    assert!(!stderr.contains("approval"), "{stderr}");
    // Resume with the flag changes the resumed branch; without it the change stays.
    for (args, marker) in [
        (vec!["--permission", "full-access"], "resumed-ran"),
        (vec![], "kept-ran"),
    ] {
        let prompt = format!("exec: printf {marker}");
        let mut command = vec!["exec", "resume", session.as_str()];
        command.extend(args);
        command.extend(["-p", prompt.as_str()]);
        let (code, stdout, stderr) = daemon.run(&command);
        assert_eq!(code, Some(0), "{stderr}");
        assert!(stdout.contains(marker), "{stdout}");
        assert!(!stderr.contains("approval"), "{stderr}");
    }
    // read-only denies without a question; the model sees Permission's denial.
    let (code, stdout, stderr) = daemon.run(&[
        "exec",
        "resume",
        &session,
        "--permission",
        "read-only",
        "-p",
        "exec: printf denied-ran",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("not allowed by the read-only preset"),
        "{stdout}"
    );
    assert!(!stdout.contains("denied-ran"), "{stdout}");
    assert!(!stderr.contains("approval"), "{stderr}");
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn real_daemon_always_allow_saves_an_exact_rule_until_approvals_removes_it() {
    let daemon = RealDaemon::start();
    let (mut terminal, child) = Terminal::launch(daemon.command());
    terminal.until("permission: workspace-write");
    terminal.until("Ctrl+J: newline");
    terminal.write(b"exec: printf always-ran\n");
    terminal.until("command: printf always-ran");
    terminal.until("Allow once, always, or deny? [y/a/N] ");
    terminal.write(b"a\n");
    terminal.until("Press Enter, or type its leading words for a prefix rule: ");
    terminal.write(b"\n");
    terminal.until("[rule saved] allow exact `printf always-ran`");
    terminal.until("tool said:");
    terminal.editor();
    // The identical call now runs without asking.
    let mark = terminal.transcript.len();
    terminal.write(b"exec: printf always-ran\n");
    terminal.until("tool said:");
    terminal.editor();
    assert!(
        !terminal.transcript[mark..].contains("[approval]"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"/approvals\n");
    terminal.until("* workspace-write");
    terminal.until("  1  allow  exact   printf always-ran");
    terminal.editor();
    terminal.write(b"/approvals remove 1\n");
    terminal.until("Rule removed: allow exact `printf always-ran`");
    terminal.editor();
    terminal.write(b"/approvals read-only\n");
    terminal.until("Preset (main branch): read-only.");
    terminal.editor();
    let mark = terminal.transcript.len();
    terminal.write(b"exec: printf always-ran\n");
    terminal.until("not allowed by the read-only preset");
    terminal.editor();
    assert!(
        !terminal.transcript[mark..].contains("[approval]"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"/exit\n");
    assert!(wait(child).status.success());
    // Without the rule, a new Session asks again.
    let (code, _, stderr) = daemon.run(&["exec", "-p", "exec: printf always-ran"]);
    assert_eq!(code, Some(3), "{stderr}");
}
