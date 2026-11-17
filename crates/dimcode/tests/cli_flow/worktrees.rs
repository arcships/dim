use super::*;

#[test]
#[ignore = "exercises the real product daemon, Git and a PTY"]
fn managed_worktree_tui_uses_native_ownership_across_restart_and_cleanup() {
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let repo = home.0.join("repo");
    let source = repo.join("packages/a");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("seed.txt"), "seed").unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "--", "packages/a/seed.txt"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (written_tool, observed_write) = std::sync::mpsc::channel();
    let (finish_turn, released) = std::sync::mpsc::channel();
    let model = thread::spawn(move || {
        for phase in 0..2 {
            let mut ready = libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(
                unsafe { libc::poll(&mut ready, 1, PATIENCE.as_millis() as i32) },
                1,
                "model request {phase} was not made"
            );
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = None;
            loop {
                let mut line = String::new();
                assert_ne!(stream.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let mut bytes = vec![0; length.expect("the model request has Content-Length")];
            stream.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"worktree-response","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = if phase == 0 {
                assert!(
                    request["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|tool| tool["function"]["name"] == "write")
                );
                frame(
                    json!({"tool_calls":[{"index":0,"id":"managed-write","type":"function","function":{"name":"write","arguments":json!({"path":"managed.txt","content":"written by the real managed Session"}).to_string()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else {
                assert!(
                    request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| {
                            message["role"] == "tool" && message["tool_call_id"] == "managed-write"
                        }),
                    "the real write result must reach the model: {request}"
                );
                written_tool.send(()).unwrap();
                released.recv_timeout(PATIENCE).unwrap();
                frame(json!({"content":"managed write complete"}), json!("stop"))
            } + "data: [DONE]\n\n";
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
                data.len()
            )
            .unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let command = || {
        let mut command = home.command();
        command.arg("--cwd").arg(&source);
        command
    };
    let started = command().args(["mcp", "list"]).output().unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = Wire::new(stream);
    let mut id = 0;
    let mut call = |method: &str, params: Value| {
        id += 1;
        wire.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let mut line = String::new();
            assert_ne!(wire.0.read_line(&mut line).unwrap(), 0);
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                assert!(reply.get("result").is_some(), "{reply}");
                break reply["result"].clone();
            }
        }
    };
    let hello = call("hello", json!({"protocolVersion":4,"clientKind":"cli"}));
    assert_eq!(hello["daemonProfile"]["kind"], "product");
    assert!(
        hello["features"]
            .as_array()
            .unwrap()
            .contains(&json!("session.create.worktree"))
    );
    let settings = call(
        "settings.read",
        json!({"source":"session-title","scope":"user"}),
    );
    call(
        "settings.update",
        json!({"source":"session-title","scope":"user",
            "expectedRevision":settings["revision"],"document":{"enabled":false}}),
    );
    let provider = "018ec73f-0000-7000-8000-000000000096";
    call(
        "provider.create",
        json!({"providerId":provider,"displayName":"Worktree fixture",
            "connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"auth":{"type":"anonymous"}},
            "models":[{"modelId":"worktree-model","displayName":"Worktree model","status":"active",
                "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000,4096)}],"enabled":true}),
    );
    call(
        "provider.select",
        json!({"providerId":provider,"modelId":"worktree-model"}),
    );
    drop(wire);
    let mut tui = command();
    tui.args([
        "--permission",
        "workspace-write",
        "tui",
        "--worktree",
        "--scrollback",
        "-p",
        "write managed marker",
    ]);
    let mut terminal = Terminal::launch_tui(tui);
    terminal.until_plain("managed.txt");
    observed_write.recv_timeout(PATIENCE).unwrap();

    let run = |args: &[&str]| command().args(args).output().unwrap();
    let list = || {
        let output = run(&["worktree", "list", "--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let listed = list();
    let trees = listed["worktrees"].as_array().unwrap();
    assert_eq!(trees.len(), 1, "{listed}");
    let tree = &trees[0];
    let root = PathBuf::from(tree["root"].as_str().unwrap());
    let task = tree["taskId"].as_str().unwrap();
    let sessions = tree["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    let session = sessions[0].as_str().unwrap();
    assert!(root.starts_with(home.0.canonicalize().unwrap().join("worktrees")));
    let written = root.join("packages/a/managed.txt");
    assert_eq!(
        fs::read_to_string(&written).unwrap(),
        "written by the real managed Session"
    );
    assert!(!source.join("managed.txt").exists());
    assert_eq!(tree["untracked"], 1);
    let refused = run(&["worktree", "remove", task, "--force"]);
    assert!(!refused.status.success());
    assert!(written.exists());
    finish_turn.send(()).unwrap();
    terminal.until_plain("managed write complete");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success(), "{}", terminal.transcript);
    model.join().unwrap();
    let stopped = run(&["daemon", "stop"]);
    assert!(stopped.status.success());
    let restored = list();
    assert_eq!(restored, listed);
    let deleted = run(&["session", "delete", session]);
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    let unowned = list();
    assert_eq!(unowned["worktrees"][0]["sessions"], json!([]));
    let refused = run(&["worktree", "remove", task]);
    assert!(!refused.status.success());
    assert!(written.exists());
    fs::remove_file(&written).unwrap();
    let removed = run(&["worktree", "remove", task, "--json"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let removed: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed["removed"], true);
    assert!(!root.exists());
    assert_eq!(list()["worktrees"], json!([]));
    assert!(run(&["daemon", "stop"]).status.success());
}
