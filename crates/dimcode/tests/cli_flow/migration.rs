use super::*;

pub(super) fn legacy_home(home: &std::path::Path, database: &str, content: &str) {
    fs::create_dir_all(home).unwrap();
    let db = rusqlite::Connection::open(home.join(database)).unwrap();
    db.execute_batch("CREATE TABLE sessions(sessionId TEXT PRIMARY KEY,cwd TEXT,title TEXT,status TEXT,tags TEXT,createdAt TEXT,updatedAt TEXT,version INTEGER);
        CREATE TABLE session_states(sessionId TEXT PRIMARY KEY,selectedProviderId TEXT,selectedModelId TEXT,settings TEXT,permissionSettings TEXT,currentMode TEXT);
        CREATE TABLE messages(messageId TEXT PRIMARY KEY,sessionId TEXT,role TEXT,parts TEXT,attachments TEXT,metadata TEXT,orderKey TEXT,createdAt TEXT);
        CREATE TABLE compaction_states(sessionId TEXT PRIMARY KEY,cursor TEXT,compactionSegments TEXT);").unwrap();
    db.execute("INSERT INTO sessions VALUES ('old',?1,'Legacy session','active','[]','2026-01-01','2026-01-01',1)",[home.canonicalize().unwrap().to_str().unwrap()]).unwrap();
    db.execute(
        "INSERT INTO session_states VALUES ('old','local','diagram-model','{}','{}','agent')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO messages VALUES ('old-message','old','user',?1,'[]','{}','a','2026-01-01T00:00:00Z')",[json!([{ "type":"text", "text":content }]).to_string()]).unwrap();
    drop(db);
}

struct Daemon<'a>(&'a Home, &'a std::path::Path);
impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .command()
            .arg("--model-config")
            .arg(self.1)
            .args(["daemon", "stop"])
            .output();
    }
}

#[test]
fn migration_commands_load_on_demand_and_reconcile_retries_without_a_live_plugin() {
    let home = Home::new();
    let source = Home::new();
    let model = home.0.join("mock-model.json");
    fs::write(
        &model,
        include_str!("../../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    legacy_home(&source.0, "dimcode.sqlite", "legacy user message");
    let legacy = rusqlite::Connection::open(source.0.join("dimcode.sqlite")).unwrap();
    legacy.execute_batch("INSERT INTO sessions SELECT 'missing-model',cwd,'Removed model',status,tags,createdAt,updatedAt,version FROM sessions WHERE sessionId='old';
        INSERT INTO session_states VALUES ('missing-model','removed-provider','removed-model','{}','{}','agent');
        INSERT INTO messages SELECT 'missing-message','missing-model',role,parts,attachments,metadata,orderKey,createdAt FROM messages WHERE messageId='old-message';").unwrap();
    drop(legacy);
    fs::write(source.0.join("permissions.json"),json!({"version":1,"execCommandPermissions":{"rules":{"cargo test *":"allow","git push *":"deny"}}}).to_string()).unwrap();
    let before = fs::read(source.0.join("dimcode.sqlite")).unwrap();
    for args in [
        vec!["migration", "--help"],
        vec!["migration", "run", "--help"],
        vec!["migration", "retry", "--help"],
    ] {
        let output = home.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!home.0.join("migration").exists());
    assert!(!home.0.join("rust/daemon.sock").exists());
    let overlapping = source
        .command()
        .args([
            "migration",
            "run",
            "--source",
            source.0.to_str().unwrap(),
            "--source-stopped",
        ])
        .output()
        .unwrap();
    assert_eq!(overlapping.status.code(), Some(1));
    assert!(!source.0.join("migration").exists());
    assert!(!source.0.join("rust/daemon.sock").exists());
    let stopped = home
        .command()
        .args(["migration", "run", "--source", source.0.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(1));
    assert!(!home.0.join("migration").exists());
    let inspected = home
        .command()
        .args([
            "migration",
            "inspect",
            "--source",
            source.0.to_str().unwrap(),
            "--source-stopped",
        ])
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let inventory: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inventory["tables"]["messages"], 2);
    assert!(!home.0.join("rust/daemon.sock").exists());
    let run = |args: &[&str]| {
        home.command()
            .args(["--model-config", model.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap()
    };
    let _daemon = Daemon(&home, &model);
    let first = run(&[
        "migration",
        "run",
        "--source",
        source.0.to_str().unwrap(),
        "--source-stopped",
        "--json",
    ]);
    assert!(
        first.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr),
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let task = first["task"]["id"].as_str().unwrap();
    let session = first["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "session" && item["id"] == "old")
        .unwrap()["targetId"]
        .as_str()
        .unwrap();
    let missing = first["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "session" && item["id"] == "missing-model")
        .unwrap();
    assert_eq!(missing["outcome"], "imported");
    let native = rusqlite::Connection::open_with_flags(
        home.0.join("rust/dimcode.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let configuration: String = native
        .query_row(
            "SELECT config FROM agent_sessions WHERE session_id=?1",
            [missing["targetId"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let configuration: Value = serde_json::from_str(&configuration).unwrap();
    assert_eq!(configuration["model"]["providerId"], "removed-provider");
    assert_eq!(configuration["model"]["modelId"], "removed-model");
    drop(native);
    let exported = run(&["session", "export", session, "--format", "json"]);
    assert!(
        exported.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&exported.stderr),
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
    assert!(String::from_utf8_lossy(&exported.stdout).contains("legacy user message"));
    assert!(run(&["daemon", "stop"]).status.success());
    let read_rules = |home: &Home| {
        let native = rusqlite::Connection::open_with_flags(
            home.0.join("rust/dimcode.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let rules: String = native
            .query_row(
                "SELECT document FROM settings_documents WHERE source='user/permission'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<Value>(&rules).unwrap()["commandRules"].clone()
    };
    assert_eq!(
        read_rules(&home),
        json!([
            {"command":"cargo test","match":"prefix","decision":"allow"},{"command":"git push","match":"prefix","decision":"deny"}
        ])
    );
    let shown = home
        .command()
        .args(["migration", "show", task])
        .output()
        .unwrap();
    assert!(shown.status.success());
    assert!(!home.0.join("rust/daemon.sock").exists());
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout).unwrap(),
        first
    );
    let retried = run(&["migration", "retry", task, "--source-stopped", "--json"]);
    assert!(
        retried.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&retried.stdout),
        String::from_utf8_lossy(&retried.stderr)
    );
    let retried: Value = serde_json::from_slice(&retried.stdout).unwrap();
    assert_eq!(retried["task"]["retryOf"], task);
    assert_ne!(retried["task"]["id"], task);
    assert!(
        retried["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "session"
                && item["outcome"] == "alreadyImported"
                && item["targetId"] == session)
    );
    assert!(run(&["daemon", "stop"]).status.success());
    let cancelled = home
        .command()
        .args(["migration", "cancel", task])
        .output()
        .unwrap();
    assert!(cancelled.status.success());
    let unaccepted = home
        .0
        .join("migration/tasks/018ec73f-0000-7000-8000-000000000099/results");
    fs::create_dir_all(&unaccepted).unwrap();
    let listed = home.command().args(["migration", "list"]).output().unwrap();
    assert!(listed.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fs::write(unaccepted.join("orphan.json"), b"{}").unwrap();
    let damaged = home.command().args(["migration", "list"]).output().unwrap();
    assert_eq!(
        damaged.status.code(),
        Some(1),
        "Results without state must remain a visible integrity error"
    );
    fs::remove_dir_all(unaccepted.parent().unwrap()).unwrap();
    fs::write(
        source.0.join("permissions.json"),
        json!({"version":1,"execCommandPermissions":{"rules":{
            "git *":"allow","git push *":"deny","git push --dry-run *":"allow",
            "npm *":"allow","npm publish":"ask","cargo test *":"allow","cargo test --all *":"deny"
        }}})
        .to_string(),
    )
    .unwrap();
    let fresh = Home::new();
    let _fresh_daemon = Daemon(&fresh, &model);
    let partial = fresh
        .command()
        .arg("--model-config")
        .arg(&model)
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
    assert_eq!(
        partial.status.code(),
        Some(2),
        "{}\n{}",
        String::from_utf8_lossy(&partial.stdout),
        String::from_utf8_lossy(&partial.stderr)
    );
    assert!(
        fresh
            .command()
            .arg("--model-config")
            .arg(&model)
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        read_rules(&fresh),
        json!([
            {"command":"cargo test","match":"prefix","decision":"allow"},{"command":"cargo test --all","match":"prefix","decision":"deny"}
        ])
    );
    let partial: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(
        partial["objects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("permission.rule:"))
                && item["outcome"] == "unsupported")
            .count(),
        5
    );
    assert_eq!(fs::read(source.0.join("dimcode.sqlite")).unwrap(), before);
}

#[test]
fn migration_terminal_resizes_and_restores_the_screen_after_cancellation() {
    let source = Home::new();
    legacy_home(
        &source.0,
        "dimcode.sqlite",
        "original history stays private",
    );
    let database = rusqlite::Connection::open(source.0.join("dimcode.sqlite")).unwrap();
    database.execute_batch("WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<1000)
        INSERT INTO sessions SELECT printf('session-%04d',n),cwd,title,status,tags,createdAt,updatedAt,version FROM numbers,sessions WHERE sessionId='old';
        INSERT INTO session_states SELECT sessionId,'local','diagram-model','{}','{}','agent' FROM sessions WHERE sessionId!='old';
        INSERT INTO messages SELECT sessionId||'-message',sessionId,'user','[{\"type\":\"text\",\"text\":\"private conversation body\"}]','[]','{}','a','2026-01-01T00:00:00Z' FROM sessions WHERE sessionId!='old';").unwrap();
    drop(database);
    let before = fs::read(source.0.join("dimcode.sqlite")).unwrap();
    let target = Home::new();
    let model = target.0.join("model.json");
    fs::write(
        &model,
        include_str!("../../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let mut command = target.command();
    command
        .env("TERM", "xterm-256color")
        .arg("--model-config")
        .arg(&model)
        .args([
            "migration",
            "--source",
            source.0.to_str().unwrap(),
            "--source-stopped",
        ]);
    let mut terminal = Terminal::launch(command);
    let resize = |terminal: &Terminal, columns, rows| {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::ioctl(
                    terminal.master.as_ref().unwrap().as_raw_fd(),
                    libc::TIOCSWINSZ,
                    &size,
                )
            },
            0
        );
    };
    resize(&terminal, 100, 30);
    terminal.until("Recent results (up to 8)");
    terminal.until("Imported");
    resize(&terminal, 60, 22);
    terminal.until("Recent results (up to 5)");
    resize(&terminal, 30, 8);
    terminal.until("Dim Migration [RUNNING]");
    terminal.signal(libc::SIGINT);
    assert_eq!(terminal.wait().code(), Some(130), "{}", terminal.transcript);
    assert!(terminal.transcript.contains("\u{1b}[?1049l"));
    assert!(terminal.transcript.contains("\u{1b}[?25h"));
    assert!(terminal.transcript.contains("Migration Cancelled"));
    assert!(terminal.transcript.contains("Full report:"));
    assert!(!terminal.transcript.contains("private conversation body"));
    assert!(!terminal.transcript.contains("\"objects\""));
    assert_eq!(fs::read(source.0.join("dimcode.sqlite")).unwrap(), before);
}

#[test]
fn default_migration_keeps_global_and_cn_in_their_own_homes() {
    let user = Home::new();
    let model = user.0.join("offline.json");
    fs::write(
        &model,
        include_str!("../../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let unrelated = user.0.join("development-home");
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dimcode"));
        command
            .env("HOME", &user.0)
            .env("DIMCODE_HOME", &unrelated)
            .env("DIMCODE_UPDATE_CHECK", "0")
            .arg("--model-config")
            .arg(&model);
        command
    };
    let missing = command()
        .args(["migration", "--source-stopped"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No default legacy v2 database"));
    let empty = command().args(["migration", "list"]).output().unwrap();
    assert!(empty.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&empty.stdout).unwrap(),
        json!({})
    );
    assert!(!unrelated.exists());
    let global = Home(user.0.join(".dim"));
    let cn = Home(user.0.join(".dimcode"));
    let global_source = global.0.join("v2");
    let cn_source = cn.0.join("v2");
    legacy_home(&global_source, "dimglobal.sqlite", "Global conversation");
    let single = command()
        .args(["migration", "inspect", "--source-stopped"])
        .output()
        .unwrap();
    assert!(single.status.success());
    let single: Value = serde_json::from_slice(&single.stdout).unwrap();
    assert_eq!(single.as_object().unwrap().len(), 1);
    assert_eq!(single["global"]["home"], global.0.to_str().unwrap());
    assert!(!cn.0.exists());
    legacy_home(&cn_source, "dimcode.sqlite", "CN conversation");
    for (source, rule) in [
        (&global_source, "git status *"),
        (&cn_source, "cargo test *"),
    ] {
        fs::write(
            source.join("permissions.json"),
            json!({"version":1,"execCommandPermissions":{"rules":{rule:"allow"}}}).to_string(),
        )
        .unwrap();
    }
    let global_bytes = fs::read(global_source.join("dimglobal.sqlite")).unwrap();
    let cn_bytes = fs::read(cn_source.join("dimcode.sqlite")).unwrap();
    let mut decline = command();
    decline.arg("migration");
    let mut terminal = Terminal::launch(decline);
    terminal.until("Continue? [y/N]");
    for expected in [&global_source, &global.0, &cn_source, &cn.0] {
        assert!(
            terminal.transcript.contains(expected.to_str().unwrap()),
            "{}",
            terminal.transcript
        );
    }
    terminal.write(b"n\n");
    assert_eq!(terminal.wait().code(), Some(130));
    assert!(!global.0.join("rust").exists());
    assert!(!cn.0.join("migration").exists());
    let inspect = command()
        .args(["migration", "inspect", "--source-stopped"])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspect: Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(inspect["global"]["home"], global.0.to_str().unwrap());
    assert_eq!(inspect["global"]["result"]["database"], "dimglobal.sqlite");
    assert_eq!(inspect["cn"]["home"], cn.0.to_str().unwrap());
    assert_eq!(inspect["cn"]["result"]["database"], "dimcode.sqlite");
    let scoped = global
        .command()
        .args(["migration", "inspect", "--source-stopped"])
        .output()
        .unwrap();
    assert!(scoped.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&scoped.stdout).unwrap()["database"],
        "dimglobal.sqlite"
    );
    assert!(!global.0.join("rust").exists());
    assert!(!cn.0.join("rust").exists());
    let _global_daemon = Daemon(&global, &model);
    let _cn_daemon = Daemon(&cn, &model);
    let mut import = command();
    import.env("TERM", "xterm-256color");
    import.arg("migration");
    let mut terminal = Terminal::launch(import);
    terminal.until("Continue? [y/N]");
    let size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.write(b"y\n");
    assert!(terminal.wait().success(), "{}", terminal.transcript);
    assert!(terminal.transcript.contains("\u{1b}[?1049h"));
    assert!(terminal.transcript.contains("\u{1b}[?1049l"));
    assert!(terminal.transcript.contains("Migration Completed"));
    assert!(terminal.transcript.contains("Full report:"));
    assert!(!terminal.transcript.contains("Global conversation"));
    assert!(!terminal.transcript.contains("CN conversation"));
    assert!(!terminal.transcript.contains("\"objects\""));
    let listed = command().args(["migration", "list"]).output().unwrap();
    assert!(listed.status.success());
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["global"]["result"].as_array().unwrap().len(), 1);
    assert_eq!(listed["cn"]["result"].as_array().unwrap().len(), 1);
    let task = listed["global"]["result"][0]["id"].as_str().unwrap();
    let mut retry = command();
    retry.args(["migration", "retry", task]);
    let mut terminal = Terminal::launch(retry);
    terminal.until("Continue? [y/N]");
    terminal.write(b"n\n");
    assert_eq!(terminal.wait().code(), Some(130));
    assert!(!terminal.transcript.contains("Migration task was not found"));
    assert!(!terminal.transcript.contains("{}"));
    let after_decline = command().args(["migration", "list"]).output().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&after_decline.stdout).unwrap(),
        listed
    );
    let mut identities = Vec::new();
    for (name, home, text, other, rule) in [
        (
            "global",
            &global,
            "Global conversation",
            "CN conversation",
            "git status",
        ),
        (
            "cn",
            &cn,
            "CN conversation",
            "Global conversation",
            "cargo test",
        ),
    ] {
        let id = listed[name]["result"][0]["id"].as_str().unwrap();
        let shown = command().args(["migration", "show", id]).output().unwrap();
        assert!(
            shown.status.success(),
            "{}",
            String::from_utf8_lossy(&shown.stderr)
        );
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown.as_object().unwrap().len(), 1);
        let report = &shown[name]["result"];
        assert_eq!(report["task"]["status"], "completed");
        let session = report["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "session")
            .unwrap()["targetId"]
            .as_str()
            .unwrap();
        let exported = home
            .command()
            .arg("--model-config")
            .arg(&model)
            .args(["session", "export", session, "--format", "json"])
            .output()
            .unwrap();
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stderr)
        );
        let history = String::from_utf8_lossy(&exported.stdout);
        assert!(history.contains(text), "{history}");
        assert!(!history.contains(other), "{history}");
        let native = rusqlite::Connection::open_with_flags(
            home.0.join("rust/dimcode.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let settings: String = native
            .query_row(
                "SELECT document FROM settings_documents WHERE source='user/permission'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&settings).unwrap()["commandRules"],
            json!([{"command":rule,"match":"prefix","decision":"allow"}])
        );
        identities.push((name, id.to_owned(), session.to_owned()));
    }
    let repeated = command()
        .args(["migration", "--source-stopped", "--json"])
        .output()
        .unwrap();
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    for (name, _, session) in &identities {
        assert!(
            repeated[*name]["result"]["objects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == "session"
                    && item["outcome"] == "alreadyImported"
                    && item["targetId"] == *session)
        );
    }
    let cn_task = &identities
        .iter()
        .find(|(name, _, _)| *name == "cn")
        .unwrap()
        .1;
    let retry = command()
        .args(["migration", "retry", cn_task, "--source-stopped"])
        .output()
        .unwrap();
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry.as_object().unwrap().len(), 1);
    assert_eq!(retry["cn"]["result"]["task"]["retryOf"], *cn_task);
    assert_eq!(
        fs::read(global_source.join("dimglobal.sqlite")).unwrap(),
        global_bytes
    );
    assert_eq!(
        fs::read(cn_source.join("dimcode.sqlite")).unwrap(),
        cn_bytes
    );
    let selected = command()
        .current_dir(&global.0)
        .args(["migration", "inspect", "--source", "v2", "--source-stopped"])
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&selected.stdout).unwrap()["database"],
        "dimglobal.sqlite"
    );
    assert!(!unrelated.exists());
    fs::write(
        global_source.join("dimglobal.sqlite"),
        b"unreadable legacy database",
    )
    .unwrap();
    let isolated = command()
        .args(["migration", "--source-stopped"])
        .output()
        .unwrap();
    assert_eq!(
        isolated.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&isolated.stdout)
    );
    let isolated: Value = serde_json::from_slice(&isolated.stdout).unwrap();
    assert!(
        isolated["global"].get("error").is_some()
            || isolated["global"]["result"]["task"]["status"] == "failed",
        "{isolated}"
    );
    assert_eq!(isolated["cn"]["result"]["task"]["status"], "completed");
    for home in [&global, &cn] {
        assert!(
            home.command()
                .arg("--model-config")
                .arg(&model)
                .args(["daemon", "stop"])
                .output()
                .unwrap()
                .status
                .success()
        );
        fs::rename(home.0.join("v2"), home.0.join("old-source-retained")).unwrap();
    }
    for (name, task, _) in &identities {
        let shown = command()
            .args(["migration", "show", task])
            .output()
            .unwrap();
        assert!(shown.status.success());
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown[*name]["result"]["task"]["id"], *task);
        let cancelled = command()
            .args(["migration", "cancel", task])
            .output()
            .unwrap();
        assert!(cancelled.status.success());
        let cancelled: Value = serde_json::from_slice(&cancelled.stdout).unwrap();
        assert_eq!(cancelled[*name]["result"]["task"]["id"], *task);
    }
    assert!(!global.0.join("rust/daemon.sock").exists());
    assert!(!cn.0.join("rust/daemon.sock").exists());
    assert!(!unrelated.exists());
}
