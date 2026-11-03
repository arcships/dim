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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("topic0000");
    terminal.write(b"\x1b[F");
    terminal.until("topic0049");
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
        wire.created();
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
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":4,"origin":"acp"},
                {"sessionId":"helper","cwd":"/workspace","title":"Inspect cache","preview":"agent preview",
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":3,"origin":"internal",
                 "parentSessionId":"chat"}
            ]}),
        );
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("[Chat] Update cache");
    terminal.until("[ACP ] Editor conversation");
    terminal.until("[Agent] Inspect cache");
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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(command);
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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(command);
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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(command);
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
    terminal.until("diagram-model");
    let ready = start.elapsed();
    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = BufReader::new(stream);
    let hello = rpc(
        &mut wire,
        "hello",
        json!({"protocolVersion":3,"clientKind":"cli"}),
    );
    assert!(
        hello["result"]["features"]
            .as_array()
            .unwrap()
            .contains(&json!("ui.leaf.2"))
    );
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
    let sent = Instant::now();
    terminal.write(b"cold first submission marker\r");
    terminal.until("native-cold-reply");
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

/// The scope key cycles this workspace, this repository and all workspaces,
/// and the repository read names the same relative directory in each worktree.
#[test]
fn tui_resume_cycles_through_the_repository_worktrees() {
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
        wire.created();
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
        // The set is read in a background task: reply to the page only after
        // the worktree read returns, so the set reaches the panel first.
        wait_for(&marker, "the repository read");
        wire.reply(
            &first,
            json!({"sessions":[{"sessionId":"mine","cwd":mine,"preview":"mine preview","model":{"providerId":"test","modelId":"model"},"updatedAt":5}]}),
        );
        let repository = wire.read("session.catalog");
        let mut got: Vec<String> = repository["params"]["cwd"]
            .as_array()
            .expect("a set of workspaces")
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        got.sort();
        let mut want = vec![mine.clone(), linked.clone()];
        want.sort();
        assert_eq!(got, want, "{repository}");
        wire.reply(
            &repository,
            json!({"sessions":[{"sessionId":"linked","cwd":linked,"preview":"linked preview","model":{"providerId":"test","modelId":"model"},"updatedAt":4}]}),
        );
        let all = wire.read("session.catalog");
        assert!(all["params"].get("cwd").is_none(), "{all}");
        wire.reply(&all, json!({"sessions":[]}));
        let back = wire.read("session.catalog");
        assert_eq!(back["params"]["cwd"], json!(mine), "{back}");
        wire.reply(&back, json!({"sessions":[]}));
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    command.env("PATH", &path);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("mine preview");
    terminal.write(b"\x13");
    terminal.until("Sessions · This repository");
    terminal.until("linked preview");
    terminal.until(&linked_shown);
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    terminal.write(b"\x13");
    terminal.until("Sessions · This workspace");
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
    let linked_shown = linked.clone();
    let mine = workspace.clone();
    let marker = scratch.path().join("worktree-read");
    let path = gated_git(scratch.path(), &marker);
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
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
        let first = wire.read("session.summaries");
        // The set is read in a background task: reply to the page only after
        // the worktree read returns, so the set reaches the panel first.
        wait_for(&marker, "the repository read");
        wire.reply(&first, rows.clone());
        let repository = wire.read("session.summaries");
        wire.reply(&repository, rows);
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    command.env("PATH", &path);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("mine preview");
    assert!(
        !terminal.transcript.contains("linked preview")
            && !terminal.transcript.contains("other preview"),
        "the workspace scope listed another workspace: {}",
        terminal.transcript
    );
    terminal.write(b"\x13");
    terminal.until("Sessions · This repository");
    terminal.until("linked preview");
    terminal.until(&linked_shown);
    assert!(
        !terminal.transcript.contains("other preview"),
        "the repository scope listed another workspace: {}",
        terminal.transcript
    );
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_resume_skips_the_repository_scope_without_a_second_worktree() {
    let home = Home::new();
    let (repo, _) = scratch_repository(&home.0, false);
    let workspace = std::fs::canonicalize(repo.join("sub")).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let mine = workspace.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.catalog.workspaces"]);
        wire.created();
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
        no_catalog(wire);
    });
    let mut command = home.command();
    command.args(["--cwd", &workspace]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("No saved sessions in this workspace. Ctrl+S shows more.");
    terminal.write(b"\x13");
    terminal.until("Sessions · All workspaces");
    assert!(
        !terminal.transcript.contains("Sessions · This repository"),
        "one worktree offers no repository scope: {}",
        terminal.transcript
    );
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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(command);
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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(command);
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
