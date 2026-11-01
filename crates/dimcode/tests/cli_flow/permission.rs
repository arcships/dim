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
fn line_permission_controls_preserve_other_fields_retry_conflicts_and_surface_old_daemon_errors() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&PRESET_METHODS);
        wire.created();
        let mut current = policy("read-only", &["read"], &["blocked"]);
        current["fileAccess"] = json!("workspace-read-write");
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":1,"policy":current}));
        wire.attached();
        for (version, fail_conflict) in [(1, true), (2, false)] {
            let read = wire.read("session.permission.get");
            wire.reply(&read, json!({"version":version,"policy":current}));
            let write = wire.read("session.permission.update");
            let mut expected = current.clone();
            expected["toolApproval"] = json!("ask");
            assert_eq!(write["params"]["policy"], expected);
            assert_eq!(write["params"]["expectedVersion"], version);
            if fail_conflict {
                fail(wire, &write, "permission_conflict");
                current["disallowedTools"] = json!(["blocked", "new-deny"]);
            } else {
                wire.reply(&write, json!({"version":3}));
                current = expected;
            }
        }
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":3,"policy":current}));
        let read = wire.read("session.permission.get");
        wire.reply(&read, json!({"version":3,"policy":current}));
        let write = wire.read("session.permission.update");
        let mut expected = current.clone();
        expected["fileAccess"] = json!("unrestricted");
        assert_eq!(write["params"]["policy"], expected);
        fail(wire, &write, "invalid_params");
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/permissions tool-approval ask\n");
    terminal.until("File access: Workspace read/write; Tool approval: Ask");
    terminal.until("\n> ");
    terminal.write(b"/permissions tool-approval ask\n");
    terminal.until("File access: Workspace read/write; Tool approval: Ask");
    terminal.until("\n> ");
    terminal.write(b"/approval file-access unrestricted\n");
    terminal.until("fixture invalid_params");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_permission_flag_overrides_saved_controls_and_invalid_defaults_are_ignored() {
    for fullscreen in [false, true] {
        for explicit in [false, true] {
            let home = Home::new();
            fs::write(
                home.0.join("config.json"),
                if explicit {
                    r#"{"locale":"en","permissionPreset":"full-access","fileAccess":"unrestricted","toolApproval":"bypass"}"#
                } else {
                    r#"{"locale":"en","permissionPreset":"workspace-write","fileAccess":false,"toolApproval":"unknown"}"#
                },
            )
            .unwrap();
            let daemon = home.daemon(move |wire| {
                wire.hello(&PRESET_METHODS);
                let create = wire.created();
                let preset = if explicit {
                    "read-only"
                } else {
                    "workspace-write"
                };
                assert_eq!(
                    create["params"]["permissionPolicy"],
                    json!({"preset":preset})
                );
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                wire.reply(
                    &history,
                    json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                );
                let read = wire.read("session.permission.get");
                wire.reply(
                    &read,
                    json!({"version":1,"policy":policy(preset, &[], &[])}),
                );
                wire.no_context();
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                let mut line = String::new();
                while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                    line.clear();
                }
            });
            let mut command = home.command();
            if fullscreen {
                command.arg("--fullscreen");
            }
            if explicit {
                command.args(["--permission", "read-only"]);
            }
            let mut terminal = Terminal::launch_tui(command);
            terminal.until("test / model");
            terminal.signal(libc::SIGKILL);
            terminal.wait();
            daemon.join().unwrap();
        }
    }
}

#[test]
fn tui_home_remembers_permissions_without_login_and_applies_them_to_the_first_message() {
    for initial_setup in [false, true] {
        for fullscreen in [false, true] {
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
                wire.hello(if initial_setup { &[] } else { &PRESET_METHODS });
                if initial_setup {
                    let create = wire.read("session.create");
                    fail(wire, &create, "provider_failed");
                } else {
                    wire.created();
                    let attach = wire.read("session.attach");
                    wire.reply(&attach, attach_record(&attach));
                    let history = wire.read("session.history");
                    wire.reply(
                        &history,
                        json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                    );
                    let read = wire.read("session.permission.get");
                    wire.reply(
                        &read,
                        json!({"version":1,"policy":policy("workspace-write", &[], &[])}),
                    );
                    wire.no_context();
                }
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
                let create = wire.read("session.create");
                assert_eq!(
                    create["params"]["permissionPolicy"],
                    json!({"preset":"workspace-write","fileAccess":"read-only","toolApproval":"bypass"})
                );
                wire.reply(
                    &create,
                    json!({"sessionId":"session","sequence":8,"model":{"providerId":"test","modelId":"model"}}),
                );
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                wire.reply(
                    &history,
                    json!({"events":[],"through":8,"nextAfter":8,"done":true}),
                );
                if !initial_setup {
                    let read = wire.read("session.permission.get");
                    wire.reply(
                        &read,
                        json!({"version":1,"policy":{"preset":"workspace-write","fileAccess":"read-only","toolApproval":"bypass","allowedTools":[],"disallowedTools":[]}}),
                    );
                }
                let submit = wire.read("session.submit");
                assert_eq!(submit["params"]["text"], "first message");
                wire.reply(&submit, json!({}));
                wire.notice(
                    "session.event",
                    json!({"sessionId":"session","branchId":"main","event":{"id":"input-9","sequence":9,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"accepted first message"}]},"mode":"queue"}}}),
                );
                for (sequence, name) in [(10, "applied"), (11, "consumed")] {
                    wire.notice(
                        "session.event",
                        json!({"sessionId":"session","branchId":"main","event":{"id":format!("input-{sequence}"),"sequence":sequence,"create_at":1,"type":"input","name":name,"data":{"input_id":"input-9"}}}),
                    );
                }
                wire.no_context();
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
            if initial_setup {
                terminal.until("Sign in with /login");
            } else {
                terminal.until("test / model");
                terminal.write(b"/new\r");
                terminal.until("new session");
            }
            let mark = terminal.transcript.len();
            terminal.write(b"/approvals\r");
            terminal.until("File access");
            terminal.write(b"\r");
            terminal.until("Read only");
            terminal.write(b"\x1b[A\r");
            terminal.until("File access: Read only (also for new sessions)");
            terminal.write(b"/permissions\r");
            terminal.until("Process isolation");
            terminal.write(b"\x1b[B\r");
            terminal.until("Bypass");
            terminal.write(b"\x1b[B\r");
            terminal.until("Tool approval: Bypass (also for new sessions)");
            let config: Value =
                serde_json::from_str(&fs::read_to_string(home.0.join("config.json")).unwrap())
                    .unwrap();
            assert_eq!(config["fileAccess"], "read-only");
            assert_eq!(config["toolApproval"], "bypass");
            assert!(config.get("permissionPreset").is_none());
            if !initial_setup {
                assert!(!terminal.transcript[mark..].contains("Sign in with /login"));
            }
            assert!(!terminal.transcript[mark..].contains("New members from main"));
            terminal.write(b"first message\r");
            terminal.until("accepted first message");
            terminal.signal(libc::SIGKILL);
            terminal.wait();
            daemon.join().unwrap();
        }
    }
}

#[test]
fn permission_flag_selects_the_new_and_resumed_preset_with_version_checks() {
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
        let mut current = policy("read-only", &[], &[]);
        current["fileAccess"] = json!("read-only");
        current["toolApproval"] = json!("ask");
        wire.reply(
            &get,
            json!({"version":6,"policy":current}),
        );
        let update = wire.read("session.permission.update");
        assert_eq!(update["params"]["expectedVersion"], 6);
        current["preset"] = json!("full-access");
        assert_eq!(update["params"]["policy"], current);
        wire.reply(
            &update,
            json!({"version":9,"policy":current}),
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
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("File access: Read only; Tool approval: Ask")
    );

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
        approval(wire, "second", "cargo build --release");
        let rules = wire.read("permission.rules");
        fail(wire, &rules, "permission_rules_failed");
        answered(wire, "second", "allow");
        approval(wire, "third", "cargo test && rm -rf target");
        answered(wire, "third", "deny");
        approval(wire, "fourth", "cargo fmt");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r3","commandRules":[deny_push, allow_test,
                {"command":"cargo  fmt","match":"exact","decision":"allow"}]}),
        );
        answered(wire, "fourth", "allow");
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
    let mut terminal = Terminal::launch(home.command());
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
    terminal.signal(libc::SIGINT);
    terminal.until("[rule not saved] the turn was stopped before the rule was written");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
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
        state(wire, 3, "workspace-write");
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r1","commandRules":[allow_test, deny_push]}),
        );
        state(wire, 3, "workspace-write");
        state(wire, 3, "workspace-write");
        let update = wire.read("session.permission.update");
        assert_eq!(
            update["params"],
            json!({"sessionId":"session","branchId":"main","expectedVersion":3,
                "policy":policy("full-access", &[], &["blocked-tool"])})
        );
        fail(wire, &update, "permission_conflict");
        state(wire, 5, "read-only");
        state(wire, 5, "read-only");
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
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r1","commandRules":[deny_push]})
        );
        wire.reply(&write, json!({"revision":"r2"}));
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
        let rules = wire.read("permission.rules");
        wire.reply(
            &rules,
            json!({"revision":"r4","commandRules":[deny_push, allow_make]}),
        );
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
        let write = wire.read("permission.set_rules");
        assert_eq!(
            write["params"],
            json!({"expectedRevision":"r5","commandRules":[deny_push, allow_make]})
        );
        wire.reply(&write, json!({"revision":"r6"}));
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
    let mut terminal = Terminal::launch(home.command());
    terminal.until("permission: workspace-write");
    terminal.until("\n> ");
    terminal.write(b"/approvals\n");
    terminal.until("File access: Workspace read/write");
    terminal.until("Tool approval: Ask");
    terminal.until("Legacy preset seed (main branch): workspace-write");
    terminal.until("  disallowed tools: blocked-tool");
    terminal.until("Command rules (user scope, every Session):");
    terminal.until("  1  allow  exact   cargo test");
    terminal.until("  2  deny   prefix  git push");
    terminal.until("Usage: /permissions");
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
    terminal.until("Preset (main branch): workspace-write. Child branches and members created later from main inherit it; existing branches and members keep their policies.");
    terminal.until("\n> ");
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
    terminal.write(b"/approvals allow exact cargo test; rm -rf target\n");
    terminal.until("Error: A command rule needs a simple command");
    terminal.until("\n> ");
    terminal.write(b"/approvals admin\n");
    terminal.until("Error: Unknown permission preset");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove\n");
    terminal.until("Error: Usage: /permissions");
    terminal.until("\n> ");
    terminal.write(b"/approvals remove 9\n");
    terminal.until("Error: No such rule in the last /approvals listing");
    terminal.until("\n> ");
    terminal.write(b"/approval remove\n");
    terminal.until("Error: Usage: /permissions");
    terminal.until("\n> ");
    terminal.write(b"after\n");
    terminal.until("[permission] main branch preset: full-access");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
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
        fs::write(&config, json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","apiKeyEnv":"DIM_CLI_TEST_API_KEY","baseUrl":format!("http://{}/v1", self.address),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]}).to_string()).unwrap();
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
    let (code, _, stderr) = daemon.run(&["exec", "-p", "exec: printf default-ran"]);
    assert_eq!(code, Some(3), "{stderr}");
    assert!(stderr.contains("Interactive approval required"), "{stderr}");
    assert!(!stderr.contains("permission:"), "{stderr}");
    let session = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session: "))
        .unwrap()
        .to_owned();
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
    assert!(
        stderr.contains("File access: Unrestricted; Tool approval: Bypass"),
        "{stderr}"
    );
    assert!(stderr.contains("[done] exec"), "{stderr}");
    assert!(
        !stderr.contains("Interactive approval required"),
        "{stderr}"
    );
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
        assert!(
            !stderr.contains("Interactive approval required"),
            "{stderr}"
        );
    }
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
    assert!(
        !stderr.contains("Interactive approval required"),
        "{stderr}"
    );
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn real_daemon_always_allow_saves_an_exact_rule_until_approvals_removes_it() {
    let daemon = RealDaemon::start();
    let mut terminal = Terminal::launch(daemon.command());
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
    terminal.until("File access: Workspace read/write");
    terminal.until("Tool approval: Ask");
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
    terminal.write(b"/permissions file-access read-only\n");
    terminal.until("File access: Read only; Tool approval: Ask");
    terminal.editor();
    terminal.write(b"/permissions tool-approval bypass\n");
    terminal.until("File access: Read only; Tool approval: Bypass");
    terminal.editor();
    let mark = terminal.transcript.len();
    terminal.write(b"exec: printf bypass-ran\n");
    terminal.until("tool said: bypass-ran");
    terminal.editor();
    assert!(
        !terminal.transcript[mark..].contains("[approval]"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"/permissions tool-approval ask\n");
    terminal.until("File access: Read only; Tool approval: Ask");
    terminal.editor();
    let mark = terminal.transcript.len();
    terminal.write(b"exec: printf ask-ran\n");
    terminal.until("requires approval under Ask tool approval");
    terminal.until("Allow once, always, or deny? [y/a/N] ");
    terminal.write(b"n\n");
    terminal.editor();
    assert!(
        !terminal.transcript[mark..].contains("tool said: ask-ran"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    let (code, _, stderr) = daemon.run(&["exec", "-p", "exec: printf always-ran"]);
    assert_eq!(code, Some(3), "{stderr}");
}

#[test]
fn member_approval_in_line_mode_allows_rejection_during_loading_and_rechecks_an_allow() {
    for reject_loading in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit"); wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"outer","call":{"name":"member_approval_allow","input":{"approvalId":"route"}},"reason":"Leader permission"}));
            let detail = wire.read("session.collaboration.approval.get");
            let original = json!({"state":"pending","approvalId":"route","descriptor":{"delegationId":"delegation","member":{"agentId":"reviewer","sessionId":"member","branchId":"main"},"requestId":"original","call":{"name":"exec","input":{"command":"cargo test line action"}},"reason":"Member process request"}});
            if !reject_loading {
                wire.reply(&detail, original.clone());
                let refresh = wire.read("session.collaboration.approval.get");
                wire.reply(&refresh, json!({"state":"ended"}));
            }
            let response = wire.read("approval.respond");
            assert_eq!(response["params"], json!({"sessionId":"session","branchId":"review","requestId":"outer","decision":"deny"}));
            wire.reply(&response, json!({}));
            if reject_loading { wire.reply(&detail, original); }
            wire.end(json!({"turn_id":"turn","status":"completed","reason":"end_turn"}));
            wire.reply(&idle, json!({"sequence":9}));
            wire.detach();
        });
        let mut terminal = Terminal::launch(home.command());
        terminal.until("\n> ");
        terminal.write(b"work\n");
        if reject_loading {
            terminal.until("Loading member action");
            terminal.write(b"y\n");
            terminal.until("Loading member action");
            terminal.write(b"n\n");
        } else {
            terminal.until("Leader reason: Leader permission");
            terminal.until("cargo test line action");
            terminal.until("Allow this member action once?");
            terminal.write(b"y\n");
            terminal.until("Member request ended");
        }
        terminal.until("\n> ");
        terminal.write(b"/exit\n");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}
