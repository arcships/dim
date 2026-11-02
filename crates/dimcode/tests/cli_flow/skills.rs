//! `dimcode skills list|enable|disable|reload` and the `/skills` panel: one
//! management area through its adapter, over the public wire.
use super::*;
use std::sync::mpsc;

fn hello(wire: &mut Wire) {
    wire.hello(&["skills.catalog", "skills.set_enabled", "skills.refresh"]);
}

fn catalog(enabled: bool) -> Value {
    json!({
        "version": 4,
        "entries": [
            {
                "id": "user:commit",
                "source": "user",
                "name": "commit",
                "description": "Writes commit messages",
                "filePath": "/w/.agents/skills/commit/SKILL.md",
                "allowedTools": [],
                "enabled": enabled,
                "shadowedBy": null,
            },
            {
                "id": "workspace:/w/.agents/skills:deploy",
                "source": "project",
                "name": "deploy",
                "description": "Deploys the project",
                "filePath": "/w/.agents/skills/deploy/SKILL.md",
                "allowedTools": [],
                "enabled": false,
                "shadowedBy": "commit",
            },
        ],
        "issues": [{"path": "/w/.agents/skills/old/SKILL.md", "message": "no name"}],
    })
}

/// The catalog a daemon serves before each entry reports its source: the
/// same reply in every other field, as `skills.catalog` answered it.
fn older_catalog(enabled: bool) -> Value {
    let mut catalog = catalog(enabled);
    for entry in catalog["entries"].as_array_mut().unwrap() {
        entry.as_object_mut().unwrap().remove("source");
    }
    catalog
}

#[test]
fn skills_list_from_a_daemon_without_sources_prints_a_dash() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let expected = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("skills.catalog");
        assert_eq!(request["params"]["cwd"], expected);
        wire.reply(&request, older_catalog(true));
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["skills", "list"])
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
        "commit\t-\tenabled\t/w/.agents/skills/commit/SKILL.md\tWrites commit messages\n\
         deploy\t-\tdisabled, shadowed by commit\t/w/.agents/skills/deploy/SKILL.md\tDeploys the project\n"
    );
}

/// A daemon that reports no source is still toggled: what the user asked for
/// is the change, and the read still names the entry.
#[test]
fn skills_enable_from_a_daemon_without_sources_reports_the_entry() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let expected = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("skills.set_enabled");
        assert_eq!(
            request["params"],
            json!({"cwd": expected, "id": "user:commit", "enabled": true})
        );
        wire.reply(&request, json!({"version": 5}));
        let request = wire.read("skills.catalog");
        wire.reply(&request, older_catalog(true));
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["skills", "enable", "user:commit"])
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
        "Enabled commit.\n"
    );
}

/// A toggle the daemon accepted is done even when the read after it fails:
/// the line names the id the user gave, since the read is what supplies the
/// name, and the command exits 0.
#[test]
fn skills_enable_reports_the_toggle_when_the_read_after_it_fails() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let expected = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("skills.set_enabled");
        assert_eq!(
            request["params"],
            json!({"cwd": expected, "id": "user:commit", "enabled": true})
        );
        wire.reply(&request, json!({"version": 5}));
        let request = wire.read("skills.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"catalog unavailable",
            "data":{"appError":"skills_unavailable"}}}));
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["skills", "enable", "user:commit"])
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
        "Enabled user:commit.\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("catalog unavailable"), "{stderr}");
}

/// A reload the daemon accepted is done even when the read after it fails:
/// the owner rescanned the roots, so the command says so and exits 0.
#[test]
fn skills_reload_reports_the_rescan_when_the_read_after_it_fails() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let expected = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("skills.refresh");
        assert_eq!(request["params"]["cwd"], expected);
        wire.reply(&request, json!({"version": 6, "issues": []}));
        let request = wire.read("skills.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"catalog unavailable",
            "data":{"appError":"skills_unavailable"}}}));
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["skills", "reload"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Reloaded.\n");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("catalog unavailable"), "{stderr}");
}

#[test]
fn skills_list_prints_every_source_and_the_issues() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let workspace = fs::canonicalize(&project).unwrap();
    let expected = workspace.to_string_lossy().into_owned();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("skills.catalog");
        assert_eq!(request["params"]["cwd"], expected);
        wire.reply(&request, catalog(true));
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["skills", "list"])
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
        "commit\tuser\tenabled\t/w/.agents/skills/commit/SKILL.md\tWrites commit messages\n\
         deploy\tproject\tdisabled, shadowed by commit\t/w/.agents/skills/deploy/SKILL.md\tDeploys the project\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Columns: name, source, state, file, description.\n\
         /w/.agents/skills/old/SKILL.md: no name\n"
    );
}

#[test]
fn skills_enable_and_disable_name_the_entry_and_send_the_workspace() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let workspace = fs::canonicalize(&project)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for enabled in [true, false] {
        let expected = workspace.clone();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("skills.set_enabled");
            assert_eq!(
                request["params"],
                json!({"cwd": expected, "id": "user:commit", "enabled": enabled})
            );
            wire.reply(&request, json!({"version": 5}));
            let request = wire.read("skills.catalog");
            wire.reply(&request, catalog(enabled));
        });
        let output = home
            .command()
            .arg("--cwd")
            .arg(&project)
            .args([
                "skills",
                if enabled { "enable" } else { "disable" },
                "user:commit",
            ])
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
            format!("{} commit.\n", if enabled { "Enabled" } else { "Disabled" })
        );
        fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    }
}

#[test]
fn skills_reload_rescans_the_workspace() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("skills.refresh");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(
            &request,
            json!({"version": 6, "issues": [{"path": "/w/old", "message": "unreadable"}]}),
        );
        let request = wire.read("skills.catalog");
        wire.reply(
            &request,
            json!({
                "version": 6,
                "entries": [],
                "issues": [{"path": "/w/old", "message": "unreadable"}],
            }),
        );
    });
    let output = home
        .command()
        .arg("--cwd")
        .arg(&project)
        .args(["skills", "reload"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "No Skill is configured.\n/w/old: unreadable\n"
    );
}

#[test]
fn skills_commands_refuse_a_daemon_that_does_not_advertise_them() {
    for args in [vec!["skills", "list"], vec!["skills", "reload"]] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let output = home.command().args(&args).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("This daemon cannot manage skills."),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A daemon that advertises the catalog alone lists Skills and refuses to
/// change one: the adapter takes only the methods it was given.
#[test]
fn a_daemon_without_skill_enablement_lists_but_refuses_a_change() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["skills.catalog"]);
        let request = wire.read("skills.catalog");
        wire.reply(&request, catalog(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home.command().args(["skills", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("commit\tuser\tenabled"), "{stdout}");
    fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["skills.catalog"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["skills", "disable", "user:commit"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("This daemon cannot manage skills."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Ctrl+R after a read that failed reads the area again: a daemon that
/// serves only `skills.catalog` answers the retry, and the panel never asks
/// for a rescan.
#[test]
fn tui_skills_panel_reads_again_after_a_failed_read() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["skills.catalog"]);
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
        let request = wire.read("skills.catalog");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"catalog unavailable",
            "data":{"appError":"skills_unavailable"}}}));
        let request = wire.read("skills.catalog");
        wire.reply(&request, catalog(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/skills\r");
    terminal.until("catalog unavailable");
    terminal.raw(b"\x12");
    terminal.until("Writes commit messages");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// `/skills` opens the panel on the catalog and Enter toggles the selected
/// entry; `/reload-skills` rescans without a panel and reports what it found.
#[test]
fn tui_skills_panel_lists_toggles_and_reloads() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
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
        let request = wire.read("skills.catalog");
        // The panel works on the Session's workspace, not the directory the
        // client started in.
        assert_eq!(request["params"]["cwd"], "/workspace");
        wire.reply(&request, catalog(true));
        let request = wire.read("skills.set_enabled");
        assert_eq!(
            request["params"]["id"], "user:commit",
            "the first entry is selected"
        );
        assert_eq!(request["params"]["cwd"], "/workspace");
        assert_eq!(request["params"]["enabled"], false);
        wire.reply(&request, json!({"version": 5}));
        let request = wire.read("skills.catalog");
        wire.reply(&request, catalog(false));
        let request = wire.read("skills.refresh");
        assert_eq!(request["params"]["cwd"], "/workspace");
        wire.reply(
            &request,
            json!({"version": 6, "issues": [{"path": "/w/old", "message": "unreadable"}]}),
        );
        let request = wire.read("skills.catalog");
        wire.reply(
            &request,
            json!({
                "version": 6,
                "entries": [],
                "issues": [{"path": "/w/old", "message": "unreadable"}],
            }),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/skills\r");
    terminal.until("/w/.agents/skills/old/SKILL.md: no name");
    terminal.until("Writes commit messages");
    terminal.raw(b"\r");
    terminal.until("Disabled commit.");
    terminal.raw(b"\x1b");
    // Esc alone stands: a byte arriving within the escape window would read
    // as its modifier.
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/reload-skills\r");
    terminal.until("Reloaded Skills: 0 entries, 0 disabled.");
    terminal.until("/w/old: unreadable");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// `/skills` lists the entries of a daemon that reports no source under the
/// area's own heading, and Enter still toggles one, in both terminal modes.
#[test]
fn tui_skills_panel_lists_unsourced_entries_under_the_area_heading() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = mpsc::channel();
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
            let request = wire.read("skills.catalog");
            wire.reply(&request, older_catalog(true));
            let request = wire.read("skills.set_enabled");
            assert_eq!(request["params"]["id"], "user:commit");
            assert_eq!(request["params"]["enabled"], false);
            wire.reply(&request, json!({"version": 5}));
            let request = wire.read("skills.catalog");
            wire.reply(&request, older_catalog(false));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/skills\r");
        terminal.until("Writes commit messages");
        let opened = terminal.transcript.clone();
        assert!(
            opened.contains("  skills"),
            "the entries are listed under the area's own heading: {opened}"
        );
        terminal.raw(b"\r");
        terminal.until("Disabled commit.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// An empty area still rescans: reload belongs to the area, so Ctrl+R works
/// with nothing to select.
#[test]
fn tui_skills_panel_reloads_an_empty_catalog() {
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
        let request = wire.read("skills.catalog");
        wire.reply(&request, json!({"version": 1, "entries": [], "issues": []}));
        let request = wire.read("skills.refresh");
        assert_eq!(request["params"]["cwd"], "/workspace");
        wire.reply(&request, json!({"version": 2, "issues": []}));
        let request = wire.read("skills.catalog");
        wire.reply(
            &request,
            json!({
                "version": 2,
                "entries": [{
                    "id": "user:commit",
                    "source": "user",
                    "name": "commit",
                    "description": "Writes commit messages",
                    "filePath": "/w/.agents/skills/commit/SKILL.md",
                    "allowedTools": [],
                    "enabled": true,
                    "shadowedBy": null,
                }],
                "issues": [],
            }),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/skills\r");
    terminal.until("No entries.");
    terminal.raw(b"\x12");
    terminal.until("Writes commit messages");
    terminal.until("Reloaded.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A daemon without the Skills methods leaves the panel open on the
/// unsupported-daemon error rather than an empty list.
#[test]
fn tui_skills_panel_says_when_the_daemon_cannot_manage_them() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
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
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/skills\r");
    terminal.until("This daemon cannot manage skills.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}
