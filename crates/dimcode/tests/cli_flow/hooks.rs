//! `dimcode hooks list|enable|disable`: the effective catalog and this
//! workspace's enablement over the public wire (Hooks design A.7).
use super::*;

fn hello(wire: &mut Wire) {
    wire.hello(&["hooks.catalog", "hooks.set_enabled", "hooks.save"]);
}

/// The catalog a daemon serves before it reports the user document: the
/// reply of a daemon that has the methods but none of the newer facts.
fn catalog() -> Value {
    json!({
        "userRevision": "user:1",
        "projectsRevision": "user:1",
        "disableAllHooks": false,
        "sources": [
            {"source": "user", "enabled": true, "state": "ok"},
            {"source": "project", "path": "/work/.dimcode/hooks.json", "enabled": false, "state": "ok"},
        ],
        "hooks": [
            {"source": "user", "event": "SessionStart", "matcher": null, "command": "welcome", "timeout": 600, "onFailure": "continue", "async": false, "state": "ok"},
            {"source": "project", "event": "UnknownEvent", "matcher": "read", "command": "guard", "timeout": 600, "onFailure": "continue", "async": false, "state": "unsupported"},
        ],
    })
}

#[test]
fn hooks_list_prints_the_catalog_and_its_source_states() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, catalog());
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "SessionStart\t*\tuser\tenabled\tok\t600s\tcontinue\tsync\twelcome\n\
         UnknownEvent\tread\tproject\tdisabled\tunsupported\t600s\tcontinue\tsync\tguard\n"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("project hooks (/work/.dimcode/hooks.json): disabled"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hooks_list_prints_a_plugin_source_as_enabled() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1",
                "projectsRevision": "missing",
                "disableAllHooks": false,
                "sources": [
                    {"source": "user", "enabled": true, "state": "ok"},
                    {"source": "plugin:kit", "path": "/work/plugins/kit", "enabled": true, "state": "ok"},
                ],
                "hooks": [
                    {"source": "plugin:kit", "event": "Stop", "matcher": null, "command": "guard", "timeout": 600, "onFailure": "continue", "async": false, "state": "ok"},
                ],
            }),
        );
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Stop\t*\tplugin:kit\tenabled\tok\t600s\tcontinue\tsync\tguard\n"
    );
}

#[test]
fn hooks_list_says_when_no_hook_is_configured() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "missing",
                "projectsRevision": "missing",
                "disableAllHooks": false,
                "sources": [{"source": "user", "enabled": true, "state": "missing"}],
                "hooks": [],
            }),
        );
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "No hook is configured.\nuser hooks: missing\n"
    );
}

#[test]
fn hooks_enable_and_disable_send_the_workspace_and_the_revision() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let workspace = fs::canonicalize(&project).unwrap();
    let expected = workspace.to_string_lossy().into_owned();
    for enabled in [true, false] {
        let workspace = expected.clone();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("hooks.catalog");
            wire.reply(&request, catalog());
            let request = wire.read("hooks.set_enabled");
            assert_eq!(request["params"]["cwd"], workspace);
            assert_eq!(request["params"]["enabled"], enabled);
            assert_eq!(request["params"]["expectedRevision"], "user:1");
            wire.reply(&request, json!({"revision": "user:2"}));
            // The adapter reads the area again after the change.
            let request = wire.read("hooks.catalog");
            wire.reply(&request, catalog());
        });
        let output = home
            .command()
            .arg("--cwd")
            .arg(&project)
            .args(["hooks", if enabled { "enable" } else { "disable" }])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.starts_with(if enabled {
                "Enabled project hooks for "
            } else {
                "Disabled project hooks for "
            }),
            "{stdout}"
        );
        assert!(stdout.contains(&expected), "{stdout}");
        fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    }
}

/// A toggle the daemon accepted is done even when the read after it fails:
/// the line names the workspace, the read error follows, and exit is 0.
#[test]
fn hooks_enable_reports_the_toggle_when_the_read_after_it_fails() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let expected = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let workspace = expected.clone();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, catalog());
        let request = wire.read("hooks.set_enabled");
        assert_eq!(
            request["params"],
            json!({"cwd": workspace, "enabled": true, "expectedRevision": "user:1"})
        );
        wire.reply(&request, json!({"revision": "projects:2"}));
        // The toggle is committed; the report after it is what fails.
        let request = wire.read("hooks.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"hooks catalog unavailable",
            "data":{"appError":"hooks_failed"}}}));
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["hooks", "enable"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("Enabled project hooks for {expected}.\n")
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("hooks catalog unavailable"), "{stderr}");
}

/// A toggle the owner refuses over a newer document writes nothing and reads
/// the area again, as a save's conflict does: the subcommand exits 1 with the
/// conflict, and the panel shows it over the view it reads again.
#[test]
fn hooks_toggle_conflict_reads_the_area_again() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let workspace = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.set_enabled");
        assert_eq!(request["params"]["cwd"], workspace);
        assert_eq!(request["params"]["enabled"], true);
        assert_eq!(request["params"]["expectedRevision"], "projects:1");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"settings changed; read the current version before updating",
            "data":{"appError":"hooks_conflict"}}}));
        // Nothing was written, so the adapter reads the area again.
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["hooks", "enable"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "This workspace's hook settings changed since they were read; nothing was written."
        ),
        "{stderr}"
    );
    daemon.join().unwrap();
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    // The panel reads the area again and shows the same conflict, keeping
    // the project off because the owner wrote nothing.
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.set_enabled");
        assert_eq!(request["params"]["expectedRevision"], "projects:1");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"settings changed; read the current version before updating",
            "data":{"appError":"hooks_conflict"}}}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/hooks\r");
    terminal.until("The user's own document; always in effect.");
    terminal.raw(b"\x1b[B");
    terminal.until("/work/.dimcode/hooks.json");
    terminal.raw(b"\r");
    terminal.until("This workspace's hook settings");
    assert!(
        !terminal.transcript.contains("Enabled project."),
        "the refused toggle writes nothing: {}",
        terminal.transcript
    );
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn project_hooks_start_disabled_and_the_user_enables_them() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let project = home.0.join("work");
    fs::create_dir_all(project.join(".dimcode")).unwrap();
    fs::write(
        project.join(".dimcode/hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf 'seen' > hook-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
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
        let output = run(&["hooks", "list"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(
        list(),
        "Stop\t*\tproject\tdisabled\tok\t600s\tcontinue\tsync\tprintf 'seen' > hook-ran.txt\n"
    );
    assert!(String::from_utf8_lossy(&run(&["hooks", "list"]).stderr).contains("disabled"));
    let enabled = run(&["hooks", "enable"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert!(String::from_utf8_lossy(&enabled.stdout).starts_with("Enabled project hooks for "));
    assert_eq!(
        list(),
        "Stop\t*\tproject\tenabled\tok\t600s\tcontinue\tsync\tprintf 'seen' > hook-ran.txt\n"
    );
    // A later edit takes effect without a restart.
    fs::write(
        project.join(".dimcode/hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf 'later' > hook-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        list(),
        "Stop\t*\tproject\tenabled\tok\t600s\tcontinue\tsync\tprintf 'later' > hook-ran.txt\n"
    );
    assert!(run(&["hooks", "disable"]).status.success());
    assert_eq!(
        list(),
        "Stop\t*\tproject\tdisabled\tok\t600s\tcontinue\tsync\tprintf 'later' > hook-ran.txt\n"
    );
    // A broken document is a state, and disable still works from it.
    fs::write(project.join(".dimcode/hooks.json"), "{broken").unwrap();
    let broken = run(&["hooks", "list"]);
    assert!(
        broken.status.success(),
        "{}",
        String::from_utf8_lossy(&broken.stderr)
    );
    assert!(
        String::from_utf8_lossy(&broken.stderr).contains("invalid"),
        "{}",
        String::from_utf8_lossy(&broken.stderr)
    );
    fs::write(
        project.join(".dimcode/hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf 'later' > hook-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    // The user document is created through the CLI, without placing a file
    // first, and the next read reports it.
    let document = home.0.join("user-hooks.json");
    fs::write(
        &document,
        json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"printf 'user-one' > user-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    let saved = run(&["hooks", "set", document.to_str().unwrap()]);
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert!(
        String::from_utf8_lossy(&saved.stdout).contains("Saved."),
        "{}",
        String::from_utf8_lossy(&saved.stdout)
    );
    let listed = list();
    assert!(
        listed.contains(
            "SessionStart\t*\tuser\tenabled\tok\t600s\tcontinue\tsync\tprintf 'user-one' > user-ran.txt"
        ),
        "{listed}"
    );
    // The next hook point runs the document the CLI wrote. SessionStart
    // fires before the model is asked, so the offline configuration's dead
    // endpoint does not keep it from running; that configuration never
    // finishes a turn, so `Stop` is not reachable here. A turn's own failure
    // is expected and not this test's subject.
    let marker = project.join("user-ran.txt");
    let _ = run(&["exec", "-p", "go"]);
    assert_eq!(
        fs::read_to_string(&marker).unwrap_or_default(),
        "user-one",
        "the saved document did not reach the next hook point"
    );
    // A replacement reaches the next hook point too.
    fs::write(
        &document,
        json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"printf 'user-two' > user-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    assert!(
        run(&["hooks", "set", document.to_str().unwrap()])
            .status
            .success()
    );
    let _ = run(&["exec", "-p", "go"]);
    assert_eq!(
        fs::read_to_string(&marker).unwrap_or_default(),
        "user-two",
        "the replacement did not reach the next hook point"
    );
    assert!(run(&["daemon", "stop"]).status.success());
}

/// D-home: a `Stop` hook archives its Session with `dimcode export` from the
/// product home the daemon serves.
#[test]
fn a_stop_hook_exports_its_session_from_dimcode_home() {
    use dim_agent_capabilities::defaults;
    use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
    use dim_agent_types::InputMode;
    use dim_app_server::{ServerConfig, serve};

    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let exported = home.0.join("export.jsonl");
    let done = home.0.join("export.done");
    let command = format!(
        "{} export \"$DIM_SESSION_ID\" --output {} && touch {}",
        env!("CARGO_BIN_EXE_dimcode"),
        exported.display(),
        done.display()
    );
    fs::write(
        home.0.join("hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":command}]}]}}).to_string(),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        dim_app_server::hooks_settings::install(&mut builder, Some(home.0.clone()));
        dim_agent_capabilities::hooks::install(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                product_home: Some(home.0.clone()),
                target_home: Some(home.0.join("rust")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let session = server
            .agent()
            .unwrap()
            .create_session(SessionConfig {
                cwd: project.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        for _ in 0..500 {
            if done.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(done.exists(), "the Stop hook did not finish its export");
        let text = fs::read_to_string(&exported).unwrap_or_default();
        assert!(
            text.contains(session.id()),
            "the Stop hook did not export the Session: {text}"
        );
        assert!(text.contains("\"type\":\"turn\""), "{text}");
        assert!(text.contains("\"name\":\"end\""), "{text}");
    });
}

/// A catalog whose user source carries the document `hooks.edit` and
/// `hooks set` replace, beside a project and a plugin source.
fn document_catalog(revision: &str, document: Value) -> Value {
    json!({
        "userRevision": revision,
        "userDocument": document,
        "projectsRevision": "projects:1",
        "disableAllHooks": false,
        "sources": [
            {"source": "user", "enabled": true, "state": "ok"},
            {"source": "project", "path": "/work/.dimcode/hooks.json", "enabled": false, "state": "ok"},
            {"source": "plugin:kit", "path": "/work/plugins/kit", "enabled": true, "state": "ok"},
        ],
        "hooks": [],
    })
}

/// An editor that writes `body` over the draft file the command names.
fn editor(home: &Home, name: &str, body: &str) -> PathBuf {
    let path = home.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The client with `program` as its editor: the subcommands and the
/// terminal UI both run it.
fn edit_command(home: &Home, program: &std::path::Path, args: &[&str]) -> Command {
    let mut command = home.command();
    command
        .env_remove("VISUAL")
        .env("EDITOR", program)
        .args(args);
    command
}

/// The drafts the edits kept, one file per edit.
fn drafts(home: &Home) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = match fs::read_dir(home.0.join("edit")) {
        Ok(dir) => dir
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect(),
        Err(_) => Vec::new(),
    };
    paths.sort();
    paths
}

fn drains(wire: &mut Wire) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        line.clear();
    }
}

/// Reads requests until the client leaves, and fails on a write: a flow that
/// refuses before writing sends none.
fn drains_without_saving(wire: &mut Wire) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        assert!(!line.contains("\"hooks.save\""), "unexpected write: {line}");
        line.clear();
    }
}

/// `hooks edit` hands the user document to `$EDITOR` and writes what it
/// leaves, with the revision the document was read at.
#[test]
fn hooks_edit_writes_what_the_editor_leaves() {
    let home = Home::new();
    let script = editor(
        &home,
        "editor.sh",
        "printf '%s' '{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"printf hi\"}]}]}}' > \"$1\"",
    );
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        assert_eq!(
            request["params"]["document"]["hooks"]["Stop"][0]["hooks"][0]["command"],
            "printf hi"
        );
        wire.reply(&request, json!({"revision": "user:2"}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
    assert!(
        terminal.transcript.contains("Saved."),
        "{}",
        terminal.transcript
    );
}

/// An editor that leaves the text as it was writes nothing.
#[test]
fn hooks_edit_writes_nothing_when_the_editor_changes_nothing() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", "exit 0");
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
    assert!(
        terminal.transcript.contains("No change."),
        "{}",
        terminal.transcript
    );
    assert!(
        !fs::read_dir(home.0.join("edit")).is_ok_and(|mut dir| dir.any(|_| true)),
        "an unchanged draft is removed"
    );
}

/// Text that does not parse is not written: the error names the file that
/// keeps the user's text.
#[test]
fn hooks_edit_keeps_the_users_text_when_it_does_not_parse() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", "printf '%s' '{broken' > \"$1\"");
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(1), "{}", terminal.transcript);
    let kept = drafts(&home);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        terminal
            .transcript
            .contains(&format!("Your text is kept in {}.", kept[0].display())),
        "{}",
        terminal.transcript
    );
    assert_eq!(fs::read_to_string(&kept[0]).unwrap(), "{broken");
}

/// A stale revision writes nothing and keeps the user's text.
#[test]
fn hooks_edit_keeps_the_users_text_when_the_revision_is_stale() {
    let home = Home::new();
    let script = editor(
        &home,
        "editor.sh",
        "printf '%s' '{\"hooks\":{\"Stop\":[]}}' > \"$1\"",
    );
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"settings changed; read the current version before updating",
            "data":{"appError":"hooks_conflict"}}}));
        // The conflict writes nothing, so the adapter reads the area again
        // and reports the document as it now is.
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(1), "{}", terminal.transcript);
    assert!(
        terminal
            .transcript
            .contains("The user document changed since it was read; nothing was written."),
        "{}",
        terminal.transcript
    );
    let kept = drafts(&home);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        terminal.transcript.contains("Your text is kept in"),
        "{}",
        terminal.transcript
    );
    assert_eq!(
        fs::read_to_string(&kept[0]).unwrap(),
        "{\"hooks\":{\"Stop\":[]}}"
    );
    // A draft can hold commands, environment and headers, so it is readable
    // only by the user, in a directory only the user can enter.
    assert_eq!(
        fs::metadata(&kept[0]).unwrap().permissions().mode() & 0o777,
        0o600,
        "the draft is readable only by the user"
    );
    assert_eq!(
        fs::metadata(home.0.join("edit"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the draft directory is enterable only by the user"
    );
}

/// Each edit keeps its own file: a second refused edit does not overwrite
/// the text the first one kept.
#[test]
fn hooks_edit_keeps_one_file_per_edit() {
    let home = Home::new();
    for (name, text) in [("first.sh", "{first"), ("second.sh", "{second")] {
        let script = editor(&home, name, &format!("printf '%s' '{text}' > \"$1\""));
        let daemon = home.daemon(|wire| {
            hello(wire);
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            drains(wire);
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
        let status = terminal.wait();
        daemon.join().unwrap();
        assert_eq!(status.code(), Some(1), "{}", terminal.transcript);
        fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    }
    let kept = drafts(&home);
    assert_eq!(kept.len(), 2, "{kept:?}");
    let mut texts: Vec<String> = kept
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    texts.sort();
    assert_eq!(texts, ["{first", "{second"]);
}

/// An editor that exits non-zero writes nothing: the message names the
/// status and the file that keeps the text.
#[test]
fn hooks_edit_writes_nothing_when_the_editor_exits_non_zero() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", "exit 1");
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(1), "{}", terminal.transcript);
    assert!(
        terminal
            .transcript
            .contains("The editor exited with status 1. Nothing was written."),
        "{}",
        terminal.transcript
    );
    let kept = drafts(&home);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        terminal
            .transcript
            .contains(&format!("Your text is kept in {}.", kept[0].display())),
        "{}",
        terminal.transcript
    );
}

/// The editor value may quote a path with a space, as git allows.
#[test]
fn hooks_edit_runs_an_editor_path_with_a_space() {
    let home = Home::new();
    let directory = home.0.join("ed itors");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("editor.sh");
    fs::write(
        &path,
        "#!/bin/sh\nprintf '%s' '{\"hooks\":{\"Stop\":[]}}' > \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        wire.reply(&request, json!({"revision": "user:2"}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
    });
    let mut command = home.command();
    command
        .env_remove("VISUAL")
        .env("EDITOR", format!("'{}'", path.display()))
        .args(["hooks", "edit"]);
    let mut terminal = Terminal::launch_tui(command);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
    assert!(
        terminal.transcript.contains("Saved."),
        "{}",
        terminal.transcript
    );
}

/// An interactive edit needs a terminal; the error names the twin.
#[test]
fn hooks_edit_refuses_stdin_that_is_not_a_terminal() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        drains(wire);
    });
    let output = home.command().args(["hooks", "edit"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hooks edit needs a terminal"), "{stderr}");
    assert!(stderr.contains("hooks set PATH"), "{stderr}");
}

/// `hooks set PATH` writes the file with the revision read just before it.
#[test]
fn hooks_set_writes_a_file_with_the_revision_read_before_it() {
    let home = Home::new();
    let file = home.0.join("hooks.json");
    fs::write(
        &file,
        json!({"hooks": {"Stop": [{"hooks": []}]}}).to_string(),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        assert_eq!(
            request["params"]["document"]["hooks"]["Stop"][0]["hooks"],
            json!([])
        );
        wire.reply(&request, json!({"revision": "user:2"}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
    });
    let output = home
        .command()
        .args(["hooks", "set"])
        .arg(&file)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Saved.\n");
}

/// `hooks set -` reads its document from stdin and keeps it when the write
/// is refused.
#[test]
fn hooks_set_reads_stdin_and_keeps_it_when_the_write_is_refused() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"settings changed; read the current version before updating",
            "data":{"appError":"hooks_conflict"}}}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
        drains(wire);
    });
    let mut child = home
        .command()
        .args(["hooks", "set", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"hooks": {"Stop": []}}).to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
    let kept = drafts(&home);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(stderr.contains(&kept[0].display().to_string()), "{stderr}");
    assert_eq!(
        fs::read_to_string(&kept[0]).unwrap(),
        json!({"hooks": {"Stop": []}}).to_string()
    );
}

/// `/hooks` lists every source as an entry, turns the project's on, and a
/// plugin's source offers no write.
#[test]
fn tui_hooks_panel_lists_the_sources_and_toggles_the_project() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("hooks.catalog");
        // The panel works on the Session's workspace, not the directory the
        // client started in.
        assert_eq!(request["params"]["cwd"], "/workspace");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        // The write reads the project revision immediately before it.
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.set_enabled");
        assert_eq!(request["params"]["cwd"], "/workspace");
        assert_eq!(request["params"]["enabled"], true);
        assert_eq!(request["params"]["expectedRevision"], "projects:1");
        wire.reply(&request, json!({"revision": "projects:2"}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/hooks\r");
    // Rows are drawn before the footer, so the row's state is waited for
    // before the detail that follows it in the same frame.
    terminal.until("read-only");
    terminal.until("The user's own document; always in effect.");
    terminal.raw(b"\x1b[B");
    terminal.until("/work/.dimcode/hooks.json");
    terminal.raw(b"\r");
    terminal.until("Enabled project.");
    terminal.raw(b"\x1b[B");
    terminal.until("Owned by plugin kit.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// Enter toggles the project, and only the read after the toggle fails: the
/// panel reports what it did and the read error, and keeps no view, in both
/// terminal modes.
#[test]
fn tui_hooks_panel_settles_a_toggle_whose_read_fails() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            // The write reads the project revision immediately before it.
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            let request = wire.read("hooks.set_enabled");
            assert_eq!(request["params"]["enabled"], true);
            wire.reply(&request, json!({"revision": "projects:2"}));
            // The toggle is committed; the report after it is what fails.
            let request = wire.read("hooks.catalog");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"hooks catalog unavailable",
                "data":{"appError":"hooks_failed"}}}));
            drains(wire);
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/hooks\r");
        terminal.until("The user's own document; always in effect.");
        terminal.raw(b"\x1b[B");
        terminal.until("/work/.dimcode/hooks.json");
        terminal.raw(b"\r");
        terminal.until("Enabled project.");
        terminal.until("hooks catalog unavailable");
        let settled = terminal.transcript.clone();
        assert!(!settled.contains("Disabled project."), "{settled}");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert_eq!(
            terminal
                .transcript
                .matches("hooks catalog unavailable")
                .count(),
            1,
            "the read error is drawn once: {}",
            terminal.transcript
        );
    }
}

/// The catalog of a workspace with no hook file anywhere: every source
/// carries the path its owner reports, and the owner reports `missing`.
fn missing_catalog() -> Value {
    json!({
        "userRevision": "missing",
        "projectsRevision": "missing",
        "disableAllHooks": false,
        "sources": [
            {"source": "user", "enabled": true, "state": "missing"},
            {"source": "project", "path": "/work/.dimcode/hooks.json", "enabled": false, "state": "missing"},
        ],
        "hooks": [],
    })
}

/// A source its owner reports as `missing` has no document yet, which is not
/// an invalid one: both surfaces name its path and say there is no hooks file
/// there, and neither calls it invalid.
#[test]
fn a_missing_source_shows_its_path_and_says_there_is_no_hooks_file() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("hooks.catalog");
        wire.reply(&request, missing_catalog());
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/hooks\r");
    terminal.until("The user's own document; always in effect.");
    // Rows are drawn before the footer, so both rows are on screen once the
    // project's detail is.
    terminal.raw(b"\x1b[B");
    terminal.until("/work/.dimcode/hooks.json");
    terminal.until("There is no hooks file there.");
    assert!(
        !terminal.transcript.contains("invalid"),
        "a missing source is not invalid: {}",
        terminal.transcript
    );
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    // The command line says the same line for the same source.
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, missing_catalog());
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("project hooks (/work/.dimcode/hooks.json): There is no hooks file there."),
        "{stderr}"
    );
}

/// Ctrl+E hands the user document to `$EDITOR` and writes what it leaves
/// with the revision the panel read, in both terminal modes.
#[test]
fn tui_hooks_panel_edits_the_user_document() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(
            &home,
            "editor.sh",
            "printf '%s' '{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"printf hi\"}]}]}}' > \"$1\"",
        );
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:1");
            assert_eq!(
                request["params"]["document"]["hooks"]["Stop"][0]["hooks"][0]["command"],
                "printf hi"
            );
            wire.reply(&request, json!({"revision": "user:2"}));
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
            drains(wire);
        });
        let mut command = edit_command(&home, &script, &[]);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/hooks\r");
        terminal.until("The user's own document; always in effect.");
        terminal.raw(b"\x05");
        terminal.until("Saved.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A daemon that reports no user document offers no edit: the user source
/// has no Ctrl+E, and the key does nothing, in both terminal modes.
#[test]
fn tui_hooks_panel_offers_no_edit_without_a_user_document() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("hooks.catalog");
            wire.reply(&request, catalog());
            drains_without_saving(wire);
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/hooks\r");
        terminal.until("The user's own document; always in effect.");
        let opened = terminal.transcript.clone();
        assert!(opened.contains("Ctrl+R"), "{opened}");
        assert!(!opened.contains("Ctrl+E"), "no edit is offered: {opened}");
        terminal.raw(b"\x05");
        thread::sleep(Duration::from_millis(200));
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// `hooks edit` refuses a daemon that reports no user document before the
/// editor opens: no draft is made and nothing is written.
#[test]
fn hooks_edit_refuses_a_daemon_without_a_user_document() {
    let home = Home::new();
    let script = editor(
        &home,
        "editor.sh",
        "printf '%s' '{\"hooks\":{\"Stop\":[]}}' > \"$1\"",
    );
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, catalog());
        drains_without_saving(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(1), "{}", terminal.transcript);
    assert!(
        terminal
            .transcript
            .contains("The daemon reports no hook document to edit."),
        "{}",
        terminal.transcript
    );
    assert!(drafts(&home).is_empty(), "the editor never opened");
}

/// `hooks set -` refuses a daemon that reports no user document: nothing is
/// written and the text it read leaves no draft behind.
#[test]
fn hooks_set_refuses_a_daemon_without_a_user_document() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, catalog());
        drains_without_saving(wire);
    });
    let mut child = home
        .command()
        .args(["hooks", "set", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"hooks": {"Stop": []}}).to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("The daemon reports no hook document to edit."),
        "{stderr}"
    );
    assert!(drafts(&home).is_empty(), "no draft for a refused write");
}

/// A save the daemon accepted is done even when the read after it fails: the
/// saved line, then the read error, and the draft is removed.
#[test]
fn hooks_set_reports_the_save_when_the_read_after_it_fails() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        wire.reply(&request, json!({"revision": "user:2"}));
        let request = wire.read("hooks.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"hooks catalog unavailable",
            "data":{"appError":"hooks_failed"}}}));
        drains(wire);
    });
    let mut child = home
        .command()
        .args(["hooks", "set", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"hooks": {"Stop": []}}).to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Saved.\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hooks catalog unavailable"), "{stderr}");
    assert!(drafts(&home).is_empty(), "the written draft is removed");
}

/// A conflict whose re-read fails is still a conflict: nothing was written,
/// the command says so with the read error that followed, and exits 1.
#[test]
fn hooks_set_reports_a_conflict_when_the_read_after_it_fails() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"settings changed; read the current version before updating",
            "data":{"appError":"hooks_conflict"}}}));
        // The conflict writes nothing, and the read of the area as it now is
        // fails.
        let request = wire.read("hooks.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"hooks catalog unavailable",
            "data":{"appError":"hooks_failed"}}}));
        drains(wire);
    });
    let mut child = home
        .command()
        .args(["hooks", "set", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"hooks": {"Stop": []}}).to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The user document changed since it was read; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains("hooks catalog unavailable"), "{stderr}");
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
    assert_eq!(drafts(&home).len(), 1, "the refused draft stays");
}

/// An editor that writes a different document for a draft holding the
/// stored one: the next edit is told which document it was opened on.
const ANSWERS_STORED: &str = "case \"$(cat \"$1\")\" in\n\
    *stored*) printf '%s' '{\"hooks\":{\"Stop\":[\"kept\"]}}' > \"$1\";;\n\
    *) printf '%s' '{\"hooks\":{\"Stop\":[\"old\"]}}' > \"$1\";;\n\
    esac";

/// Ctrl+E saves, and only the read after the save fails: the panel reports
/// the save and the read error, keeps no draft, and Ctrl+R reads the area
/// again so the next edit opens the document that read returned, in both
/// terminal modes.
#[test]
fn tui_hooks_panel_settles_a_save_whose_read_fails() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", ANSWERS_STORED);
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:1");
            wire.reply(&request, json!({"revision": "user:2"}));
            // The save is committed; the report after it is what fails.
            let request = wire.read("hooks.catalog");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"hooks catalog unavailable",
                "data":{"appError":"hooks_failed"}}}));
            // Ctrl+R reads the area again.
            let request = wire.read("hooks.catalog");
            wire.reply(
                &request,
                document_catalog("user:9", json!({"hooks": {"Stop": ["stored"]}})),
            );
            // The next edit opens that document at the revision it was read
            // at, not the draft the save removed.
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:9");
            assert_eq!(
                request["params"]["document"]["hooks"]["Stop"][0],
                json!("kept")
            );
            wire.reply(&request, json!({"revision": "user:10"}));
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:10", json!({"hooks": {}})));
            drains(wire);
        });
        let mut command = edit_command(&home, &script, &[]);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/hooks\r");
        terminal.until("The user's own document; always in effect.");
        terminal.raw(b"\x05");
        terminal.until("hooks catalog unavailable");
        let settled = terminal.transcript.clone();
        assert!(settled.contains("Saved."), "{settled}");
        assert!(
            !settled.contains("Your text is kept"),
            "the written draft is not named as kept: {settled}"
        );
        assert!(drafts(&home).is_empty(), "the written draft is removed");
        // Ctrl+R reads the area, and the rows come back.
        terminal.raw(b"\x12");
        terminal.until("plugin:kit");
        terminal.raw(b"\x05");
        terminal.until("Saved.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(drafts(&home).is_empty(), "the written draft is removed");
        assert_eq!(
            terminal
                .transcript
                .matches("hooks catalog unavailable")
                .count(),
            1,
            "the read error is drawn once: {}",
            terminal.transcript
        );
    }
}

/// A conflict whose re-read fails still settles the draft: the panel says
/// nothing was written, shows the read error once, keeps no view, and the
/// next edit opens the stored document, in both terminal modes.
#[test]
fn tui_hooks_panel_drops_the_draft_when_the_conflict_read_fails() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", ANSWERS_STORED);
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:1");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"settings changed; read the current version before updating",
                "data":{"appError":"hooks_conflict"}}}));
            // The conflict writes nothing, and the read of the area as it now
            // is fails.
            let request = wire.read("hooks.catalog");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"hooks catalog unavailable",
                "data":{"appError":"hooks_failed"}}}));
            // Ctrl+R reads the area, and the next edit opens the document
            // that read returned, not the draft the save kept.
            let request = wire.read("hooks.catalog");
            wire.reply(
                &request,
                document_catalog("user:9", json!({"hooks": {"Stop": ["stored"]}})),
            );
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:9");
            assert_eq!(
                request["params"]["document"]["hooks"]["Stop"][0],
                json!("kept")
            );
            wire.reply(&request, json!({"revision": "user:10"}));
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:10", json!({"hooks": {}})));
            drains(wire);
        });
        let mut command = edit_command(&home, &script, &[]);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/hooks\r");
        terminal.until("The user's own document; always in effect.");
        terminal.raw(b"\x05");
        terminal.until("The user document changed since it was read");
        terminal.until("Your text is kept");
        terminal.until("hooks catalog unavailable");
        let settled = terminal.transcript.clone();
        assert!(!settled.contains("Saved."), "{settled}");
        // The link to the stale revision went, so the next edit is the stored
        // document's.
        terminal.raw(b"\x12");
        terminal.until("plugin:kit");
        terminal.raw(b"\x05");
        terminal.until("Saved.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert_eq!(
            terminal
                .transcript
                .matches("hooks catalog unavailable")
                .count(),
            1,
            "the read error is drawn once: {}",
            terminal.transcript
        );
    }
}

/// An editor that answers differently once the draft already holds its own
/// text: what a reopened draft gets that a fresh one does not.
const WITH_MEMORY: &str = "case \"$(cat \"$1\")\" in\n\
    *fresh*) printf '%s' '{\"hooks\":{\"Stop\":[\"reopened\"]}}' > \"$1\";;\n\
    *) printf '%s' '{\"hooks\":{\"Stop\":[\"fresh\"]}}' > \"$1\";;\n\
    esac";

/// A stale revision writes nothing, says so, reads the area again, and the
/// next edit opens the document as it now is, in both terminal modes.
#[test]
fn tui_hooks_panel_keeps_the_users_text_when_the_write_is_refused() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", WITH_MEMORY);
        let (shown, wait_shown) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:1");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"settings changed; read the current version before updating",
                "data":{"appError":"hooks_conflict"}}}));
            // The conflict writes nothing, so the adapter reads the area
            // again, and the panel moves to the newer revision.
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
            // The next edit opens the stored document at that revision, not
            // the rejected text.
            let request = wire.read("hooks.save");
            assert_eq!(request["params"]["expectedRevision"], "user:2");
            assert_eq!(
                request["params"]["document"]["hooks"]["Stop"][0],
                json!("fresh")
            );
            wire.reply(&request, json!({"revision": "user:3"}));
            let request = wire.read("hooks.catalog");
            wire.reply(&request, document_catalog("user:3", json!({"hooks": {}})));
            drains(wire);
        });
        let mut command = edit_command(&home, &script, &[]);
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/hooks\r");
        terminal.until("The user's own document; always in effect.");
        terminal.raw(b"\x05");
        terminal.until("Your text is kept");
        // The first text stays where the message says it is.
        let kept = drafts(&home);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(
            fs::read_to_string(&kept[0]).unwrap(),
            "{\"hooks\":{\"Stop\":[\"fresh\"]}}"
        );
        // Ctrl+E again writes the current document, so it can be written.
        terminal.raw(b"\x05");
        terminal.until("Saved.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        let kept = drafts(&home);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(
            fs::read_to_string(&kept[0]).unwrap(),
            "{\"hooks\":{\"Stop\":[\"fresh\"]}}"
        );
    }
}

/// A text the owner refuses without a newer document keeps the panel's
/// revision through a toggle on another entry, so the next Ctrl+E reopens
/// the user's text and writes it again.
#[test]
fn tui_hooks_panel_reopens_the_users_text_after_a_refusal() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", WITH_MEMORY);
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"hooks document is invalid",
            "data":{"appError":"hooks_invalid"}}}));
        // A toggle on another entry goes through: it is no draft of its own,
        // so it leaves the kept text linked to its revision.
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        let request = wire.read("hooks.set_enabled");
        assert_eq!(request["params"]["enabled"], true);
        assert_eq!(request["params"]["expectedRevision"], "projects:1");
        wire.reply(&request, json!({"revision": "projects:2"}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        // No newer document: the revision stays, and the reopened draft is
        // written with it.
        let request = wire.read("hooks.save");
        assert_eq!(request["params"]["expectedRevision"], "user:1");
        // The editor found its own text in the reopened draft; a fresh copy
        // of the stored document would have written "fresh" again.
        assert_eq!(
            request["params"]["document"]["hooks"]["Stop"][0],
            json!("reopened")
        );
        wire.reply(&request, json!({"revision": "user:2"}));
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:2", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &[]));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/hooks\r");
    terminal.until("The user's own document; always in effect.");
    terminal.raw(b"\x05");
    terminal.until("hooks document is invalid");
    terminal.until("Your text is kept");
    terminal.raw(b"\x1b[B");
    terminal.until("/work/.dimcode/hooks.json");
    terminal.raw(b"\r");
    terminal.until("Enabled project.");
    terminal.raw(b"\x1b[A");
    terminal.raw(b"\x05");
    terminal.until("Saved.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(drafts(&home).is_empty(), "the written draft is removed");
}

/// A refused editor exit writes nothing: the panel names the status and
/// keeps the file.
#[test]
fn tui_hooks_panel_writes_nothing_when_the_editor_exits_non_zero() {
    let home = Home::new();
    let script = editor(&home, "editor.sh", "exit 1");
    let (shown, wait_shown) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("hooks.catalog");
        wire.reply(&request, document_catalog("user:1", json!({"hooks": {}})));
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &[]));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/hooks\r");
    terminal.until("The user's own document; always in effect.");
    terminal.raw(b"\x05");
    terminal.until("The editor exited with status 1. Nothing was written.");
    terminal.until("Your text is kept");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert_eq!(drafts(&home).len(), 1, "the draft is kept");
}

/// A daemon without the Hooks methods is refused by name, at either
/// surface, never read from a file — and a daemon that lists the catalog
/// but cannot write refuses before the editor opens.
#[test]
fn hooks_commands_refuse_a_daemon_that_does_not_advertise_them() {
    for verb in ["list", "enable", "set"] {
        let home = Home::new();
        let file = home.0.join("hooks.json");
        fs::write(&file, json!({"hooks": {}}).to_string()).unwrap();
        let args: Vec<&str> = match verb {
            "set" => vec!["hooks", "set", file.to_str().unwrap()],
            verb => vec!["hooks", verb],
        };
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            drains(wire);
        });
        let output = home.command().args(&args).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("This daemon cannot manage hooks."),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // The catalog alone is no write: `edit` says so before the user spends
    // time in an editor whose result could never be saved.
    let home = Home::new();
    let script = editor(&home, "editor.sh", "exit 0");
    let daemon = home.daemon(|wire| {
        wire.hello(&["hooks.catalog"]);
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, &["hooks", "edit"]));
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(1), "{}", terminal.transcript);
    assert!(
        terminal
            .transcript
            .contains("This daemon lists hooks but cannot edit them."),
        "{}",
        terminal.transcript
    );
    assert!(drafts(&home).is_empty(), "the editor never opened");
    // The catalog alone is no write for `set` either: it refuses before the
    // input can hold the call, so a writer that never closes stdin is not
    // kept waiting, and no draft is made just to report the refusal.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["hooks.catalog"]);
        drains(wire);
    });
    let mut child = home
        .command()
        .args(["hooks", "set", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let waited = loop {
        match child.try_wait().unwrap() {
            Some(status) => break Some(status),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => break None,
        }
    };
    let output = match waited {
        Some(_) => child.wait_with_output().unwrap(),
        None => {
            // The writer is still open in this process: only a read of it
            // keeps the CLI here.
            let _ = child.kill();
            let _ = child.wait();
            daemon.join().unwrap();
            panic!("hooks set - read stdin before refusing the daemon");
        }
    };
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon lists hooks but cannot edit them."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(drafts(&home).is_empty(), "no draft for a refused write");
}

/// A daemon without the Hooks methods leaves the panel open on the
/// unsupported-daemon error rather than an empty list.
#[test]
fn tui_hooks_panel_says_when_the_daemon_cannot_manage_them() {
    let home = Home::new();
    let (shown, wait_shown) = std::sync::mpsc::channel();
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
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/hooks\r");
    terminal.until("This daemon cannot manage hooks.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}
