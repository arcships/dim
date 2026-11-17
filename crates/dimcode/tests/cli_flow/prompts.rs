//! `dimcode prompt ...`: the user's overrides of the built-in policy texts
//! through `settings.read` and `settings.update` ([prompt
//! overrides](../../../../docs/design/appserver/prompt-overrides.md)).
use super::*;

fn hello(wire: &mut Wire) {
    wire.hello(&["settings.read", "settings.update"]);
}

fn read(wire: &mut Wire, revision: &str, document: Value) {
    let request = wire.read("settings.read");
    assert_eq!(request["params"]["source"], "prompts");
    assert_eq!(request["params"]["scope"], "user");
    wire.reply(
        &request,
        json!({"revision": revision, "document": document}),
    );
}

#[test]
fn prompt_lists_the_three_texts_and_their_origin() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(
            wire,
            "rev:1",
            json!({"summary": "KEEP THIS SHORT", "skills": "K"}),
        );
    });
    let output = home.command().args(["prompt"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "prompts (user scope, revision rev:1)\n\
         \x20 base     built-in\n\
         \x20 summary  custom (15 B)\n\
         \x20 skills   custom (1 B)\n"
    );
}

#[test]
fn prompt_show_prints_one_override() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(wire, "rev:1", json!({"base": "BASE TEXT"}));
    });
    let shown = home
        .command()
        .args(["prompt", "show", "base"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(shown.status.success());
    assert_eq!(String::from_utf8(shown.stdout).unwrap(), "BASE TEXT\n");
}

#[test]
fn prompt_show_names_a_built_in() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(wire, "rev:1", json!({"base": "BASE TEXT"}));
    });
    let missing = home
        .command()
        .args(["prompt", "show", "summary"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("summary: built-in (no override)"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
fn prompt_set_merges_one_field_and_keeps_the_others() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(wire, "rev:1", json!({"skills": "K"}));
        let update = wire.read("settings.update");
        assert_eq!(update["params"]["source"], "prompts");
        assert_eq!(update["params"]["scope"], "user");
        assert_eq!(update["params"]["expectedRevision"], "rev:1");
        assert_eq!(
            update["params"]["document"],
            json!({"base": "NEW BASE", "skills": "K"})
        );
        wire.reply(
            &update,
            json!({"revision": "rev:2", "document": {"base": "NEW BASE", "skills": "K"}}),
        );
    });
    let mut child = home
        .command()
        .args(["prompt", "set", "base", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"NEW BASE").unwrap();
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("base: saved"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn prompt_edit_writes_what_the_editor_leaves() {
    let home = Home::new();
    let script = home.0.join("editor.sh");
    fs::write(&script, "#!/bin/sh\nprintf '%s' 'EDITED BASE' > \"$1\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(wire, "rev:1", json!({}));
        let update = wire.read("settings.update");
        assert_eq!(update["params"]["expectedRevision"], "rev:1");
        assert_eq!(update["params"]["document"], json!({"base": "EDITED BASE"}));
        wire.reply(
            &update,
            json!({"revision": "rev:2", "document": {"base": "EDITED BASE"}}),
        );
    });
    let mut command = home.command();
    command
        .env_remove("VISUAL")
        .env("EDITOR", &script)
        .args(["prompt", "edit", "base"]);
    let mut terminal = Terminal::launch_tui(command);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
    assert!(
        terminal.transcript.contains("base: saved"),
        "{}",
        terminal.transcript
    );
}

#[test]
fn prompt_set_skills_warns_about_the_protocol_sentences() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(wire, "rev:1", json!({}));
        let update = wire.read("settings.update");
        assert_eq!(update["params"]["document"], json!({"skills": "K"}));
        wire.reply(
            &update,
            json!({"revision": "rev:2", "document": {"skills": "K"}}),
        );
    });
    let mut child = home
        .command()
        .args(["prompt", "set", "skills", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"K").unwrap();
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("marker and call shape"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn prompt_reset_removes_only_its_field() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        read(wire, "rev:1", json!({"base": "B", "summary": "S"}));
        let update = wire.read("settings.update");
        assert_eq!(update["params"]["expectedRevision"], "rev:1");
        assert_eq!(update["params"]["document"], json!({"summary": "S"}));
        wire.reply(
            &update,
            json!({"revision": "rev:2", "document": {"summary": "S"}}),
        );
    });
    let output = home
        .command()
        .args(["prompt", "reset", "base"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("base: reset"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
