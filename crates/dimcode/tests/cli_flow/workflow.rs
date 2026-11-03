//! `dimcode workflow ...` and the `/workflow` panel: the Workflow area
//! through its adapter, over the public wire.
use super::*;
use std::sync::mpsc;

/// A workspace directory the commands run in, and the string the commands
/// send as its cwd.
fn workspace(home: &Home) -> (PathBuf, String) {
    let path = home.0.join("work");
    fs::create_dir(&path).unwrap();
    let cwd = fs::canonicalize(&path)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (path, cwd)
}

fn hello(wire: &mut Wire) {
    wire.hello(&[
        "workflow.list",
        "workflow.save",
        "workflow.status",
        "settings.read",
        "settings.update",
    ]);
}

/// The user Settings snapshot: one saved definition, one disabled project
/// definition and the limit fields the owner declares.
fn settings() -> Value {
    json!({
        "revision": "settings:1",
        "document": {
            "definitions": {
                "review-and-fix": {
                    "schema": "dim.workflow/v1",
                    "name": "review-and-fix",
                    "description": "Analyze, fix in parallel, summarize",
                    "parameters": [
                        {"name": "target", "description": "Path to review", "required": true}
                    ],
                    "steps": [{"stage": {"id": "review", "prompt": "Review {{params.target}}"}}]
                }
            },
            "projectEnabled": {"/workspace": {"nightly-check": false}},
            "max_concurrent_runs": 4,
            "max_concurrent_stages": 4
        }
    })
}

/// What `workflow.list` reports: a valid user definition, an invalid
/// disabled project one and a package one.
fn definitions() -> Value {
    json!([
        {
            "name": "review-and-fix", "source": "user", "enabled": true,
            "description": "Analyze, fix in parallel, summarize",
            "parameters": [
                {"name": "target", "description": "Path to review", "required": true}
            ],
            "valid": true, "issues": [], "issue_count": 0
        },
        {
            "name": "nightly-check", "source": "project", "enabled": false,
            "description": "Nightly checks", "parameters": [],
            "valid": false,
            "issues": ["Stage build: invalid reference {{params.branch}}"],
            "issue_count": 12
        },
        {
            "name": "release-notes", "source": "plugin", "package_id": "kit",
            "enabled": true, "description": "Release notes", "parameters": [],
            "valid": true, "issues": [], "issue_count": 0
        }
    ])
}

/// The `workflow.list` reply around `definitions()`: the entries and whether
/// the owner cut the listing at one payload.
fn listing(truncated: bool) -> Value {
    json!({"definitions": definitions(), "truncated": truncated})
}

/// The interface's opening exchange, and the two requests it makes on its
/// own before a command. `features` are the management methods the daemon
/// advertises besides the Session basics.
fn opened_with(wire: &mut Wire, features: &[&str]) {
    wire.hello(features);
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
            Some("session.context.usage") => wire
                .send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
            method => panic!("unexpected request {method:?}"),
        }
    }
}

fn opened(wire: &mut Wire) {
    opened_with(
        wire,
        &[
            "workflow.list",
            "workflow.save",
            "workflow.status",
            "settings.read",
            "settings.update",
        ],
    );
}

/// An editor that writes `script` as its body over the draft file.
fn editor(home: &Home, script: &str) -> PathBuf {
    let path = home.0.join("editor.sh");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn edit_command(home: &Home, program: &std::path::Path, fullscreen: bool) -> Command {
    let mut command = home.command();
    command.env_remove("VISUAL").env("EDITOR", program);
    if fullscreen {
        command.arg("--fullscreen");
    }
    command
}

fn drains(wire: &mut Wire) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        line.clear();
    }
}

/// Reads requests until the client leaves, and fails on a definition write.
fn drains_without_saving(wire: &mut Wire) {
    drains_without(wire, "workflow.save");
}

/// Reads requests until the client leaves, and fails on a start.
fn drains_without_starting(wire: &mut Wire) {
    drains_without(wire, "workflow.start");
}

fn drains_without(wire: &mut Wire, method: &str) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        assert!(
            !line.contains(&format!("\"{method}\"")),
            "unexpected call: {line}"
        );
        line.clear();
    }
}

#[test]
fn workflow_list_prints_the_definitions_and_their_issues() {
    let home = Home::new();
    let (project, cwd) = workspace(&home);
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        assert_eq!(request["params"], json!({"cwd": cwd}));
        wire.reply(&request, listing(true));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "list"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "review-and-fix\tuser\tenabled\tAnalyze, fix in parallel, summarize\n\
         nightly-check\tproject\tdisabled, invalid\tNightly checks\n\
         release-notes\tplugin\tenabled\tRelease notes\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Columns: name, source, state, description."),
        "{stderr}"
    );
    assert!(
        stderr.contains("nightly-check: Stage build: invalid reference {{params.branch}}"),
        "{stderr}"
    );

    assert!(
        stderr.contains("The list reached its size limit; later definitions are not shown."),
        "{stderr}"
    );
    // A name such a listing does not hold may be past the cut: the refusal
    // says so, for a history and a start alike.
    for args in [
        ["workflow", "history", "past-the-cut"],
        ["workflow", "start", "past-the-cut"],
    ] {
        let home = Home::new();
        let (project, cwd) = workspace(&home);
        let daemon = home.daemon(move |wire| {
            // A start is refused for a missing kind before any read when the
            // daemon does not advertise `workflow.start`.
            wire.hello(&[
                "workflow.list",
                "workflow.save",
                "workflow.start",
                "workflow.status",
                "settings.read",
                "settings.update",
            ]);
            let request = wire.read("workflow.list");
            assert_eq!(request["params"], json!({"cwd": cwd}));
            wire.reply(&request, listing(true));
            drains(wire);
        });
        let output = home
            .command()
            .arg("--cwd")
            .arg(&project)
            .args(args)
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(
                "No workflow definition named past-the-cut is in the list, which reached its size limit."
            ),
            "{stderr}"
        );
    }
}

/// `--help` lists the Workflow area with the other management areas.
#[test]
fn help_lists_the_workflow_commands() {
    let home = Home::new();
    let output = home.command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for line in [
        "workflow list",
        "workflow enable|disable NAME",
        "workflow edit NAME",
        "workflow set NAME PATH",
        "workflow add PATH",
        "workflow start NAME [PATH]",
        "workflow history NAME",
    ] {
        assert!(help.contains(line), "{help}");
    }
}

/// Enabling a project definition is a client write of the user document: the
/// document is copied as read, so an unknown member and another workspace's
/// map survive, and the write CASes the revision the read reported.
#[test]
fn workflow_enable_writes_the_enablement_and_keeps_the_document_as_read() {
    let home = Home::new();
    let (project, cwd) = workspace(&home);
    let stored = json!({
        "revision": "settings:1",
        "document": {
            "definitions": settings()["document"]["definitions"].clone(),
            "projectEnabled": {"/workspace": {"nightly-check": false}, "/other": {"keep": true}},
            "laterMember": {"kept": [1, 2, 3]},
        }
    });
    let daemon = home.daemon(move |wire| {
        hello(wire);
        // The name is checked against the owner's list before any write.
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, stored.clone());
        let request = wire.read("settings.update");
        assert_eq!(request["params"]["source"], "workflow");
        assert_eq!(request["params"]["scope"], "user");
        assert_eq!(request["params"]["expectedRevision"], "settings:1");
        let document = &request["params"]["document"];
        assert_eq!(document["projectEnabled"][&cwd]["nightly-check"], true);
        assert_eq!(
            document["projectEnabled"]["/workspace"],
            json!({"nightly-check": false})
        );
        assert_eq!(document["projectEnabled"]["/other"], json!({"keep": true}));
        assert_eq!(document["laterMember"], json!({"kept": [1, 2, 3]}));
        assert_eq!(document["definitions"], stored["document"]["definitions"]);
        wire.reply(
            &request,
            json!({"revision": "settings:2", "document": request["params"]["document"]}),
        );
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "enable", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Enabled nightly-check.\n"
    );
}

/// The toggle was written and only the read that reports the definitions
/// failed: the action's own line still stands, and the read error goes to
/// stderr with exit 0.
#[test]
fn workflow_enable_reports_its_line_when_the_read_after_it_fails() {
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        let request = wire.read("settings.update");
        wire.reply(
            &request,
            json!({"revision": "settings:2", "document": request["params"]["document"]}),
        );
        let request = wire.read("workflow.list");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,"message":"the definitions are unavailable"}}));
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "enable", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Enabled nightly-check.\n"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("the definitions are unavailable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A conflict whose re-read fails is still a conflict: the settings changed
/// and nothing was written, so the command says so and why it cannot show
/// the area.
#[test]
fn workflow_a_conflict_whose_reread_fails_is_still_a_conflict() {
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        let request = wire.read("settings.update");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,"message":"settings changed","data":{"appError":"settings_conflict"}}}));
        // The re-read the conflict reports fails too.
        let request = wire.read("workflow.list");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,"message":"the definitions are unavailable"}}));
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "enable", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("The definitions changed since they were read; nothing was written."),
        "{stderr}"
    );
    assert!(
        stderr.contains("the definitions are unavailable"),
        "{stderr}"
    );
}

#[test]
fn workflow_enable_refuses_a_name_the_owner_does_not_list_as_a_project() {
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        // Nothing was written: no settings.read follows.
        drains_without_saving(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "enable", "review-and-fix"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("No project definition is named review-and-fix."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A project definition starts disabled, and its bare name still names it:
/// `edit` and `set` answer with the file to edit, and `history` reads its
/// detail instead of refusing the name.
#[test]
fn workflow_edit_set_and_history_find_a_disabled_project_definition() {
    // `edit`: the editor is never opened, since there is no document to give
    // it; the answer names the definition's file.
    let home = Home::new();
    let (project, _) = workspace(&home);
    let script = editor(&home, "exit 0");
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let mut command = home.command();
    command
        .env_remove("VISUAL")
        .env("EDITOR", &script)
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "edit", "nightly-check"]);
    let mut terminal = Terminal::launch_tui(command);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(1));
    assert!(
        terminal
            .transcript
            .contains("A project definition is a file; edit ")
            && terminal.transcript.contains("nightly-check.yaml"),
        "{}",
        terminal.transcript
    );

    // `set`: the same resolution, before the text it would write matters.
    let home = Home::new();
    let (project, _) = workspace(&home);
    let text = home.0.join("nightly.json");
    fs::write(
        &text,
        json!({"schema": "dim.workflow/v1", "name": "nightly-check"}).to_string(),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains_without_saving(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "set", "nightly-check"])
        .arg(&text)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("A project definition is a file; edit "),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // `history`: the detail is the definition's lines and its runs.
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.status");
        assert_eq!(request["params"], json!({}));
        wire.reply(&request, json!([]));
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "history", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Nightly checks"), "{stdout}");
    assert!(stdout.contains("nightly-check.yaml"), "{stdout}");
    assert!(
        stdout.contains("Starts only after you enable it."),
        "{stdout}"
    );
    assert!(stdout.contains("No run yet."), "{stdout}");
}

/// One retained run as `workflow.status` reports it.
fn run(run_id: &str, name: &str, source: &str, cwd: &str) -> Value {
    json!({
        "run_id": run_id, "name": name, "source": source, "cwd": cwd,
        "state": "completed",
        "created_at": "2026-11-01T09:00:00.000Z",
        "ended_at": "2026-11-01T09:01:00.000Z",
    })
}

/// A project definition's history is this workspace's runs: another
/// workspace's definition of the same name is a different definition. A user
/// definition is the same everywhere, so all its runs are listed, each with
/// the workspace it started in.
#[test]
fn workflow_history_scopes_a_project_definition_to_this_workspace() {
    // The project definition: this workspace's run, not another's.
    let home = Home::new();
    let (project, cwd) = workspace(&home);
    let runs = json!([
        run("wf-2", "nightly-check", "project", &cwd),
        run("wf-1", "nightly-check", "project", "/other"),
    ]);
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.status");
        wire.reply(&request, runs);
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "history", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("wf-2"), "{stdout}");
    assert!(!stdout.contains("wf-1"), "{stdout}");

    // The user definition: another workspace's run is listed with its cwd.
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.status");
        wire.reply(
            &request,
            json!([run("wf-3", "review-and-fix", "user", "/other")]),
        );
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "history", "review-and-fix"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("wf-3"), "{stdout}");
    assert!(stdout.contains("/other"), "{stdout}");
}

/// A save may not rename its definition: the owner keys by the document's
/// name, and the area has no remove. Nothing is written and the text stays.
#[test]
fn workflow_set_refuses_a_rename_before_the_save() {
    let home = Home::new();
    let (project, _) = workspace(&home);
    let text = home.0.join("renamed.json");
    fs::write(
        &text,
        json!({
            "schema": "dim.workflow/v1",
            "name": "renamed",
            "description": "Changed.",
            "steps": [{"stage": {"id": "review", "prompt": "Review."}}]
        })
        .to_string(),
    )
    .unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains_without_saving(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .arg("workflow")
        .arg("set")
        .arg("review-and-fix")
        .arg(&text)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("A definition's name is its key; add it to save it under another name."),
        "{stderr}"
    );
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
}

#[test]
fn workflow_add_is_refused_before_stdin_is_read() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["workflow.list"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut child = home
        .command()
        .args(["workflow", "add", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The write end stays open and nothing is written: a read would wait.
    let _stdin = child.stdin.take();
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon lists workflow definitions but cannot add them."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `start NAME -` sends the stdin parameters, resolved to the entry's source,
/// and prints the run the owner started.
#[test]
fn workflow_start_sends_the_parameters_from_stdin() {
    let home = Home::new();
    let (project, cwd) = workspace(&home);
    let daemon = home.daemon(move |wire| {
        wire.hello(&["workflow.list", "workflow.start", "settings.read"]);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.start");
        assert_eq!(
            request["params"],
            json!({
                "name": "review-and-fix",
                "source": "user",
                "cwd": cwd,
                "params": {"target": "src"},
            })
        );
        wire.reply(&request, json!({"run_id": "wf-1"}));
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let mut child = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "start", "review-and-fix", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"target\":\"src\"}")
        .unwrap();
    let output = wait(child);
    assert!(
        output.status.success(),
        "exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Started review-and-fix: wf-1.\n"
    );
    daemon.join().unwrap();
}

/// An explicit parameter document is sent as given: a value equal to the
/// listing's cut preview is still the caller's value. The omission of an
/// unchanged default belongs to the panel's editor, which knows the document
/// it opened with.
#[test]
fn workflow_start_sends_an_explicit_document_unchanged() {
    let cut = "a long default the listing cut …";
    for from_stdin in [true, false] {
        let home = Home::new();
        let (project, cwd) = workspace(&home);
        let daemon = home.daemon(move |wire| {
            wire.hello(&["workflow.list", "workflow.start", "settings.read"]);
            let request = wire.read("workflow.list");
            wire.reply(&request, cut_default());
            let request = wire.read("workflow.start");
            assert_eq!(
                request["params"],
                json!({
                    "name": "review-and-fix",
                    "source": "user",
                    "cwd": cwd,
                    "params": {"target": cut},
                }),
                "the explicit document is sent as given"
            );
            wire.reply(&request, json!({"run_id": "wf-1"}));
            let request = wire.read("workflow.list");
            wire.reply(&request, cut_default());
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            drains(wire);
        });
        let document = json!({"target": cut}).to_string();
        let mut command = home.command();
        command.arg("--cwd").arg(&project);
        command.args(["workflow", "start", "review-and-fix"]);
        let child = if from_stdin {
            command.arg("-");
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
                .write_all(document.as_bytes())
                .unwrap();
            child
        } else {
            let path = home.0.join("parameters.json");
            fs::write(&path, &document).unwrap();
            command
                .arg(&path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let output = wait(child);
        assert!(
            output.status.success(),
            "from stdin {from_stdin}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "Started review-and-fix: wf-1.\n"
        );
        daemon.join().unwrap();
    }
}

/// An unknown name is refused before the start, and a parameter document the
/// definition does not describe is refused before it too.
#[test]
fn workflow_start_refuses_an_unknown_name_or_parameters() {
    // The unknown name: the list is read, and nothing is started.
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        wire.hello(&["workflow.list", "workflow.start"]);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        drains_without_starting(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "start", "missing"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("No workflow definition is named missing."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // An unknown parameter key is the client's own document shape, refused
    // with the editor's shape sentence and no start.
    let home = Home::new();
    let (project, _) = workspace(&home);
    let daemon = home.daemon(|wire| {
        wire.hello(&["workflow.list", "workflow.start"]);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        drains_without_starting(wire);
    });
    let mut child = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "start", "review-and-fix", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"target\":\"src\",\"extra\":\"x\"}")
        .unwrap();
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Parameters are a JSON object of strings."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A name the area lists only disabled reaches the owner, whose refusal
    // names the state; the client does not claim the name is unknown.
    let home = Home::new();
    let (project, cwd) = workspace(&home);
    let daemon = home.daemon(move |wire| {
        wire.hello(&["workflow.list", "workflow.start"]);
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.start");
        assert_eq!(
            request["params"],
            json!({
                "name": "nightly-check",
                "source": "project",
                "cwd": cwd,
                "params": {},
            })
        );
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,"message":"No enabled definition is named nightly-check.",
            "data":{"appError":"definition_not_found"}}}));
        drains(wire);
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["workflow", "start", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("No enabled definition is named nightly-check."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Without `settings.read` and `settings.update` the toggle is not offered,
/// so the command is refused before it reads anything.
#[test]
fn workflow_enable_is_refused_without_the_settings_methods() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["workflow.list", "workflow.save", "workflow.status"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["workflow", "enable", "nightly-check"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon lists workflow definitions but cannot toggle them."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const WRITES_THE_PROJECT: &str = "printf '%s' '{\"source\":\"project\",\"document\":{\"schema\":\"dim.workflow/v1\",\"name\":\"nightly-check\",\"description\":\"Night checks.\",\"steps\":[{\"stage\":{\"id\":\"check\",\"prompt\":\"Check.\"}}]}}' > \"$1\"";

const WRITES_THE_USER_EDIT: &str = "printf '%s' '{\"schema\":\"dim.workflow/v1\",\"name\":\"review-and-fix\",\"description\":\"Changed.\",\"parameters\":[{\"name\":\"target\",\"description\":\"Path to review\",\"required\":true}],\"steps\":[{\"stage\":{\"id\":\"review\",\"prompt\":\"Review {{params.target}}\"}}]}' > \"$1\"";

const WRITES_A_RENAME: &str = "printf '%s' '{\"schema\":\"dim.workflow/v1\",\"name\":\"renamed\",\"description\":\"Changed.\",\"steps\":[{\"stage\":{\"id\":\"review\",\"prompt\":\"Review.\"}}]}' > \"$1\"";

const WRITES_A_NEW_DEFINITION: &str = "printf '%s' '{\"source\":\"user\",\"document\":{\"schema\":\"dim.workflow/v1\",\"name\":\"example\",\"description\":\"Mine.\",\"steps\":[{\"stage\":{\"id\":\"one\",\"prompt\":\"One.\"}}]}}' > \"$1\"";

/// `/workflow` lists the definitions by source, and the selected entry's
/// detail and kinds, in both terminal modes.
#[test]
fn tui_workflow_panel_lists_definitions() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            assert_eq!(request["params"], json!({"cwd": "/workspace"}));
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            assert_eq!(
                request["params"],
                json!({"source": "workflow", "scope": "user"})
            );
            wire.reply(&request, settings());
            drains(wire);
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow\r");
        terminal.until("review-and-fix");
        terminal.until("plugin:kit");
        terminal.until("Analyze, fix in parallel, summarize");
        terminal.until("target — Path to review (required)");
        terminal.until("Saved in the user workflow source; always enabled.");
        terminal.until("Ctrl+E");
        terminal.raw(b"\x1b[B");
        terminal.until("Nightly checks");
        terminal.until("/workspace/.dimcode/workflows/nightly-check.yaml");
        terminal.until("Starts only after you enable it.");
        terminal.until("Stage build: invalid reference");
        terminal.until("and 11 more");
        terminal.until("toggle");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A short terminal keeps the composer, the selected entry and the key hint
/// visible with `/workflow` open, in both layouts.
#[test]
fn tui_workflow_short_screen_keeps_the_composer_and_hints() {
    for fullscreen in [false, true] {
        for (cols, rows) in [(32, 8), (40, 10)] {
            let home = Home::new();
            let (shown, wait_shown) = mpsc::channel();
            let daemon = home.daemon(move |wire| {
                opened(wire);
                wait_shown.recv_timeout(PATIENCE).unwrap();
                let request = wire.read("workflow.list");
                wire.reply(&request, listing(false));
                let request = wire.read("settings.read");
                wire.reply(&request, settings());
                drains(wire);
            });
            let mut command = home.command();
            sized(&mut command, cols, rows);
            if fullscreen {
                command.arg("--fullscreen");
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("Git branch");
            // A draft proves the composer is on the short screen; Ctrl+C
            // clears it before the command.
            terminal.write(b"xyzzy");
            terminal.until("xyzzy");
            terminal.raw(b"\x03");
            shown.send(()).unwrap();
            terminal.write(b"/workflow\r");
            terminal.until("review");
            // The bottom border's closing key, painted after the list.
            terminal.until("Esc");
            terminal.raw(b"\x1b");
            thread::sleep(Duration::from_millis(200));
            terminal.write(b"/exit\r");
            terminal.until("Bye!");
            assert!(terminal.wait().success());
            daemon.join().unwrap();
        }
    }
}

/// `/workflow:save` opens the create editor; the project document it writes
/// carries the workspace and no revision, and the note says it stays
/// disabled.
#[test]
fn tui_workflow_save_command_writes_a_definition() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, WRITES_THE_PROJECT);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            let request = wire.read("workflow.save");
            assert_eq!(request["params"]["source"], "project");
            assert_eq!(request["params"]["cwd"], "/workspace");
            assert_eq!(request["params"].get("expectedRevision"), None);
            assert_eq!(request["params"]["document"]["name"], "nightly-check");
            assert_eq!(
                request["params"]["document"]["description"],
                "Night checks."
            );
            wire.reply(&request, json!({"name": "nightly-check"}));
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            drains(wire);
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow:save\r");
        terminal.until("Saved nightly-check. It stays disabled until you enable it.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A completed management run as `workflow.status {}` reports it, addressed
/// to the Session whose panel started it.
fn started_run() -> Value {
    json!({
        "run_id": "wf-1",
        "name": "review-and-fix",
        "source": "user",
        "definition": {"version": "dim.workflow/v1"},
        "params": {"target": "src"},
        "cwd": "/workspace",
        "started_by": "management",
        "created_at": "2026-11-02T10:00:00.000Z",
        "ended_at": "2026-11-02T10:01:00.000Z",
        "state": "completed",
        "stages": [],
        "log": [],
        "report": {"state": "none"},
        "report_to": {"session_id": "session", "branch_id": "main"},
    })
}

/// Ctrl+T on a definition with parameters opens the editor on the template
/// and starts the run addressed to the Session's line; `/tasks` finds it
/// through the discovery read.
#[test]
fn tui_workflow_panel_starts_a_run_with_parameters() {
    for fullscreen in [false, true] {
        let home = Home::new();
        // The editor keeps the template it was opened on, then fills it.
        let template = home.0.join("template.json");
        let script = editor(
            &home,
            &format!(
                "cp \"$1\" {}\nprintf '%s' '{{\"target\":\"src\"}}' > \"$1\"",
                template.display()
            ),
        );
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened_with(
                wire,
                &[
                    "workflow.list",
                    "workflow.save",
                    "workflow.start",
                    "workflow.status",
                    "settings.read",
                    "settings.update",
                ],
            );
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            // Ctrl+T: the adapter resolves the entry, then starts it with the
            // parameters the editor left and the line the panel drew over.
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("workflow.start");
            assert_eq!(
                request["params"],
                json!({
                    "name": "review-and-fix",
                    "source": "user",
                    "cwd": "/workspace",
                    "params": {"target": "src"},
                    "report_to": {"session_id": "session", "branch_id": "main"},
                })
            );
            wire.reply(&request, json!({"run_id": "wf-1"}));
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            // `/tasks` discovers the management-started run.
            let request = wire.read("workflow.status");
            assert_eq!(request["params"], json!({}));
            wire.reply(&request, json!([started_run()]));
            drains(wire);
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow\r");
        terminal.until("review-and-fix");
        terminal.raw(b"\x14");
        terminal.until("Started review-and-fix: wf-1.");
        assert_eq!(
            fs::read_to_string(&template).unwrap(),
            "{\n  \"target\": \"\"\n}\n"
        );
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/tasks\r");
        terminal.until("review-and-fix");
        terminal.write(b"\r");
        terminal.until("Run: wf-1");
        // Back to the list, then out of the panel.
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// Two parameterized user definitions of the run-draft test: each declares
/// one parameter of its own, so the template says which one opened.
fn two_parameterized() -> Value {
    json!([
        {
            "name": "review-and-fix", "source": "user", "enabled": true,
            "description": "Analyze", "parameters": [
                {"name": "target", "description": "Path to review", "required": true}
            ],
            "valid": true, "issues": [], "issue_count": 0
        },
        {
            "name": "digest", "source": "user", "enabled": true,
            "description": "Digest", "parameters": [
                {"name": "path", "description": "Path to digest", "required": true}
            ],
            "valid": true, "issues": [], "issue_count": 0
        }
    ])
}

/// A definition whose `target` default is a cut preview, as the listing's
/// 1024-byte field bound leaves it, and whose `style` default fits.
fn cut_default() -> Value {
    json!({"definitions": [{
        "name": "review-and-fix", "source": "user", "enabled": true,
        "description": "Analyze", "parameters": [
            {"name": "target", "description": "Path to review", "default": "a long default the listing cut …"},
            {"name": "style", "description": "Writing style", "default": "concise"}
        ],
        "valid": true, "issues": [], "issue_count": 0
    }], "truncated": false})
}

/// A run the owner refused keeps its parameters for its own definition:
/// Ctrl+T on another definition opens that definition's template and starts
/// it, and the refused one still reopens its own kept text.
#[test]
fn tui_workflow_panel_keeps_a_runs_parameters_for_its_own_definition() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let log = home.0.join("opened.log");
        // The editor records the text it was opened on, then fills the
        // parameters of whichever template this is.
        let script = editor(
            &home,
            "printf '%s\\n===\\n' \"$(cat \"$1\")\" >> \"$RUN_LOG\"\n\
             case \"$(cat \"$1\")\" in\n\
               *'\"target\": \"\"'*) printf '%s' '{\"target\":\"kept\"}' > \"$1\" ;;\n\
               *'\"path\": \"\"'*) printf '%s' '{\"path\":\"docs\"}' > \"$1\" ;;\n\
             esac",
        );
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened_with(
                wire,
                &[
                    "workflow.list",
                    "workflow.save",
                    "workflow.start",
                    "workflow.status",
                    "settings.read",
                    "settings.update",
                ],
            );
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            wire.reply(
                &request,
                json!({"definitions": two_parameterized(), "truncated": false}),
            );
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            // Ctrl+T on review-and-fix: its parameters go out, and the owner
            // refuses the run, so the text stays.
            let request = wire.read("workflow.list");
            wire.reply(
                &request,
                json!({"definitions": two_parameterized(), "truncated": false}),
            );
            let request = wire.read("workflow.start");
            assert_eq!(request["params"]["name"], "review-and-fix");
            assert_eq!(request["params"]["params"], json!({"target": "kept"}));
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,"message":"the owner refused the run",
                "data":{"appError":"start_refused"}}}));
            // Ctrl+T on digest opens digest's own template and starts digest
            // with its own parameters, not the kept draft.
            let request = wire.read("workflow.list");
            wire.reply(
                &request,
                json!({"definitions": two_parameterized(), "truncated": false}),
            );
            let request = wire.read("workflow.start");
            assert_eq!(request["params"]["name"], "digest");
            assert_eq!(request["params"]["params"], json!({"path": "docs"}));
            wire.reply(&request, json!({"run_id": "wf-2"}));
            let request = wire.read("workflow.list");
            wire.reply(
                &request,
                json!({"definitions": two_parameterized(), "truncated": false}),
            );
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            // Back on review-and-fix, Ctrl+T reopens the text it kept.
            let request = wire.read("workflow.list");
            wire.reply(
                &request,
                json!({"definitions": two_parameterized(), "truncated": false}),
            );
            let request = wire.read("workflow.start");
            assert_eq!(request["params"]["name"], "review-and-fix");
            assert_eq!(request["params"]["params"], json!({"target": "kept"}));
            wire.reply(&request, json!({"run_id": "wf-3"}));
            let request = wire.read("workflow.list");
            wire.reply(
                &request,
                json!({"definitions": two_parameterized(), "truncated": false}),
            );
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            drains(wire);
        });
        let mut command = edit_command(&home, &script, fullscreen);
        command.env("RUN_LOG", &log);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow\r");
        terminal.until("review-and-fix");
        terminal.raw(b"\x14");
        terminal.until("the owner refused the run");
        terminal.raw(b"\x1b[B");
        terminal.until("Digest");
        terminal.raw(b"\x14");
        terminal.until("Started digest: wf-2.");
        terminal.raw(b"\x1b[A");
        terminal.until("Analyze");
        terminal.raw(b"\x14");
        terminal.until("Started review-and-fix: wf-3.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        let opened = fs::read_to_string(&log).unwrap();
        let entries: Vec<&str> = opened
            .split("\n===\n")
            .filter(|entry| !entry.trim().is_empty())
            .collect();
        assert_eq!(entries.len(), 3, "{opened}");
        assert!(entries[0].contains("\"target\": \"\""), "{opened}");
        assert!(entries[1].contains("\"path\": \"\""), "{opened}");
        assert_eq!(entries[2].trim(), "{\"target\":\"kept\"}", "{opened}");
    }
}

/// A parameter left at the default the listing gave is not sent: the listed
/// default may be a cut preview, so the owner applies the definition's own
/// default to a parameter the request leaves out. A value the user changed
/// is sent as typed.
#[test]
fn tui_workflow_run_leaves_a_listed_default_unsent() {
    let home = Home::new();
    // The user keeps `target` as the template filled it and changes `style`.
    let script = editor(
        &home,
        "sed 's/\"concise\"/\"verbose\"/' \"$1\" > \"$1.new\" && mv \"$1.new\" \"$1\"",
    );
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with(
            wire,
            &[
                "workflow.list",
                "workflow.save",
                "workflow.start",
                "workflow.status",
                "settings.read",
                "settings.update",
            ],
        );
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("workflow.list");
        wire.reply(&request, cut_default());
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        // Ctrl+T reads the definitions again, then starts: `target` is left
        // at its listed default and sent by nobody; `style` was changed.
        let request = wire.read("workflow.list");
        wire.reply(&request, cut_default());
        let request = wire.read("workflow.start");
        assert_eq!(request["params"]["name"], "review-and-fix");
        assert_eq!(
            request["params"]["params"],
            json!({"style": "verbose"}),
            "{request}"
        );
        wire.reply(&request, json!({"run_id": "wf-1"}));
        let request = wire.read("workflow.list");
        wire.reply(&request, cut_default());
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, false));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/workflow\r");
    terminal.until("review-and-fix");
    terminal.raw(b"\x14");
    terminal.until("Started review-and-fix: wf-1.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A run started from the panel reports to the line the panel shows: the
/// user focused `line-1` before they opened `/workflow`, so the start is
/// addressed to it, not to `main`.
#[test]
fn tui_workflow_panel_starts_a_run_on_the_shown_line() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with(
            wire,
            &[
                "workflow.list",
                "workflow.save",
                "workflow.start",
                "workflow.status",
                "settings.read",
                "settings.update",
            ],
        );
        // A line opens from main; focusing it reads the line's own history.
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main","event":{
                "id":"branch-9","sequence":9,"create_at":1,"type":"branch","name":"create",
                "data":{"branch":"line-1","at":{"kind":"create"},
                        "source":{"kind":"host","operation_id":"op-1"},
                        "prompt":"child work"}}}),
        );
        let history = wire.read("session.history");
        assert_eq!(history["params"]["branchId"], "line-1");
        wire.reply(
            &history,
            json!({"events":[{"id":"start-line-1","sequence":10,"create_at":1,
                    "type":"branch","name":"start","data":{"parent":"main"}}],
                "through":10,"nextAfter":10,"done":true}),
        );
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        // Ctrl+T on release-notes, which declares no parameters, starts at
        // once, addressed to the line the transcript shows.
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.start");
        assert_eq!(
            request["params"],
            json!({
                "name": "release-notes",
                "source": "plugin",
                "cwd": "/workspace",
                "params": {},
                "report_to": {"session_id": "session", "branch_id": "line-1"},
            })
        );
        wire.reply(&request, json!({"run_id": "wf-2"}));
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    // The branch row may be painted before the frame that `test / model`
    // matched, so the row is looked for anywhere in the transcript.
    terminal.until_anywhere("line-1");
    terminal.write(b"/lines\r");
    terminal.until("the session itself");
    terminal.write(b"\x1b[B");
    terminal.write(b"\r");
    terminal.until("⑂ main ▸ child work");
    terminal.write(b"/workflow\r");
    terminal.until("review-and-fix");
    terminal.raw(b"\x1b[B");
    terminal.until("Nightly checks");
    terminal.raw(b"\x1b[B");
    terminal.until("Release notes");
    terminal.raw(b"\x14");
    terminal.until("Started release-notes: wf-2.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// `/workflow` after `/new`, before the first message, has no Session and
/// so no line to report to: the start carries no `report_to`.
#[test]
fn tui_workflow_starts_without_a_session_and_reports_nowhere() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with(
            wire,
            &[
                "workflow.list",
                "workflow.save",
                "workflow.start",
                "workflow.status",
                "settings.read",
                "settings.update",
            ],
        );
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("workflow.start");
        assert_eq!(
            request["params"],
            json!({
                "name": "release-notes",
                "source": "plugin",
                "cwd": "/workspace",
                "params": {},
            })
        );
        wire.reply(&request, json!({"run_id": "wf-3"}));
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/new\r");
    terminal.until("─ new session ─");
    terminal.write(b"/workflow\r");
    terminal.until("review-and-fix");
    terminal.raw(b"\x1b[B");
    terminal.until("Nightly checks");
    terminal.raw(b"\x1b[B");
    terminal.until("Release notes");
    terminal.raw(b"\x14");
    terminal.until("Started release-notes: wf-3.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A stale revision writes nothing: the toggle and the edit both read the
/// area again and say so, and the edit keeps the user's text.
#[test]
fn tui_workflow_panel_reports_a_conflict_and_rereads() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, WRITES_THE_USER_EDIT);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            // Enter on the project entry: the name is checked against the
            // list, then the document is read immediately before the write.
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            let request = wire.read("settings.update");
            assert_eq!(request["params"]["expectedRevision"], "settings:1");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,"message":"settings changed","data":{"appError":"settings_conflict"}}}));
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            // Ctrl+E on the user definition: the save carries the revision
            // the panel read, and a conflict reads the area again.
            let request = wire.read("workflow.save");
            assert_eq!(request["params"]["source"], "user");
            assert_eq!(request["params"]["expectedRevision"], "settings:1");
            assert_eq!(request["params"]["document"]["description"], "Changed.");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,"message":"conflict","data":{"appError":"conflict"}}}));
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            drains(wire);
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow\r");
        terminal.until("review-and-fix");
        terminal.raw(b"\x1b[B");
        terminal.until("Nightly checks");
        terminal.raw(b"\r");
        terminal.until("The definitions changed since they were read; nothing was written.");
        terminal.raw(b"\x1b[A");
        terminal.until("Analyze, fix in parallel, summarize");
        terminal.raw(b"\x05");
        terminal.until("The definition changed since it was read; nothing was written.");
        terminal.until("Your text is kept in");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// The panel refuses a rename before it writes: the owner keys by the
/// document's name, so nothing is sent and the text stays.
#[test]
fn tui_workflow_panel_refuses_a_rename_and_keeps_the_draft() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, WRITES_A_RENAME);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            drains_without_saving(wire);
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow\r");
        terminal.until("review-and-fix");
        terminal.raw(b"\x05");
        terminal.until("A definition's name is its key");
        terminal.until("Your text is kept in");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A daemon without `workflow.save` cannot serve `/workflow:save`: the
/// panel stays on its list with the support check's sentence.
#[test]
fn tui_workflow_save_is_refused_when_the_daemon_cannot_write() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with(
            wire,
            &[
                "workflow.list",
                "workflow.status",
                "settings.read",
                "settings.update",
            ],
        );
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        let request = wire.read("settings.read");
        wire.reply(&request, settings());
        drains_without_saving(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/workflow:save\r");
    terminal.until("This daemon lists workflow definitions but cannot add them.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A save the owner refuses shows its issues and keeps the draft, which the
/// next Ctrl+N reopens as the user's text, not the create template.
#[test]
fn tui_workflow_save_reports_an_invalid_definition_and_keeps_the_draft() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, WRITES_A_NEW_DEFINITION);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            opened(wire);
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("workflow.list");
            wire.reply(&request, listing(false));
            let request = wire.read("settings.read");
            wire.reply(&request, settings());
            let first = wire.read("workflow.save");
            assert_eq!(first["params"]["source"], "user");
            assert_eq!(first["params"]["document"]["description"], "Mine.");
            wire.send(json!({"jsonrpc":"2.0","id":first["id"],"error":{
                "code":-32000,"message":"Stage build: invalid reference {{params.branch}}","data":{"appError":"invalid_definition"}}}));
            // Ctrl+N reopens the user's text, so the next save carries it
            // again rather than the create template.
            let second = wire.read("workflow.save");
            assert_eq!(second["params"]["document"], first["params"]["document"]);
            wire.send(json!({"jsonrpc":"2.0","id":second["id"],"error":{
                "code":-32000,"message":"Stage build: invalid reference {{params.branch}}","data":{"appError":"invalid_definition"}}}));
            drains_without_saving(wire);
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/workflow:save\r");
        terminal.until("Stage build: invalid reference");
        terminal.until("Your text is kept in");
        terminal.raw(b"\x0e");
        terminal.until("Stage build: invalid reference");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// Without `settings.read` and `settings.update` the panel offers no toggle
/// and no edit: the view carries neither kind, and the hint says so.
#[test]
fn tui_workflow_panel_offers_no_write_without_the_settings_methods() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with(wire, &["workflow.list", "workflow.save", "workflow.status"]);
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("workflow.list");
        wire.reply(&request, listing(false));
        // No settings.read: the daemon does not advertise one.
        drains(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/workflow\r");
    terminal.until("review-and-fix");
    terminal.until("Ctrl+N");
    assert!(
        !terminal.transcript.contains("toggle"),
        "no toggle without settings.update: {}",
        terminal.transcript
    );
    assert!(
        !terminal.transcript.contains("Ctrl+E"),
        "no edit without settings.read: {}",
        terminal.transcript
    );
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// The real owner: the client's `projectEnabled` write, an add into each
/// source, an edit at the revision the read reported, and `history` against
/// the runs the owner keeps. Listing the workspace's MCP entries authorizes
/// it with Settings, as the product's management paths do; no model is used.
#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn workflow_commands_write_and_enable_against_the_real_owner() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let socket = home.0.join("rust/daemon.sock");
    // The installed layout, from the checkout: the product home holds the
    // plugin files, the defaults name the shipped plugins, and the runtimes
    // are the system Node and the checkout's runtime packages
    // (`crates/dim-app-server/README.md`, Plugins). `runtime/node_modules`
    // holds the packages.
    let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime");
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("node"))
        .find(|path| path.is_file())
        .expect("node on PATH");
    let log = fs::File::create(home.0.join("daemon.log")).unwrap();
    let mut daemon = Command::new(&binary)
        .arg("--unix")
        .arg(&socket)
        .arg("--no-ws")
        .arg("--home")
        .arg(home.0.join("rust"))
        .arg("--product-home")
        .arg(&home.0)
        .arg("--defaults")
        .arg(runtime.join("default-plugins.json"))
        .arg("--node")
        .arg(node)
        .arg("--js-runtime")
        .arg(runtime.join("node_modules/@arcships/rutis-runtime"))
        .current_dir(&project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let _daemon = OwnedDaemon(socket.clone());
    let deadline = Instant::now() + Duration::from_secs(60);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon did not listen:\n{}",
            fs::read_to_string(home.0.join("daemon.log")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(50));
    }
    // Default plugins start after the daemon does; Workflow's methods are
    // advertised once it runs.
    let advertised = || -> Option<bool> {
        let stream = UnixStream::connect(&socket).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut stream = BufReader::new(stream);
        writeln!(
            stream.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"hello",
                   "params":{"protocolVersion":3,"clientKind":"cli"}})
        )
        .ok()?;
        let mut line = String::new();
        stream.read_line(&mut line).ok()?;
        let reply: Value = serde_json::from_str(&line).ok()?;
        Some(
            reply["result"]["features"]
                .as_array()
                .is_some_and(|features| features.iter().any(|feature| feature == "workflow.list")),
        )
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if advertised() == Some(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Workflow's methods were not advertised:\n{}",
            fs::read_to_string(home.0.join("daemon.log")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(100));
    }
    let run = |args: &[&str], stdin: Option<&str>| {
        let mut command = home.command();
        command
            .arg("--server")
            .arg(&binary)
            .arg("--cwd")
            .arg(&project)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        wait(child)
    };
    let list = || {
        let output = run(&["workflow", "list"], None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    // The workspace is not registered yet, so the first read fails with the
    // owner's message.
    let unregistered = run(&["workflow", "list"], None);
    assert_eq!(unregistered.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&unregistered.stderr).contains("not registered with Settings"),
        "{}",
        String::from_utf8_lossy(&unregistered.stderr)
    );
    // Registration goes through a product path, not one added for this
    // test: `mcp.catalog` authorizes the workspace it reads and registers
    // it with Settings (`mcp/management.rs::project`). `session.create`
    // cannot be used: it needs a model catalog, the product-home mode that
    // provides the plugin files refuses `--model-config`, and the OSS
    // binary injects no managed provider.
    let registered = run(&["mcp", "list"], None);
    assert!(
        registered.status.success(),
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );
    // A project definition is written to the repository and stays disabled.
    let project_document = json!({
        "source": "project",
        "document": {
            "schema": "dim.workflow/v1",
            "name": "nightly-check",
            "description": "Nightly checks.",
            "steps": [{"stage": {"id": "check", "prompt": "Check the workspace."}}]
        }
    })
    .to_string();
    let added = run(&["workflow", "add", "-"], Some(&project_document));
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert_eq!(
        String::from_utf8(added.stdout).unwrap(),
        "Saved nightly-check. It stays disabled until you enable it.\n"
    );
    assert!(
        list().contains("nightly-check\tproject\tdisabled\tNightly checks."),
        "{}",
        list()
    );
    let enabled = run(&["workflow", "enable", "nightly-check"], None);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        String::from_utf8(enabled.stdout).unwrap(),
        "Enabled nightly-check.\n"
    );
    assert!(
        list().contains("nightly-check\tproject\tenabled\tNightly checks."),
        "{}",
        list()
    );
    // A user definition is saved to the user source and edited at the
    // revision the read reported.
    let user_document = json!({
        "document": {
            "schema": "dim.workflow/v1",
            "name": "review-and-fix",
            "description": "First description.",
            "steps": [{"stage": {"id": "review", "prompt": "Review."}}]
        }
    })
    .to_string();
    let added = run(&["workflow", "add", "-"], Some(&user_document));
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert_eq!(
        String::from_utf8(added.stdout).unwrap(),
        "Saved review-and-fix.\n"
    );
    let edited = json!({
        "schema": "dim.workflow/v1",
        "name": "review-and-fix",
        "description": "Second description.",
        "steps": [{"stage": {"id": "review", "prompt": "Review."}}]
    })
    .to_string();
    let set = run(&["workflow", "set", "review-and-fix", "-"], Some(&edited));
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert_eq!(
        String::from_utf8(set.stdout).unwrap(),
        "Saved review-and-fix.\n"
    );
    let listed = list();
    assert!(
        listed.contains("review-and-fix\tuser\tenabled\tSecond description."),
        "{listed}"
    );
    // The user save kept the enablement the toggle wrote.
    assert!(
        listed.contains("nightly-check\tproject\tenabled\tNightly checks."),
        "{listed}"
    );
    // `history` resolves the name by the precedence `start` uses and reads
    // the owner's retained runs.
    let history = run(&["workflow", "history", "nightly-check"], None);
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
    let history = String::from_utf8(history.stdout).unwrap();
    assert!(history.contains("Nightly checks."), "{history}");
    assert!(history.contains("Recent runs"), "{history}");
    assert!(history.contains("No run yet."), "{history}");
    drop(_daemon);
    let _ = daemon.wait();
}
