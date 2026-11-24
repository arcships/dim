#![cfg(unix)]

#[path = "support/cli_harness.rs"]
mod harness;
#[allow(
    unused_imports,
    reason = "submodules re-import shared harness items via super::*"
)]
use harness::*;

#[path = "cli_flow/accounts.rs"]
mod accounts;
#[path = "cli_flow/acp.rs"]
mod acp;
#[path = "cli_flow/hooks.rs"]
mod hooks;
#[path = "cli_flow/lines.rs"]
mod lines;
#[path = "cli_flow/mcp.rs"]
mod mcp;
#[path = "cli_flow/permission.rs"]
mod permission;
#[path = "cli_flow/staged_update.rs"]
mod staged_update;
#[path = "cli_flow/workflow.rs"]
mod workflow;

#[test]
#[ignore = "requires DIM_TEST_NPM_WRAPPER pointing to npm/bin/dimcode.mjs"]
fn unix_npm_wrapper_preserves_the_native_terminal_and_interrupt_flow() {
    let source =
        PathBuf::from(std::env::var("DIM_TEST_NPM_WRAPPER").expect("DIM_TEST_NPM_WRAPPER"));
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
    let wrapper = home.0.join("wrapper");
    let launcher = wrapper.join("bin").join("dimcode.mjs");
    fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    fs::copy(&source, &launcher).unwrap();
    fs::copy(
        source
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        wrapper.join("package.json"),
    )
    .unwrap();
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
        .arg(launcher)
        .arg("--no-tui")
        .arg("--home")
        .arg(&home.0)
        .env("DIMCODE_UPDATE_CHECK", "0");
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"hello\n");
    terminal.until("streaming");
    assert_eq!(
        unsafe { libc::kill(-(terminal.child.id() as i32), libc::SIGINT) },
        0
    );
    terminal.until("Waiting for resource settlement");
    terminal.until("\n> ");
    terminal.write(&[4]);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn fresh_native_startup_uses_workspace_and_ask_without_policy_file() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let output = home
        .command()
        .arg("--server")
        .arg(&binary)
        .arg("--model-config")
        .arg(&config)
        .args(["mcp", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = Wire::new(stream);
    wire.send(json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocolVersion":4,"clientKind":"cli"}}));
    let mut line = String::new();
    wire.0.read_line(&mut line).unwrap();
    let hello: Value = serde_json::from_str(&line).unwrap();
    assert!(hello.get("result").is_some(), "{hello}");
    wire.send(json!({"jsonrpc":"2.0","id":2,"method":"settings.read","params":{"source":"execution","scope":"user"}}));
    line.clear();
    wire.0.read_line(&mut line).unwrap();
    let snapshot: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        snapshot["result"]["document"],
        json!({"sandbox": false, "network": false}),
        "{snapshot}"
    );
    assert!(!home.0.join("rust/execution.json").exists());
    assert!(
        home.command()
            .arg("--server")
            .arg(&binary)
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
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
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
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
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tenabled\t—\n");
    assert_eq!(fs::read(project.join(".mcp.json")).unwrap(), written);
    assert!(run(&["daemon", "stop"]).status.success());
    assert_eq!(
        list(),
        "\"docs\"\tproject\tstdio\tenabled\t—\n",
        "restart lost the choice"
    );
    assert!(run(&["mcp", "disable", "docs"]).status.success());
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
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
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
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
    assert_eq!(list(), "\"docs\"\tproject\tstdio\tdisabled\t—\n");
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
        let body = json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]}).to_string();
        fs::write(&original, &body).unwrap();
        fs::write(&requested, format!("{body}\n")).unwrap();
        let socket = home.0.join("rust/daemon.sock");
        daemon_cleanup::mark_test_server(&home.0);
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
        let deadline = Instant::now() + PATIENCE;
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
    use base64::Engine as _;
    let binary = std::env::var("DIM_TEST_APP_SERVER")
        .expect("Set DIM_TEST_APP_SERVER to the dim-app-server binary");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let image = home.0.join("screenshot.dat");
    let bytes = image_fixture(image::ImageFormat::Png, 1, 1);
    fs::write(&image, &bytes).unwrap();
    let model_image = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let outside = Home::new();
    let reference = outside.0.join("outside reference.txt");
    fs::write(&reference, "outside-attachment-marker\n").unwrap();
    let model_reference = reference.clone();
    let paste = format!(
        "native-paste-reader-marker\r\n{}",
        "中 e\u{301} text\r\n".repeat(2000)
    );
    let model_paste = paste.clone();
    fs::write(home.0.join("marker.txt"), "production-read-marker\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let model = thread::spawn(move || {
        for phase in 0..7 {
            let deadline = Instant::now() + PATIENCE;
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
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
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
            let last_user = || {
                request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| {
                        message["role"] == "user"
                            && !message["content"].as_str().is_some_and(|text| {
                                text.starts_with("<dim-state ") && text.ends_with("</dim-state>")
                            })
                    })
                    .unwrap()
            };
            if phase == 2 {
                let user = last_user();
                assert_eq!(
                    user["content"],
                    json!([
                        {"type":"text","text":"continue"},
                        {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{model_image}")}}
                    ])
                );
            }
            assert!(
                request["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| { tool["function"]["name"] == "read" }),
                "the production read tool used by this flow is available"
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
            } else if phase == 3 {
                let user = last_user();
                let text = match &user["content"] {
                    Value::String(text) => text.as_str(),
                    Value::Array(parts) => {
                        assert_eq!(parts.len(), 1);
                        assert_eq!(parts[0]["type"], "text");
                        parts[0]["text"].as_str().unwrap()
                    }
                    other => panic!("Unexpected user content: {other}"),
                };
                let path = text
                    .strip_prefix("[Pasted text: ")
                    .unwrap()
                    .split_once(" (")
                    .unwrap()
                    .0;
                assert_eq!(fs::read(path).unwrap(), model_paste.as_bytes());
                frame(
                    json!({"tool_calls":[{"index":0,"id":"paste-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"path":path})).unwrap()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else if phase == 5 {
                let user = last_user();
                let parts = user["content"].as_array().unwrap();
                assert_eq!(parts[0], json!({"type":"text","text":"selected "}));
                assert_eq!(
                    parts[1],
                    json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{model_image}")}})
                );
                assert!(
                    user["content"]
                        .to_string()
                        .contains(model_reference.to_str().unwrap())
                );
                frame(
                    json!({"tool_calls":[{"index":0,"id":"selected-read","type":"function","function":{"name":"read","arguments":serde_json::to_string(&json!({"path":model_reference})).unwrap()}}]}),
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
                } else if phase == 2 {
                    let messages = request["messages"].to_string();
                    assert!(messages.contains("dim-shell-input"));
                    assert!(messages.contains("cli-recorded-marker"));
                    assert!(!messages.contains("cli-private-marker"));
                } else if phase == 4 {
                    assert!(
                        request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|message| {
                                message["role"] == "tool"
                                    && message["content"].as_str().is_some_and(|text| {
                                        text.contains("native-paste-reader-marker")
                                    })
                            }),
                        "The stored paste did not reach the model through read"
                    );
                } else {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains("outside-attachment-marker"))
                        )
                    );
                }
                frame(
                    json!({"content":match phase {1 => "read completed", 2 => "session continued", 4 => "paste read completed", _ => "files read completed"}}),
                    json!("stop"),
                )
            } + "data: [DONE]\n\n";
            write!(stream.get_mut(),"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config,serde_json::to_vec(&json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","apiKeyEnv":"DIM_CLI_TEST_API_KEY","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]})).unwrap()).unwrap();
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
    let mut terminal = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.until("Ctrl+J: newline");
    terminal.write(b"!printf cli-recorded-marker\n");
    terminal.until("[shell recorded] sequence:");
    terminal.until("\n> ");
    terminal.write(b"!!printf cli-private-marker\n");
    terminal.until("[shell completed] exit: 0");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
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
            "--image",
            image.to_str().unwrap(),
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
    let mut terminal = Terminal::launch(interactive);
    terminal.until("\n> ");
    terminal.until("Ctrl+J: newline");
    terminal.paste(&paste);
    terminal.until("[Paste 1: 2001 lines]");
    terminal.write(b"\n");
    terminal.until("paste read completed");
    terminal.editor();
    terminal.write(b"selected ");
    terminal.paste(image.to_str().unwrap());
    terminal.until("[Image 1: screenshot.dat]");
    terminal.paste(url::Url::from_file_path(&reference).unwrap().as_str());
    terminal.until("[File: outside reference.txt]");
    terminal.write(b"\n");
    // The referenced file is outside the Session workspace, so the read asks
    // (ADR 014 D2); the user answers the ordinary approval.
    terminal.until("Allow once? [y/N]");
    terminal.write(b"y\n");
    terminal.until("files read completed");
    terminal.editor();
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
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
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["user"]["content"][0]
                        == json!({"type":"text","text":"continue"})
                    && event["data"]["user"]["content"][1]["source"]["type"] == "file"
                    && event["data"]["user"]["content"][1]["source"]["fileId"]
                        .as_str()
                        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            })
    );
    assert!(
        conversation["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["user"]["content"][0]["text"]
                        .as_str()
                        .is_some_and(|text| {
                            text.starts_with("[Pasted text: ") && text.ends_with(" (2001 lines)]")
                        })
            })
    );
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
            .any(|event| {
                let content = &event["data"]["user"]["content"];
                event["type"] == "input"
                    && event["name"] == "message"
                    && content[0] == json!({"type":"text","text":"selected "})
                    && content[1]["source"]["type"] == "file"
                    && content[1]["source"]["fileId"]
                        .as_str()
                        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                    && content[2] == json!({"type":"text","text":reference.to_str().unwrap()})
            })
    );
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

/// Loopback model endpoint that answers each request with HTTP 400 so
/// `exec` fails immediately instead of sleeping through transport retries.
fn rejecting_model_config(home: &Home, requests: usize) -> (PathBuf, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let body = r#"{"error":{"message":"stub offline model"}}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        for index in 0..requests {
            let deadline = Instant::now() + PATIENCE;
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Rejecting model request {index} was not made"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            if length > 0 {
                let mut discard = vec![0; length];
                let _ = reader.read_exact(&mut discard);
            }
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    let config = home.0.join("model.json");
    fs::write(
        &config,
        json!({
            "defaultModel": { "providerId": "local", "modelId": "diagram-model" },
            "providers": [{
                "id": "local",
                "kind": "openai-compatible",
                "baseUrl": format!("http://{address}/v1"),
                "models": [{
                    "id": "diagram-model",
                    "contextWindow": 128000,
                    "maxOutputTokens": 4096,
                    "config": { "maxTokens": 4096 }
                }]
            }]
        })
        .to_string(),
    )
    .unwrap();
    (config, server)
}

/// `dimcode export` (Timeline export design §7, §13 item 6): the sibling
/// App Server's read-only export, live and after stop, without a daemon.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn export_writes_the_timeline_without_starting_a_daemon() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let _daemon = OwnedDaemon(socket.clone());
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .args(args)
            .current_dir(&home.0)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let (config, model) = rejecting_model_config(&home, 1);
    let exec = run(&[
        "--model-config",
        config.to_str().unwrap(),
        "exec",
        "-p",
        "hello",
    ]);
    model.join().unwrap();
    let stderr = String::from_utf8(exec.stderr).unwrap();
    let session = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap_or_else(|| panic!("{stderr}"))
        .to_owned();
    let lines = |text: &[u8]| -> Vec<Value> {
        std::str::from_utf8(text)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    let events = |lines: &[Value]| -> Vec<Value> {
        lines
            .iter()
            .filter(|line| line["kind"] == "event")
            .cloned()
            .collect()
    };

    let live = run(&["export", &session]);
    assert!(
        live.status.success(),
        "{}",
        String::from_utf8_lossy(&live.stderr)
    );
    assert!(live.stderr.is_empty());
    let exported = lines(&live.stdout);
    assert_eq!(exported[0]["kind"], "header");
    assert_eq!(exported[0]["session_id"], session.as_str());
    assert_eq!(exported[0]["sensitive"], true);
    assert_eq!(exported.last().unwrap()["kind"], "end");
    let output = home.0.join("timeline.jsonl");
    let written = run(&["export", &session, "--output", "timeline.jsonl"]);
    assert!(written.status.success());
    assert!(written.stdout.is_empty());
    assert!(String::from_utf8_lossy(&written.stderr).contains("Exported Timeline (sensitive): "));
    let written_bytes = fs::read(&output).unwrap();
    let written_lines = lines(&written_bytes);
    assert_eq!(written_lines[0], exported[0]);
    assert_eq!(written_lines.last().unwrap()["kind"], "end");
    let written_events = events(&written_lines);
    assert!(written_events.starts_with(&events(&exported)));
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let refused = run(&["export", &session, "--output", "timeline.jsonl"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(fs::read(&output).unwrap(), written_bytes);
    let failed = run(&["export", "missing-session", "--output", "failed.jsonl"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Session missing-session is not in"));
    assert!(!home.0.join("failed.jsonl").exists());

    assert!(run(&["daemon", "stop"]).status.success());
    assert!(!socket.exists());
    let stopped = run(&["export", &session, "--database", "rust/dimcode.sqlite"]);
    assert!(stopped.status.success());
    let stopped = lines(&stopped.stdout);
    assert_eq!(stopped[0], exported[0]);
    let (before, after) = (written_events, events(&stopped));
    assert!(!before.is_empty());
    assert_eq!(after[..before.len()], before[..]);
    assert!(!socket.exists());
}
