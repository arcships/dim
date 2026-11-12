use super::*;

fn rows(start: usize, end: usize) -> Value {
    (start..end).map(|index|json!({"sessionId":format!("session-{index}"),"cwd":"/workspace","preview":format!("topic{index:04}"),"model":{"providerId":"test","modelId":"model"},"updatedAt":10000-index})).collect()
}

#[test]
fn tui_resume_catalog_pages_searches_and_closes_without_waiting_for_a_read() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let (ready, waiting) = std::sync::mpsc::channel();
    let expected = workspace.clone();
    let daemon=home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.catalog.cancel"]);
        let attach=wire.read("session.attach");
        wire.reply(&attach,attach_record(&attach));
        let history=wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers=wire.read("account.providers");
        wire.reply(&providers,json!([]));
        let first=wire.read("session.catalog");
        assert_eq!(first["params"],json!({"limit":50,"query":"","withPreview":true,"cwd":expected}));
        wire.reply(&first,json!({"sessions":rows(0,50),"nextCursor":"second-page"}));
        let second=wire.read("session.catalog");
        assert_eq!(second["params"],json!({"limit":50,"query":"","withPreview":true,"cwd":expected,"cursor":"second-page"}));
        wire.reply(&second,json!({"sessions":rows(50,100)}));
        ready.send(()).unwrap();
        let query=wire.read("session.catalog");
        assert_eq!(query["params"],json!({"limit":50,"query":"ä title","withPreview":true,"cwd":expected}));
        wire.reply(&query,json!({"sessions":[{"sessionId":"off-page","cwd":"/workspace","title":"off-page result","preview":"different preview","model":{"providerId":"test","modelId":"model"},"updatedAt":2}]}));
        let cleared=wire.read("session.catalog");
        assert_eq!(cleared["params"]["query"],"");
        wire.reply(&cleared,json!({"sessions":rows(0,50)}));
        let held=wire.read("session.catalog");
        ready.send(()).unwrap();
        let cancelled=wire.read("session.catalog.cancel");
        assert_eq!(cancelled["params"], json!({"requestId":held["id"]}));
        wire.reply(&cancelled,json!({}));
        let usage=wire.read("session.context.usage");
        wire.send(json!({"jsonrpc":"2.0","id":usage["id"],"error":{"code":-32601,"message":"unsupported","data":{"appError":"unknown_method"}}}));
        wire.reply(&held,json!({"sessions":[{"sessionId":"obsolete","cwd":"/workspace","preview":"STALE CATALOG ROW","model":{"providerId":"test","modelId":"model"},"updatedAt":1}]}));
        let reopened=wire.read("session.catalog");
        wire.reply(&reopened,json!({"sessions":[{"sessionId":"fresh","cwd":"/workspace","preview":"fresh opening","model":{"providerId":"test","modelId":"model"},"updatedAt":3}]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("topic0000");
    terminal.write(b"\x1b[F");
    terminal.until("topic0049");
    waiting.recv_timeout(PATIENCE).unwrap();
    terminal.write("ä title".as_bytes());
    terminal.until("off-page result");
    terminal.write(b"\x1b[27u");
    terminal.until("topic0000");
    terminal.write(b"\x12");
    waiting.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b[27u");
    terminal.write(b"/usage\r");
    terminal.until("This daemon does not track context usage");
    terminal.write(b"/resume\r");
    terminal.until("fresh opening");
    assert!(!terminal.transcript.contains("STALE CATALOG ROW"));
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// #557: the picker labels every row by its creation origin and asks for ACP
/// Sessions only when the saved preference says so.
#[test]
fn tui_resume_labels_origins_and_shows_acp_sessions_when_asked() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    fs::write(
        home.0.join("config.json"),
        r#"{"locale":"en","showAcpSessions":true}"#,
    )
    .unwrap();
    let expected = workspace.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.catalog",
            "session.catalog.cancel",
            "session.catalog.origins",
        ]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        assert_eq!(
            first["params"],
            json!({"limit":50,"query":"","withPreview":true,"withAcpSessions":true,"cwd":expected})
        );
        wire.reply(
            &first,
            json!({"sessions":[
                {"sessionId":"chat","cwd":"/workspace","title":"Update cache","preview":"chat preview",
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":5,"origin":"client"},
                {"sessionId":"editor","cwd":"/workspace","title":"Editor conversation","preview":"acp preview",
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":4,"origin":"acp"}
            ]}),
        );
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("[Chat] Update cache");
    terminal.until("[ACP ] Editor conversation");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Ctrl+S widens the picker like a new query: every workspace, with each row
/// naming the one it lives in.
#[test]
fn tui_resume_ctrl_s_widens_to_every_workspace_and_names_the_workspace_of_a_row() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let mine = workspace.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        assert_eq!(first["params"]["cwd"], json!(mine), "{first}");
        wire.reply(
            &first,
            json!({"sessions":[{"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5}]}),
        );
        let widened = wire.read("session.catalog");
        assert!(
            widened["params"].get("cwd").is_none(),
            "the widened read names no workspace: {widened}"
        );
        wire.reply(
            &widened,
            json!({"sessions":[
                {"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5},
                {"sessionId":"other","cwd":"/elsewhere/project","title":"Other project session","preview":"other preview","model":{"providerId":"test","modelId":"model"},"updatedAt":4}
            ]}),
        );
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("mine preview");
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    terminal.until("Other project session");
    terminal.until("/elsewhere/project");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_resume_says_when_the_workspace_has_no_saved_sessions() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, json!({"sessions":[]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("No saved sessions in this workspace. Ctrl+S shows more.");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A daemon before `session.catalog` is listed through `session.summaries`,
/// and the client applies the workspace scope to the rows it receives.
#[test]
fn tui_resume_hides_other_workspaces_on_the_summaries_fallback() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let mine = workspace.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let summaries = wire.read("session.summaries");
        let rows = json!([
            {"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5},
            {"sessionId":"other","cwd":"/elsewhere/project","title":"Other workspace session","preview":"other preview","model":{"providerId":"test","modelId":"model"},"updatedAt":4}
        ]);
        wire.reply(&summaries, rows.clone());
        let widened = wire.read("session.summaries");
        wire.reply(&widened, rows);
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("mine preview");
    assert!(
        !terminal.transcript.contains("Other workspace session"),
        "a Session of another workspace was listed: {}",
        terminal.transcript
    );
    terminal.write(b"\x13");
    terminal.until("Other workspace session");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn a_cold_ui_leaf_submits_its_first_message_with_a_large_native_catalog() {
    fn rpc(wire: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        wire.get_mut().flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                wire.read_line(&mut line).unwrap() > 0,
                "{method}: connection closed"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == 1 {
                return value;
            }
        }
    }
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let source = Home::new();
    super::migration::legacy_home(
        &source.0,
        "dimcode.sqlite",
        &"large imported context ".repeat(5000),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let model = home.0.join("model.json");
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../dim-agent-aimux/examples/offline.json"
    ))
    .unwrap();
    config["providers"][0]["baseUrl"] = format!("http://{address}/v1").into();
    fs::write(&model, config.to_string()).unwrap();
    let command = || {
        let mut command = home.command();
        command.args([
            "--server",
            &binary,
            "--model-config",
            model.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
        ]);
        command
    };
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let imported = command()
        .args([
            "migration",
            "run",
            "--source",
            source.0.to_str().unwrap(),
            "--source-stopped",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert!(
        command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut database = rusqlite::Connection::open(home.0.join("rust/dimcode.sqlite")).unwrap();
    let seed: String = database
        .query_row("SELECT session_id FROM agent_sessions LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let bytes: i64 = database
        .query_row(
            "SELECT max(length(body)) FROM timeline_events WHERE kind='context/compaction'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        bytes > 64 * 1024,
        "the imported fixture has a large compaction: {bytes}"
    );
    let tx = database.transaction().unwrap();
    for index in 0..1000 {
        let id = format!("cold-catalog-{index:04}");
        let params = rusqlite::params![id, seed];
        for sql in [
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id) SELECT ?1, config, created_at_ms, leader_session_id FROM agent_sessions WHERE session_id=?2",
            "INSERT INTO session_owners SELECT ?1,agent_id,protocol_version FROM session_owners WHERE session_id=?2",
            "INSERT INTO timeline_branches SELECT ?1,branch_id,info,sequence FROM timeline_branches WHERE session_id=?2",
            "INSERT INTO timeline_content SELECT ?1,hash,kind,body FROM timeline_content WHERE session_id=?2",
            "INSERT INTO timeline_events SELECT ?1,branch_id,sequence,last_sequence,kind,body,event_id,context_render,context_state FROM timeline_events WHERE session_id=?2",
            "INSERT INTO timeline_snapshots SELECT ?1,branch_id,sequence,interpreter,event_id,state FROM timeline_snapshots WHERE session_id=?2",
            "INSERT INTO session_metadata SELECT ?1,revision,title,title_source FROM session_metadata WHERE session_id=?2",
            "INSERT INTO session_catalog SELECT ?1,updated_at_ms,preview,cwd FROM session_catalog WHERE session_id=?2",
        ] {
            tx.execute(sql, params).unwrap();
        }
    }
    tx.commit().unwrap();
    drop(database);
    let http = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut wire = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            wire.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut body = vec![0; length];
        wire.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("cold first submission marker"), "{body}");
        assert!(
            !body.contains("large imported context"),
            "another Session's history entered the new request"
        );
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"id":"cold-flow","choices":[{"index":0,"delta":{"content":"native-cold-reply"},"finish_reason":null}]}),
            json!({"id":"cold-flow","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
        write!(wire.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        wire.get_mut().flush().unwrap();
    });
    let start = Instant::now();
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("Ask anything...");
    let ready = start.elapsed();
    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
    let mut wire = BufReader::new(stream);
    let hello = rpc(
        &mut wire,
        "hello",
        json!({"protocolVersion":4,"clientKind":"cli"}),
    );
    assert!(
        hello["result"]["features"]
            .as_array()
            .unwrap()
            .contains(&json!("ui.leaf.2"))
    );
    let status = rpc(&mut wire, "ui.status", json!({}));
    assert_eq!(status["result"]["instances"], json!([]), "{status}");
    let sent = Instant::now();
    terminal.write(b"cold first submission marker\r");
    terminal.until("native-cold-reply");
    let status = rpc(&mut wire, "ui.status", json!({}));
    assert_eq!(
        status["result"]["instances"].as_array().unwrap().len(),
        1,
        "{status}"
    );
    assert!(
        status["result"]["instances"][0]["uiId"]
            .as_str()
            .unwrap()
            .starts_with("ui-"),
        "the terminal uses the ui.leaf.2 path: {status}"
    );
    eprintln!(
        "1001 dormant Sessions, compaction row {bytes} bytes: cold UI {ready:?}, first reply {:?}",
        sent.elapsed()
    );
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    http.join().unwrap();
}

/// A scratch git repository under `parent`, holding `sub/`, and when
/// `worktrees` is set a linked worktree of it that also holds `sub/`.
fn scratch_repository(
    parent: &std::path::Path,
    worktrees: bool,
) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
    let repo = parent.join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Initial",
        ],
    );
    if !worktrees {
        return (repo, None);
    }
    let linked = parent.join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(linked.join("sub")).unwrap();
    (repo, Some(linked))
}

/// A short scratch directory for one flow, removed with everything the flow
/// created under it when the test ends, as `Home` removes its own.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::path::PathBuf::from("/tmp")
            .join(format!("dim-r-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        daemon_cleanup::remove_directory(&self.0);
    }
}

/// A `git` that runs the real one, and after a `worktree` call writes
/// `marker`, so the daemon replies to the first page only once the repository
/// set has been read. Returns the PATH value that puts it first.
fn gated_git(directory: &std::path::Path, marker: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let git = directory.join("git");
    let real = real_git();
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = worktree ]; then\n    {real} \"$@\"\n    status=$?\n    : > {}\n    exit $status\n  fi\ndone\nexec {real} \"$@\"\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        directory.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Waits until `path` exists, with the harness's patience.
fn wait_for(path: &std::path::Path, what: &str) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The real git, for a script that only slows some invocations.
#[cfg(unix)]
fn real_git() -> String {
    if std::path::Path::new("/usr/bin/git").exists() {
        return "/usr/bin/git".into();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("git");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "git".into()
}

/// Ctrl+S toggles between the local scope and all workspaces, two states; the
/// local one is the repository git detected, naming the same relative
/// directory in each worktree, and it returns as itself.
#[test]
fn tui_resume_toggles_between_the_repository_and_all_workspaces() {
    let home = Home::new();
    let scratch = Scratch::new();
    let (repo, linked) = scratch_repository(scratch.path(), true);
    let workspace = std::fs::canonicalize(repo.join("sub")).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let linked = std::fs::canonicalize(linked.unwrap().join("sub")).unwrap();
    let linked = linked.to_str().unwrap().to_owned();
    let linked_shown = linked.clone();
    let mine = workspace.clone();
    let marker = scratch.path().join("worktree-read");
    let path = gated_git(scratch.path(), &marker);
    let git = scratch.path().join("git");
    let script = std::fs::read_to_string(&git).unwrap();
    assert!(script.contains("    status=$?\n"));
    std::fs::write(
        &git,
        script.replace(
            "    status=$?\n",
            "    status=$?\n    printf 'worktree /tmp/dim-invalid-\\377\\000\\000'\n",
        ),
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.catalog.workspaces"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        assert_eq!(first["params"]["cwd"], json!(mine), "{first}");
        // The set is read in a background task: answer only after the
        // worktree read returns, so the set reaches the panel first. The
        // panel adopts it and reads the first page again, dropping this
        // page's reply.
        wait_for(&marker, "the repository read");
        let repository = wire.read("session.catalog");
        let mut want = vec![mine.clone(), linked.clone()];
        want.sort();
        let mut got: Vec<String> = repository["params"]["cwd"]
            .as_array()
            .expect("a set of workspaces")
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        got.sort();
        assert_eq!(got, want, "{repository}");
        wire.reply(
            &repository,
            json!({"sessions":[
                {"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5},
                {"sessionId":"linked","cwd":linked,"preview":"linked preview","model":{"providerId":"test","modelId":"model"},"updatedAt":4}
            ]}),
        );
        let all = wire.read("session.catalog");
        assert!(all["params"].get("cwd").is_none(), "{all}");
        wire.reply(&all, json!({"sessions":[]}));
        let back = wire.read("session.catalog");
        let mut got: Vec<String> = back["params"]["cwd"]
            .as_array()
            .expect("the local scope returns as the set")
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        got.sort();
        assert_eq!(got, want, "{back}");
        wire.reply(&back, json!({"sessions":[]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    command.env("PATH", &path);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Sessions · This repository");
    terminal.until("mine preview");
    terminal.until("linked preview");
    terminal.until(&linked_shown);
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    terminal.write(b"\x13");
    terminal.until("Sessions · This repository");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// The summaries fallback applies the repository scope to the rows it
/// receives: the launch workspace and the same directory in each linked
/// worktree, not another workspace.
#[test]
fn tui_resume_keeps_the_repository_rows_on_the_summaries_fallback() {
    let home = Home::new();
    let scratch = Scratch::new();
    let (repo, linked) = scratch_repository(scratch.path(), true);
    let workspace = std::fs::canonicalize(repo.join("sub")).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let linked = std::fs::canonicalize(linked.unwrap().join("sub")).unwrap();
    let linked = linked.to_str().unwrap().to_owned();
    let mine = workspace.clone();
    let marker = scratch.path().join("worktree-read");
    let path = gated_git(scratch.path(), &marker);
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let rows = json!([
            {"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5},
            {"sessionId":"linked","cwd":linked,"preview":"linked preview","model":{"providerId":"test","modelId":"model"},"updatedAt":4},
            {"sessionId":"other","cwd":"/elsewhere/project","preview":"other preview","model":{"providerId":"test","modelId":"model"},"updatedAt":3}
        ]);
        let _ = wire.read("session.summaries");
        // The set is read in a background task: answer only after the
        // worktree read returns, so the set reaches the panel first. The
        // panel adopts it and reads the first page again, dropping this
        // one.
        wait_for(&marker, "the repository read");
        let repository = wire.read("session.summaries");
        wire.reply(&repository, rows);
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    command.env("PATH", &path);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Sessions · This repository");
    terminal.until("mine preview");
    terminal.until("linked preview");
    assert!(
        !terminal.transcript.contains("other preview"),
        "the local scope listed another workspace: {}",
        terminal.transcript
    );
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// One worktree still names the local scope the repository; it changes only
/// the title, and the cycle keeps its two states.
#[test]
fn tui_resume_names_a_one_worktree_repository() {
    let home = Home::new();
    let (repo, _) = scratch_repository(&home.0, false);
    let workspace = std::fs::canonicalize(repo.join("sub")).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let mine = workspace.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.catalog.workspaces"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        assert_eq!(first["params"]["cwd"], json!(mine));
        wire.reply(&first, json!({"sessions":[]}));
        let all = wire.read("session.catalog");
        assert!(all["params"].get("cwd").is_none(), "{all}");
        wire.reply(&all, json!({"sessions":[]}));
        // The local stop returns as the single-entry set; the catalog's
        // canonical cwd treats it as the same filter.
        let back = wire.read("session.catalog");
        assert_eq!(back["params"]["cwd"], json!([mine]), "{back}");
        wire.reply(&back, json!({"sessions":[]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("No saved sessions in this workspace. Ctrl+S shows more.");
    terminal.until_anywhere("Sessions · This repository");
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    terminal.write(b"\x13");
    terminal.until("Sessions · This repository");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A git that hangs cannot hold the first page: the picker draws the
/// workspace at once, and the scope key widens the list while the read runs.
#[cfg(unix)]
#[test]
fn tui_resume_draws_the_workspace_while_a_slow_git_runs() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let fake = Scratch::new();
    let git = fake.path().join("git");
    let real = real_git();
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    --show-toplevel|worktree) exec sleep 30 ;;\n  esac\ndone\nexec {real} \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mine = workspace.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.catalog.workspaces"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        assert_eq!(first["params"]["cwd"], json!(mine), "{first}");
        wire.reply(
            &first,
            json!({"sessions":[{"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5}]}),
        );
        let all = wire.read("session.catalog");
        assert!(all["params"].get("cwd").is_none(), "{all}");
        wire.reply(&all, json!({"sessions":[]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    command.env(
        "PATH",
        format!(
            "{}:{}",
            fake.path().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("mine preview");
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_resume_skips_the_repository_scope_a_daemon_cannot_read() {
    let home = Home::new();
    let (repo, linked) = scratch_repository(&home.0, true);
    let workspace = std::fs::canonicalize(repo.join("sub")).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let mine = workspace.clone();
    assert!(linked.is_some(), "the worktree exists");
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        assert_eq!(first["params"]["cwd"], json!(mine));
        wire.reply(&first, json!({"sessions":[]}));
        let all = wire.read("session.catalog");
        assert!(
            all["params"].get("cwd").is_none(),
            "a daemon without the set is asked for every workspace: {all}"
        );
        wire.reply(&all, json!({"sessions":[]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("No saved sessions in this workspace. Ctrl+S shows more.");
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}
/// Two stored Sessions the picker lists, and the launch workspace.
fn two_sessions() -> Value {
    json!({"sessions":[
        {"sessionId":"session","cwd":"/workspace","title":"Shown session","preview":"shown preview","model":{"providerId":"test","modelId":"model"},"updatedAt":9},
        {"sessionId":"other","cwd":"/workspace","title":"Other session","preview":"other preview","model":{"providerId":"test","modelId":"model"},"updatedAt":8}
    ]})
}

/// Ctrl+X deletes the selected row: the row leaves on the reply, the picker
/// stays open, and nothing is detached.
#[test]
fn tui_resume_ctrl_x_deletes_the_selected_row_and_keeps_the_shown_session() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.delete"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, two_sessions());
        let delete = wire.read("session.delete");
        assert_eq!(delete["params"], json!({"sessionId":"other"}));
        wire.reply(&delete, json!({"deleted": true}));
        // The row leaves on the reply, not on a second catalog read.
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(
                !line.contains("session.catalog") && !line.contains("session.summaries"),
                "the delete must remove the row without reading the catalog again: {line}"
            );
            line.clear();
        }
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Shown session");
    terminal.write(b"\x1b[B"); // select the second row
    terminal.until("Other session");
    terminal.write(b"\x18"); // Ctrl+X
    terminal.until("\"Other session\"? Enter confirm");
    terminal.write(b"\r");
    terminal.until("Deleted Other session.");
    // The picker stays open, the other row remains, and no detach was sent.
    terminal.until("Shown session");
    assert!(!terminal.transcript.contains("session.detach"));
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A delete can wait on a slow close, so Esc leaves the picker while the
/// reply is outstanding and the outcome is still reported in the transcript
/// afterwards.
#[test]
fn tui_resume_esc_leaves_the_picker_while_the_delete_is_in_flight() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let (deleting, delete_started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.delete"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, two_sessions());
        let delete = wire.read("session.delete");
        assert_eq!(delete["params"], json!({"sessionId":"other"}));
        deleting.send(()).unwrap();
        released.recv_timeout(PATIENCE).unwrap();
        wire.reply(&delete, json!({"deleted": true}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Shown session");
    terminal.write(b"\x1b[B");
    terminal.until("Other session");
    terminal.write(b"\x18");
    terminal.until("\"Other session\"? Enter confirm");
    terminal.write(b"\r");
    delete_started.recv_timeout(PATIENCE).unwrap();
    // Esc leaves while the delete is unanswered, even though no other key acts.
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    release.send(()).unwrap();
    // Closed before the reply: the outcome still reaches the transcript.
    terminal.until("Deleted Other session.");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Deleting the shown Session leaves for Home on the reply: the daemon sends
/// `session.state` and `session.closed` before `{deleted:true}` (the real
/// order), and the queued notices then match no Session and are ignored. The
/// draft typed before the delete is gone, as after `/new`.
#[test]
fn tui_resume_deletes_the_shown_session_and_returns_home() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let workspace = std::fs::canonicalize(&home.0).unwrap();
        let workspace = workspace.to_str().unwrap().to_owned();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.catalog", "session.delete", "session.create.modelFromSession"]);
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let first = wire.read("session.catalog");
            wire.reply(&first, two_sessions());
            let delete = wire.read("session.delete");
            assert_eq!(delete["params"], json!({"sessionId":"session"}));
            // The real order: the close notices, then the reply.
            wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":false}));
            wire.notice("session.closed", json!({"sessionId":"session"}));
            wire.reply(&delete, json!({"deleted": true}));
            // Home's first message creates a new Session.
            let create = wire.read("session.create");
            assert!(
                create["params"]["sessionId"].is_null(),
                "the next message must create a Session, not submit to the deleted one: {create}"
            );
            wire.reply(&create, json!({"sessionId":"fresh","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            wire.no_context();
            wire.hold();
        });
        let mut command = home.command();
        command.args(["--cwd", &workspace]);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        // A draft typed before the delete is gone afterwards, as after /new.
        terminal.write(b"draft before the delete");
        terminal.write(b"\x0f"); // Ctrl+O stashes the draft and opens the palette
        terminal.write(b"resume\r");
        terminal.until("Shown session");
        terminal.write(b"\x18");
        terminal.until("\"Shown session\"? Enter confirm");
        terminal.write(b"\r");
        terminal.until("─ new session ─");
        terminal.until("Deleted Shown session.");
        // The draft typed before the delete is gone, as after /new: the
        // frames drawn since Home do not carry it.
        let home_at = terminal.transcript.rfind("─ new session ─").unwrap();
        assert!(
            !terminal.transcript[home_at..].contains("draft before the delete"),
            "the draft must be cleared with the switch: {}",
            &terminal.transcript[home_at..]
        );
        assert!(
            !terminal
                .transcript
                .contains("Session closed while attached"),
            "{}",
            terminal.transcript
        );
        assert!(
            !terminal.transcript.contains("Disconnected"),
            "{}",
            terminal.transcript
        );
        assert!(
            !terminal.transcript.contains("Error:"),
            "{}",
            terminal.transcript
        );
        // The daemon asserts the next message creates a Session; send one.
        terminal.write(b"hello\r");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A daemon without `session.delete`: the picker says so and sends nothing.
#[test]
fn tui_resume_delete_keeps_the_row_when_the_daemon_cannot_delete() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, two_sessions());
        wire.hold();
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Shown session");
    terminal.write(b"\x18");
    terminal.until("\"Shown session\"? Enter confirm");
    terminal.write(b"\r");
    terminal.until("This daemon does not support deleting Sessions");
    terminal.until("Shown session");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A failed delete keeps the row; a `{deleted:false}` reply re-reads the
/// first page and reports that nothing was removed.
#[test]
fn tui_resume_delete_keeps_the_row_on_a_failure_and_refreshes_when_nothing_removed() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.delete"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, two_sessions());
        let delete = wire.read("session.delete");
        wire.send(json!({"jsonrpc":"2.0","id":delete["id"],"error":{"code":-32000,"message":"fixture storage_failed"}}));
        let delete = wire.read("session.delete");
        wire.reply(&delete, json!({"deleted": false}));
        // Nothing-removed re-reads the first page.
        let again = wire.read("session.catalog");
        wire.reply(&again, two_sessions());
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Shown session");
    terminal.write(b"\x18");
    terminal.until("\"Shown session\"? Enter confirm");
    terminal.write(b"\r");
    terminal.until("Delete failed · fixture storage_failed");
    terminal.until("Shown session");
    terminal.write(b"\x18");
    terminal.until("\"Shown session\"? Enter confirm");
    terminal.write(b"\r");
    terminal.until("Nothing was removed: no stored Session Shown session.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A shown Session's delete answering `-32603` lands Home: the close already
/// ran, and the queued `session.closed` must not produce a disconnect report.
#[test]
fn tui_resume_delete_after_the_close_lands_home() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.delete"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, two_sessions());
        let delete = wire.read("session.delete");
        assert_eq!(delete["params"], json!({"sessionId":"session"}));
        wire.notice("session.state", json!({"sessionId":"session","branchId":"main","running":false}));
        wire.notice("session.closed", json!({"sessionId":"session"}));
        wire.send(json!({"jsonrpc":"2.0","id":delete["id"],"error":{"code":-32603,"message":"Session operation failed"}}));
        wire.hold();
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Shown session");
    terminal.write(b"\x18");
    terminal.until("\"Shown session\"? Enter confirm");
    terminal.write(b"\r");
    terminal.until("─ new session ─");
    terminal.until("Delete failed · Session operation failed");
    assert!(
        !terminal
            .transcript
            .contains("Session closed while attached"),
        "{}",
        terminal.transcript
    );
    assert!(
        !terminal.transcript.contains("Disconnected"),
        "{}",
        terminal.transcript
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// The picker's hint is drawn translated in a non-English locale.
#[test]
fn the_resume_hint_is_drawn_translated() {
    let home = Home::new();
    fs::write(home.0.join("config.json"), r#"{"locale":"zh-CN"}"#).unwrap();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.catalog");
        wire.reply(&first, two_sessions());
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("Ctrl+X");
    let plain = strip_ansi(&terminal.transcript);
    assert!(
        plain.contains("Enter 打开 · Ctrl+X 删除 · Ctrl+S 切换范围 · Ctrl+R 刷新 · Esc 关闭"),
        "the hint must be drawn translated: {plain}"
    );
    assert!(!plain.contains("Enter open · Ctrl+S scope"));
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}
