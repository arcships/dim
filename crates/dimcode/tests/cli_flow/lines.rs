//! The lines panel and the Session's default branch: the root and the default
//! branch are marked, `p` continues on a line, and `x` ends neither the
//! default branch nor a line it continues from (design §14).
use super::*;

/// A line's creation, as its parent's Timeline carries it.
/// The requests every flow meets besides the ones it asserts, answered here so
/// each flow reads only the requests it is about.
fn next(wire: &mut Wire) -> Value {
    next_usage(wire, &mut |_| None)
}

/// The same requests, with the context reading answered when `usage` returns
/// one and reported unknown otherwise.
fn next_usage(wire: &mut Wire, usage: &mut impl FnMut(&Value) -> Option<Value>) -> Value {
    loop {
        let request = wire.read_any();
        let branch = request["params"]["branchId"].clone();
        let through = request["params"]["through"].clone();
        match request["method"].as_str().unwrap_or_default() {
            "session.methods" => {
                let methods: Vec<_> = [
                    "session.input.pending",
                    "session.exec.pending",
                    "session.turn.get",
                ]
                .into_iter()
                .map(|name| json!({"name":name,"mutating":false}))
                .collect();
                wire.reply(&request, json!({"methods":methods}));
            }
            "session.question.pending" => wire.reply(
                &request,
                json!({"branchId":branch,"through":through,"batches":[],"next":null}),
            ),
            "session.input.pending" => wire.reply(
                &request,
                json!({"branchId":branch,"through":through,"inputs":[],"next":null}),
            ),
            "session.exec.pending" => wire.reply(
                &request,
                json!({"branchId":branch,"through":through,"tasks":[],"next":null}),
            ),
            "session.turn.get" => wire.reply(
                &request,
                json!({"branchId":branch,"through":through,"turn":null,"output":null}),
            ),
            "account.providers" => wire.reply(&request, json!([])),
            "provider.list" => wire.reply(&request, json!({"providers":[]})),
            "session.context.usage" => match usage(&request) {
                Some(reading) => wire.reply(&request, reading),
                None => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method: session.context.usage",
                        "data":{"appError":"unknown_method"}}})),
            },
            _ => return request,
        }
    }
}

fn created(branch: &str, sequence: u64) -> Value {
    json!({"id":format!("{branch}-create"),"sequence":sequence,"create_at":1,"type":"branch","name":"create","data":{
        "branch":branch,"at":{"kind":"create"},
        "source":{"kind":"tool","turn_id":"turn","request_id":"request","call_id":format!("{branch}-call")},
        "prompt":format!("{branch} work"),
    }})
}

fn history(through: u64, events: Vec<Value>) -> Value {
    json!({"events":events,"through":through,"nextAfter":through,"done":true})
}

/// A user message `text` on the line, as its Timeline records it; the line
/// draws it as a row once [`applied`] carries its id.
fn said(sequence: u64, text: &str) -> Value {
    json!({"id":format!("input-{sequence}"),"sequence":sequence,"create_at":1,
        "type":"input","name":"message",
        "data":{"user":{"content":[{"type":"text","text":text}]},"mode":"append"}})
}

/// The line adopted the message `input_id` the event `sequence` names: the
/// projection draws it as a transcript row from here.
fn applied(sequence: u64, input_id: &str) -> Value {
    json!({"id":format!("applied-{sequence}"),"sequence":sequence,"create_at":1,
        "type":"input","name":"applied",
        "data":{"input_id":input_id,"mode":"append"}})
}

/// The model consumed the message `input_id`.
fn consumed(sequence: u64, input_id: &str) -> Value {
    json!({"id":format!("consumed-{sequence}"),"sequence":sequence,"create_at":1,
        "type":"input","name":"consumed","data":{"input_id":input_id}})
}

/// The composer's top border as the interface last painted it since
/// transcript offset `from`: that row carries the model label, and the
/// transcript still holds the label an earlier frame drew. A panel's border
/// names its title right after the corner; the composer's model label sits
/// at the right end of its own.
fn bar_since(terminal: &Terminal, from: usize) -> Option<String> {
    terminal.painted_rows(from).into_iter().rfind(|row| {
        let row = row.trim_start();
        row.starts_with('╭') && row.ends_with('╮') && !row.starts_with("╭─ ")
    })
}

fn directory(default_branch_id: &str, lines: &[(&str, Option<&str>)]) -> Value {
    json!({
        "defaultBranchId": default_branch_id,
        "branches": lines.iter().map(|(branch, parent)| match parent {
            Some(parent) => json!({"branchId":branch,"parentBranchId":parent,"branchPoint":1,"state":"open","head":8,"purpose":format!("{branch} work")}),
            None => json!({"branchId":branch,"state":"open","head":8}),
        }).collect::<Vec<_>>(),
    })
}

fn entry(branch: &str, parent: &str) -> Value {
    json!({"branchId":branch,"parentBranchId":parent,"branchPoint":2,"state":"open","head":8,"purpose":format!("{branch} work"),"cutMessages":0,"end":null})
}

/// The lines panel marks the root and the default branch; `p` continues on the
/// selected line, and the next message with no line chosen reaches it.
#[test]
fn tui_lines_panel_marks_the_default_branch_and_p_moves_it() {
    let home = Home::new();
    let (moved, move_seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.branch_end",
        ]);
        wire.2 = true;
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        let read = next(wire);
        assert_eq!(read["method"], "session.history");
        assert_eq!(read["params"]["branchId"], "main");
        wire.reply(&read, history(8, vec![created("line-1", 2)]));

        let mut moved_on = false;
        loop {
            let request = next(wire);
            match request["method"].as_str().unwrap() {
                "session.branch_set_default" => {
                    assert_eq!(
                        request["params"]["branchId"],
                        json!("line-1"),
                        "p continues on the selected line"
                    );
                    wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    moved_on = true;
                    moved.send(()).unwrap();
                }
                "session.branch.get" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    wire.reply(&request, entry("line-1", "main"));
                }
                "session.history" => {
                    if moved_on {
                        assert_eq!(
                            request["params"]["branchId"],
                            json!("line-1"),
                            "the shown transcript is the line the move reached"
                        );
                    }
                    wire.reply(&request, history(8, vec![]));
                }
                "session.submit" => {
                    assert_eq!(
                        request["params"]["branchId"],
                        json!("line-1"),
                        "a message with no line chosen reaches the default branch"
                    );
                    break;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
        wire.detach();
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/lines\r");
    terminal.until("the session itself");
    terminal.until("Main line");
    terminal.until("Start");
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    move_seen.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b");
    terminal.until("⑂ main ▸ line-1 work");
    terminal.write(b"hello\r");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// `x` ends neither the default branch nor a line it continues from, so the
/// end of a line above the role is refused with its own message.
#[test]
fn tui_x_is_refused_on_the_default_branch_and_on_a_line_it_continues_from() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.branch_end",
        ]);
        wire.2 = true;
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory(
                "line-2",
                &[
                    ("main", None),
                    ("line-1", Some("main")),
                    ("line-2", Some("line-1")),
                ],
            ),
        );
        loop {
            let request = next(wire);
            match request["method"].as_str().unwrap() {
                "session.history" => {
                    wire.reply(&request, history(8, vec![]));
                }
                "session.branch.get" => {
                    assert_eq!(request["params"]["branchId"], json!("line-2"));
                    wire.reply(&request, entry("line-2", "line-1"));
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    sized(&mut command, 120, 36);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    // The panel opens on the shown line, which is the default branch.
    terminal.write(b"/lines\r");
    terminal.until("Main line");
    terminal.write(b"x");
    terminal.until("the default branch and the lines it continues from stay open; continue on another line first");

    // Reopened, the selection is the shown line again; one row up is the
    // line it was opened from.
    terminal.write(b"/lines\r");
    terminal.write(b"\x1b[A");
    terminal.write(b"x");
    terminal.until("the default branch and the lines it continues from stay open; continue on another line first");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A refusal from `session.branch_set_default` is drawn in the panel that
/// raised it, not only reported elsewhere.
#[test]
fn tui_a_refused_continue_is_shown_in_the_lines_panel() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.branch_end",
        ]);
        wire.2 = true;
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        let refused = loop {
            let request = next(wire);
            match request["method"].as_str().unwrap() {
                "session.history" => wire.reply(&request, history(8, vec![created("line-1", 2)])),
                "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                "session.branch_set_default" => break request,
                other => panic!("unexpected {other}: {request}"),
            }
        };
        assert_eq!(refused["params"]["branchId"], json!("line-1"));
        wire.send(json!({"jsonrpc":"2.0","id":refused["id"],"error":{
            "code":-32001,
            "message":"the line was created through a client before the upgrade, so it cannot be the default branch",
            "data":{"appError":"branch_not_selectable"}}}));
        wire.detach();
    });
    let mut command = home.command();
    sized(&mut command, 120, 40);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/lines\r");
    terminal.until("the session itself");
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    terminal.until("so it cannot be the default branch");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// The shown line resumes from the cursor its attach reply carries: a line's
/// own sequence, and the root's `sequence` when the conversation is the root
/// (design §14). Its model is that line's own, adopted at or below the cursor,
/// read through `session.model.get`: the reply's model is the root's, and the
/// production display transcript omits `context/configured`. The reply's cursor
/// is authoritative: the directory only answers for a server that does not
/// carry the field.
#[test]
fn tui_resumes_the_shown_line_from_the_attach_cursor() {
    for (line, cursor, listed, entries) in [
        ("main", 8, "main", &[("main", None)][..]),
        (
            "line-1",
            5,
            "line-1",
            &[("main", None), ("line-1", Some("main"))][..],
        ),
        (
            "line-1",
            5,
            "main",
            &[("main", None), ("line-1", Some("main"))][..],
        ),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
                "session.branch_end",
                "session.history.display",
                "session.history.detail",
                "session.history.display.tail",
                "session.history.display.transcript",
                "session.model.get",
            ]);
            wire.2 = true;
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":line,"sequence":cursor});
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(&branches, directory(listed, entries));
            let window = next(wire);
            assert_eq!(window["method"], "session.history.display");
            assert_eq!(window["params"]["branchId"], json!(line));
            assert_eq!(
                window["params"]["through"],
                json!(cursor),
                "the shown line resumes from the attach reply's default cursor"
            );
            wire.reply(
                &window,
                json!({"items":[],"through":cursor,"nextBefore":1,"nextAfter":cursor,"done":true}),
            );
            let parent = entries
                .iter()
                .find(|(id, _)| *id == line)
                .and_then(|(_, parent)| *parent);
            loop {
                let request = next(wire);
                match request["method"].as_str().unwrap() {
                    "session.branch.get" => {
                        assert_eq!(request["params"]["branchId"], json!(line));
                        let detail = match parent {
                            Some(parent) => entry(line, parent),
                            None => json!({"branchId":line,"state":"open","head":8}),
                        };
                        wire.reply(&request, detail);
                    }
                    "session.model.get" => {
                        assert_eq!(request["params"]["branchId"], json!(line));
                        assert_eq!(request["params"]["through"], json!(cursor));
                        assert!(
                            request["params"].get("before").is_none(),
                            "a line's configuration is read at one cutoff, not paged out of its history"
                        );
                        wire.reply(
                            &request,
                            json!({"branchId":line,"through":cursor,
                                "configuration":{"model":{"providerId":"test","modelId":"line-model"}}}),
                        );
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        sized(&mut command, 100, 32);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until(match line {
            "main" => "test / model",
            _ => "test / line-model",
        });
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// Another client's adoption on a line reaches the bar when the user continues
/// there, and that line's own facts are read then (timeline UI §5, design §14).
fn adopted(wire: &mut Wire, branch: &str, sequence: u64, model: &str, preferences: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,
            "event":configured_event(sequence, model, preferences)}),
    );
}

/// A context reading as `session.context.usage` reports one.
fn reading(input: f64) -> Value {
    json!({"contextWindow":256000.0,"outputTokens":32000.0,"thresholdTokens":200000.0,
        "estimatedInputTokens":input,"baselineSource":"estimate"})
}

fn policy(preset: &str) -> Value {
    json!({"preset":preset,"allowedTools":[],"disallowedTools":[]})
}

/// `/resume` applies the launch preset to the line the attach reply names, the
/// line the conversation is on, not to the root the interface is leaving and
/// not to a line the directory reported before the attach (design §14).
#[test]
fn tui_resume_applies_the_preset_to_the_resumed_default_branch() {
    let home = Home::new();
    let workspace = std::fs::canonicalize(&home.0).unwrap();
    let workspace = workspace.to_str().unwrap().to_owned();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.catalog",
            "session.branches",
            "session.permission.get",
            "session.permission.update",
        ]);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let catalog = wire.read("session.catalog");
        wire.reply(
            &catalog,
            json!({"sessions":[{"sessionId":"session","cwd":workspace,
                "preview":"on the line","model":{"providerId":"test","modelId":"model"},
                "updatedAt":5}]}),
        );
        // The directory's line and the attach reply's disagree: the reply is
        // the one the write follows.
        let directory = || directory("main", &[("main", None), ("line-1", Some("main"))]);
        let mut opening_read = true;
        loop {
            let request = next(wire);
            match request["method"].as_str().unwrap_or_default() {
                "session.branches" => wire.reply(&request, directory()),
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["defaultBranch"] = json!({"branchId":"line-1","sequence":5});
                    wire.reply(&request, record);
                }
                "session.permission.get" => {
                    assert_eq!(
                        request["params"]["branchId"],
                        json!("line-1"),
                        "the preset follows the attach reply's line"
                    );
                    wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    );
                    if !opening_read {
                        break;
                    }
                    opening_read = false;
                }
                "session.history" => wire.reply(&request, history(5, vec![])),
                other => panic!("unexpected {other}: {request}"),
            }
        }
        let update = next(wire);
        assert_eq!(update["method"], "session.permission.update");
        assert_eq!(
            update["params"]["branchId"],
            json!("line-1"),
            "the write follows the attach reply's line"
        );
        wire.reply(&update, json!({"version":2,"policy":policy("read-only")}));
        loop {
            let request = next(wire);
            match request["method"].as_str().unwrap_or_default() {
                "session.branches" => wire.reply(&request, directory()),
                "session.permission.get" => {
                    wire.reply(&request, json!({"version":2,"policy":policy("read-only")}))
                }
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["defaultBranch"] = json!({"branchId":"line-1","sequence":5});
                    wire.reply(&request, record);
                }
                "session.history" => wire.reply(
                    &request,
                    history(5, vec![configured_event(4, "line-model", json!({}))]),
                ),
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.args(["--permission", "read-only"]);
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Read only");
    terminal.write(b"/resume\r");
    terminal.until("on the line");
    terminal.write(b"\r");
    terminal.until("test / line-model");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// `p` continues on the moved line: its transcript, and its own model,
/// permission and context in the top bar, with its configuration facts folding
/// from then on even at a sequence below the root's (design §14, timeline UI §5).
#[test]
fn tui_p_shows_the_moved_lines_own_model_permission_and_usage() {
    let home = Home::new();
    let (moved, move_seen) = std::sync::mpsc::channel();
    let (preset, presets) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.branch_end",
            "session.permission.get",
            "session.permission.update",
            "session.context.usage",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        let root = next(wire);
        assert_eq!(root["method"], "session.history");
        wire.reply(&root, history(8, vec![]));
        // Each line reports its own context: the root's, then the line's.
        let mut usage = |request: &Value| {
            let branch = request["params"]["branchId"].as_str().unwrap_or_default();
            Some(reading(if branch == "line-1" { 64000. } else { 1800. }))
        };
        let mut root_preset = false;
        loop {
            let request = next_usage(wire, &mut usage);
            let branch = request["params"]["branchId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.permission.get" => match branch {
                    "main" => {
                        assert!(!root_preset, "the root's preset was read twice");
                        root_preset = true;
                        wire.reply(
                            &request,
                            json!({"version":1,"policy":policy("workspace-write")}),
                        );
                        // Another client adopts a model on the line while the
                        // root is still the default branch.
                        adopted(
                            wire,
                            "line-1",
                            3,
                            "replacement",
                            json!({"reasoningEffort":"high"}),
                        );
                    }
                    "line-1" => {
                        preset.send(branch.to_owned()).unwrap();
                        wire.reply(
                            &request,
                            json!({"version":4,"policy":policy("full-access")}),
                        );
                    }
                    other => panic!("permission read on {other}"),
                },
                "session.branch_set_default" => {
                    assert_eq!(branch, "line-1");
                    wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    moved.send(()).unwrap();
                }
                "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                "session.history" => {
                    let through = if branch == "line-1" { 3 } else { 8 };
                    wire.reply(&request, history(through, vec![]));
                }
                "session.submit" => {
                    assert_eq!(branch, "line-1");
                    wire.reply(&request, json!({"event":{}}));
                    // A fact below the root's cursor is still the line's own.
                    adopted(
                        wire,
                        "line-1",
                        4,
                        "chosen",
                        json!({"reasoningEffort":"low"}),
                    );
                    break;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
        wire.hold();
    });
    let mut command = home.command();
    sized(&mut command, 110, 34);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("1.8K / 256K");
    terminal.until("Workspace write");
    terminal.write(b"/lines\r");
    terminal.until("Main line");
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    move_seen.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b");
    terminal.until("⑂ main ▸ line-1 work");
    terminal.until_anywhere("test / replacement · high");
    terminal.until_anywhere("⏵⏵ Full access");
    terminal.until_anywhere("64K / 256K");
    assert_eq!(
        presets.recv_timeout(PATIENCE).unwrap(),
        "line-1",
        "the moved line's own preset is read"
    );
    terminal.write(b"hello\r");
    terminal.until("test / chosen · low");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A line whose configuration this client cannot read carries no model in the
/// bar, and moving back to the root shows the root's own again (design §14):
/// a server without `session.model.get`, and one that answers
/// `configuration: null`, both leave the moved line without a model.
#[test]
fn tui_a_line_without_a_readable_configuration_carries_no_model_and_the_root_restores_its_own() {
    for offers in [false, true] {
        let home = Home::new();
        let (moved, move_seen) = std::sync::mpsc::channel();
        let (back, back_seen) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            let mut features = vec![
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
            ];
            if offers {
                features.push("session.model.get");
            }
            wire.hello(&features);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(
                &branches,
                directory("main", &[("main", None), ("line-1", Some("main"))]),
            );
            let root = next(wire);
            assert_eq!(root["method"], "session.history");
            wire.reply(&root, history(8, vec![]));
            let mut usage = |_request: &Value| None;
            let mut moves = 0;
            loop {
                let request = next_usage(wire, &mut usage);
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.branch_set_default" => {
                        moves += 1;
                        let target = if moves == 1 { "line-1" } else { "main" };
                        assert_eq!(branch, target);
                        wire.reply(&request, json!({"defaultBranchId":target}));
                        if moves == 1 {
                            moved.send(()).unwrap();
                        } else {
                            back.send(()).unwrap();
                        }
                    }
                    "session.branch.get" => {
                        assert_eq!(branch, "line-1");
                        wire.reply(&request, entry("line-1", "main"));
                    }
                    "session.history" => {
                        let (through, events) = if branch == "line-1" {
                            (3, vec![said(2, "line window"), applied(3, "input-2")])
                        } else {
                            (8, vec![])
                        };
                        wire.reply(&request, history(through, events));
                    }
                    "session.model.get" => {
                        assert!(offers, "a server without the read is not asked");
                        assert_eq!(branch, "line-1");
                        wire.reply(
                            &request,
                            json!({"branchId":"line-1","through":3,"configuration":null}),
                        );
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        sized(&mut command, 100, 32);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        let before_move = terminal.consumed;
        terminal.write(b"\x1b[B");
        terminal.write(b"p");
        move_seen.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"\x1b");
        terminal.until("line window");
        terminal.write(b"probe");
        terminal.until("probe");
        terminal.write(b"\x7f\x7f\x7f\x7f\x7f");
        let bar = bar_since(&terminal, before_move).expect("the bar was painted after the move");
        assert!(
            !bar.contains("test / model"),
            "offers={offers}: the moved line carries no model: {bar}"
        );
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        terminal.write(b"\x1b[A");
        terminal.write(b"p");
        back_seen.recv_timeout(PATIENCE).unwrap();
        terminal.until("test / model");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A resumed Session whose conversation is on a line, with a server that
/// cannot read that line's configuration, shows no model and reports the
/// limitation once for the whole visit: not again when the window is ready,
/// when the user opens the line, or when `/earlier` reads another page of it
/// (design §14).
#[test]
fn tui_a_resumed_line_without_a_configuration_read_reports_it_once() {
    let home = Home::new();
    let (switched, switch_seen) = std::sync::mpsc::channel();
    let (window, window_done) = std::sync::mpsc::channel();
    let (asked, earlier_seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.catalog",
        ]);
        fn page(through: u64, before: u64, events: Vec<Value>) -> Value {
            json!({"events":events,"through":through,"nextAfter":through,
                "nextBefore":before,"done":true})
        }
        // Answers the inventory reads that follow a window, through the line's
        // turn read that finishes it.
        fn inventory(wire: &mut Wire) {
            loop {
                let request = wire.read_any();
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                let through = request["params"]["through"].clone();
                match request["method"].as_str().unwrap_or_default() {
                    "session.input.pending" => wire.reply(
                        &request,
                        json!({"branchId":branch,"through":through,"inputs":[],"next":null}),
                    ),
                    "session.exec.pending" => wire.reply(
                        &request,
                        json!({"branchId":branch,"through":through,"tasks":[],"next":null}),
                    ),
                    "session.branch.get" => {
                        wire.reply(&request, entry("line-1", "main"));
                    }
                    "session.turn.get" => {
                        wire.reply(
                            &request,
                            json!({"branchId":branch,"through":through,"turn":null,"output":null}),
                        );
                        if branch == "line-1" {
                            break;
                        }
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        }
        let mut usage = |_request: &Value| None;
        let mut windows = 0;
        loop {
            let request = next_usage(wire, &mut usage);
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let session = request["params"]["sessionId"].as_str().unwrap_or_default();
            let branch = request["params"]["branchId"].as_str().unwrap_or_default();
            match method.as_str() {
                "session.attach" => {
                    let mut record = attach_record(&request);
                    if session == "on-the-line" {
                        record["defaultBranch"] = json!({"branchId":"line-1","sequence":5});
                        switched.send(()).unwrap();
                    } else {
                        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                    }
                    wire.reply(&request, record);
                }
                "session.branches" => {
                    let default = if session == "on-the-line" {
                        "line-1"
                    } else {
                        "main"
                    };
                    wire.reply(
                        &request,
                        directory(default, &[("main", None), ("line-1", Some("main"))]),
                    );
                }
                "session.history" if branch != "line-1" => {
                    wire.reply(&request, history(8, vec![]));
                }
                "session.history" => {
                    let before = request["params"]
                        .get("before")
                        .and_then(serde_json::Value::as_u64);
                    match before {
                        // The resumed window, and later the line opened for
                        // view: one page that leaves older content unread.
                        None => {
                            wire.reply(
                                &request,
                                page(
                                    6,
                                    3,
                                    vec![
                                        said(4, "resumed line"),
                                        applied(5, "input-4"),
                                        consumed(6, "input-4"),
                                    ],
                                ),
                            );
                            inventory(wire);
                            windows += 1;
                            if windows == 1 {
                                window.send(()).unwrap();
                            }
                        }
                        // `/earlier`: the page that reaches the branch start.
                        Some(3) => {
                            wire.reply(
                                &request,
                                page(6, 1, vec![said(1, "older page"), applied(2, "input-1")]),
                            );
                            asked.send(()).unwrap();
                            break;
                        }
                        other => panic!("unexpected earlier cursor {other:?}: {request}"),
                    }
                }
                "session.catalog" => wire.reply(
                    &request,
                    json!({"sessions":[
                        {"sessionId":"on-the-line","cwd":"/workspace","preview":"on the line",
                            "model":{"providerId":"test","modelId":"model"},"updatedAt":1},
                        {"sessionId":"session","cwd":"/workspace","preview":"root work",
                            "model":{"providerId":"test","modelId":"model"},"updatedAt":2}
                    ]}),
                ),
                "session.detach" => wire.reply(&request, json!({})),
                "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                other => panic!("unexpected {other}: {request}"),
            }
        }
        wire.hold();
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    let before_switch = terminal.consumed;
    terminal.write(b"/resume\r");
    terminal.until("on the line");
    terminal.write(b"\r");
    switch_seen.recv_timeout(PATIENCE).unwrap();
    terminal.until("Earlier content not loaded");
    // A composer echo proves the interface applied what the reply queued
    // before it, the limitation notice among it.
    terminal.write(b"probe");
    terminal.until("probe");
    terminal.write(b"\x7f\x7f\x7f\x7f\x7f");
    let bar = bar_since(&terminal, before_switch).expect("the bar was painted after the resume");
    assert!(
        !bar.contains("test / model"),
        "the resumed line carries no model: {bar}"
    );
    let reported = |terminal: &Terminal| {
        strip_ansi(&terminal.transcript)
            .matches("cannot read line line-1")
            .count()
    };
    assert_eq!(
        reported(&terminal),
        1,
        "the resume reports the limitation once"
    );
    window_done.recv_timeout(PATIENCE).unwrap();
    // Opening the line for view reads its window again, and makes its earlier
    // pages draw; both stay within the visit the limitation was reported for.
    terminal.write(b"/lines\r");
    terminal.until("Main line");
    terminal.write(b"\r");
    terminal.until("resumed line");
    // `/earlier` is a no-op until a window's inventory finishes; the read
    // starts as soon as it has, and the daemon names the page it asks for.
    terminal.write(b"/earlier\r");
    let mut attempts = 0;
    while earlier_seen
        .recv_timeout(Duration::from_millis(200))
        .is_err()
    {
        attempts += 1;
        assert!(attempts < 25, "the earlier read never started");
        terminal.write(b"/earlier\r");
    }
    terminal.write(b"/timeline\r");
    terminal.until("#1  older page");
    assert_eq!(
        reported(&terminal),
        1,
        "another page of the same line does not report it again"
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// Continuing on a line whose configuration this client cannot read reports
/// the limitation once: the window that follows the move does not report it
/// again (design §14).
#[test]
fn tui_continuing_on_a_line_without_a_configuration_read_reports_it_once() {
    let home = Home::new();
    let (moved, move_seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        let root = next(wire);
        assert_eq!(root["method"], "session.history");
        wire.reply(&root, history(8, vec![]));
        let mut usage = |_request: &Value| None;
        loop {
            let request = next_usage(wire, &mut usage);
            let branch = request["params"]["branchId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.branch_set_default" => {
                    assert_eq!(branch, "line-1");
                    wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    moved.send(()).unwrap();
                }
                "session.branch.get" => {
                    assert_eq!(branch, "line-1");
                    wire.reply(&request, entry("line-1", "main"));
                }
                "session.history" => {
                    let (through, events) = if branch == "line-1" {
                        (3, vec![said(2, "line window"), applied(3, "input-2")])
                    } else {
                        (8, vec![])
                    };
                    wire.reply(&request, history(through, events));
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/lines\r");
    terminal.until("Main line");
    let before_move = terminal.consumed;
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    move_seen.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b");
    terminal.until("line window");
    terminal.write(b"probe");
    terminal.until("probe");
    let bar = bar_since(&terminal, before_move).expect("the bar was painted after the move");
    assert!(
        !bar.contains("test / model"),
        "the moved line carries no model: {bar}"
    );
    assert_eq!(
        strip_ansi(&terminal.transcript)
            .matches("cannot read line line-1")
            .count(),
        1,
        "the move reports the limitation once"
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A policy read left before the conversation moved on does not become the new
/// centre's policy: the reply belongs to the line and the move that started it
/// (design §14). The new centre's read is released first, then the earlier
/// line's, so only the guard can keep the older reply out of the display.
#[test]
fn tui_a_late_policy_reply_from_an_earlier_line_does_not_replace_the_centres() {
    let home = Home::new();
    let (moved, move_seen) = std::sync::mpsc::channel();
    let (back, back_seen) = std::sync::mpsc::channel();
    let (new_read, new_seen) = std::sync::mpsc::channel();
    let (release_old, release_old_rx) = std::sync::mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.permission.get",
            "session.permission.update",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        let root = next(wire);
        assert_eq!(root["method"], "session.history");
        wire.reply(&root, history(8, vec![]));
        let mut usage = |_request: &Value| None;
        let mut moves = 0;
        let mut held: Option<Value> = None;
        loop {
            let request = next_usage(wire, &mut usage);
            let branch = request["params"]["branchId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            match request["method"].as_str().unwrap_or_default() {
                "session.permission.get" => match (moves, branch.as_str()) {
                    (0, "main") => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    ),
                    (1, "line-1") => held = Some(request),
                    (2, "main") => {
                        wire.reply(&request, json!({"version":5,"policy":policy("read-only")}));
                        new_read.send(()).unwrap();
                        release_old_rx.recv_timeout(PATIENCE).unwrap();
                        wire.reply(
                            &held.take().expect("the earlier line's read"),
                            json!({"version":4,"policy":policy("full-access")}),
                        );
                        wire.notice(
                            "session.event",
                            json!({"sessionId":"session","branchId":"main","event":said(9, "after the late policy")}),
                        );
                        wire.notice(
                            "session.event",
                            json!({"sessionId":"session","branchId":"main","event":applied(10, "input-9")}),
                        );
                    }
                    other => panic!("permission read on {other:?}"),
                },
                "session.branch_set_default" => {
                    moves += 1;
                    let target = if moves == 1 { "line-1" } else { "main" };
                    assert_eq!(branch, target);
                    wire.reply(&request, json!({"defaultBranchId":target}));
                    if moves == 1 {
                        moved.send(()).unwrap();
                    } else {
                        back.send(()).unwrap();
                    }
                }
                "session.branch.get" => {
                    assert_eq!(branch, "line-1");
                    wire.reply(&request, entry("line-1", "main"));
                }
                "session.history" => wire.reply(&request, history(8, vec![])),
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    sized(&mut command, 110, 34);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Workspace write");

    terminal.write(b"/lines\r");
    terminal.until("Main line");
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    move_seen.recv_timeout(PATIENCE).unwrap();

    // The panel stays open across the move; the root becomes the centre again
    // and its policy is read anew.
    terminal.write(b"\x1b[A");
    terminal.write(b"p");
    back_seen.recv_timeout(PATIENCE).unwrap();
    let from = terminal.consumed;
    new_seen.recv_timeout(PATIENCE).unwrap();
    terminal.until_anywhere("⏵⏵ Read only");

    release_old.send(()).unwrap();
    terminal.until_anywhere("after the late policy");
    let painted = terminal.painted_rows(from);
    assert!(
        painted.iter().any(|row| row.contains("⏵⏵ Read only")),
        "the new centre's own policy is shown: {painted:?}"
    );
    assert!(
        !painted.iter().any(|row| row.contains("⏵⏵ Full access")),
        "the earlier line's late reply does not replace it: {painted:?}"
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A move whose new centre's policy read is refused shows that line's policy as
/// unavailable: the outgoing line's preset is not left in the status line or in
/// the Permissions panel (design §14).
#[test]
fn tui_a_moved_line_with_a_refused_policy_read_shows_no_outgoing_preset() {
    let home = Home::new();
    let (moved, move_seen) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.permission.get",
            "session.permission.update",
            "session.context.usage",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        let root = next(wire);
        assert_eq!(root["method"], "session.history");
        wire.reply(&root, history(8, vec![]));
        let mut usage = |_request: &Value| None;
        loop {
            let request = next_usage(wire, &mut usage);
            let branch = request["params"]["branchId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.permission.get" => match branch {
                    "main" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    ),
                    "line-1" => {
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                            "code":-32000,"message":"fixture denied",
                            "data":{"appError":"denied"}}}));
                        moved.send(()).unwrap();
                        break;
                    }
                    other => panic!("permission read on {other}"),
                },
                "session.branch_set_default" => {
                    assert_eq!(branch, "line-1");
                    wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                }
                "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                "session.history" => {
                    let through = if branch == "line-1" { 3 } else { 8 };
                    wire.reply(&request, history(through, vec![]));
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
        wire.hold();
    });
    let mut command = home.command();
    sized(&mut command, 110, 34);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Workspace write");

    terminal.write(b"/lines\r");
    terminal.until("Main line");
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    move_seen.recv_timeout(PATIENCE).unwrap();
    // The panel keeps its selection across the move; Esc returns the keyboard
    // to the composer.
    terminal.write(b"\x1b");
    terminal.until("permission: unavailable");
    // A keystroke repaints every row from the state the failed read left.
    terminal.write(b"probe");
    terminal.until("probe");
    terminal.write(b"\x7f\x7f\x7f\x7f\x7f");

    let shown = strip_ansi(&terminal.transcript);
    let footer = shown.rsplit("⏵⏵").next().expect("the footer was painted");
    assert!(
        footer.trim_start().starts_with("Permission unavailable"),
        "the moved line's policy is unknown, not the outgoing preset: {footer}"
    );

    terminal.write(b"/permissions\r");
    terminal.until("Permission mode");
    let shown = strip_ansi(&terminal.transcript);
    let panel = &shown[shown.rfind("Permission mode").expect("the panel opened")..];
    assert!(
        !panel.contains("(*)"),
        "the panel marks no outgoing preset as current: {panel}"
    );

    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A send-now pressed before a queued move reaches the centre stops the line
/// it was pressed on, and its draft goes back to the composer rather than to
/// the line the centre moved to (design §14).
#[test]
fn tui_send_now_keeps_its_draft_when_a_queued_move_reaches_the_centre_first() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (move_seen, moved) = std::sync::mpsc::channel();
        let (release_move, release) = std::sync::mpsc::channel::<()>();
        let (stop, stop_seen) = std::sync::mpsc::channel::<Value>();
        let (submit, submit_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
                "session.permission.get",
                "session.permission.update",
            ]);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            record["running"] = json!(true);
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(
                &branches,
                directory("main", &[("main", None), ("line-1", Some("main"))]),
            );
            loop {
                let request = next(wire);
                match request["method"].as_str().unwrap_or_default() {
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.history" => {
                        wire.reply(&request, history(8, vec![]));
                        break;
                    }
                    other => panic!("unexpected opening request: {other}: {request}"),
                }
            }
            let mut usage = |_request: &Value| None;
            let mut announced = false;
            loop {
                let request = next_usage(wire, &mut usage);
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.permission.get" => {
                        if !announced && branch == "main" {
                            announced = true;
                            wire.notice(
                                "session.state",
                                json!({"sessionId":"session","branchId":"main","running":true}),
                            );
                        }
                        wire.reply(
                            &request,
                            json!({"version":1,"policy":policy(if branch == "line-1" {
                                "full-access"
                            } else {
                                "workspace-write"
                            })}),
                        )
                    }
                    "session.branch_set_default" => {
                        assert_eq!(branch, "line-1");
                        move_seen.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    }
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                    "session.history" => {
                        let through = if branch == "line-1" { 3 } else { 8 };
                        wire.reply(&request, history(through, vec![]));
                    }
                    "session.stop" => {
                        stop.send(request["params"].clone()).unwrap();
                        wire.reply(&request, json!({"sequence":9}));
                    }
                    "session.submit" => {
                        submit.send(request["params"].clone()).unwrap();
                        wire.reply(&request, json!({"event":{}}));
                        wire.hold();
                        return;
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.until("⏵⏵ Workspace write");
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        terminal.write(b"\x1b[B");
        terminal.write(b"p");
        moved.recv_timeout(PATIENCE).unwrap();
        // The panel can close before the move replies: Esc returns the keys to
        // the composer while the move is still in flight.
        terminal.write(b"\x1b[27u");
        terminal.write(b"carry on");
        terminal.until("carry on");
        terminal.write(b"\x1b[13;5u");
        release_move.send(()).unwrap();
        let stopped = stop_seen.recv_timeout(PATIENCE).unwrap();
        assert_eq!(
            stopped["branchId"], "main",
            "the stop stays on the line the gesture started on"
        );
        // The cancelled submit reports itself; the draft is restored rather
        // than written into the line the centre moved to.
        terminal.until("The centre moved before the command ran");
        terminal.write(b"\r");
        let sent = submit_seen.recv_timeout(PATIENCE).unwrap();
        assert_eq!(
            sent["branchId"], "line-1",
            "the kept draft goes to the line the operator sees"
        );
        assert_eq!(sent["text"], "carry on", "the whole draft is kept");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A draft submitted while a move is in flight is not written into the line
/// the centre moved to: it is kept and reported, and a second Enter sends it
/// to the line on screen (design §14).
#[test]
fn tui_a_draft_typed_before_a_queued_move_does_not_go_to_the_new_centre() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (move_seen, moved) = std::sync::mpsc::channel();
        let (release_move, release) = std::sync::mpsc::channel::<()>();
        let (probe, probe_seen) = std::sync::mpsc::channel::<Vec<Value>>();
        let (submit, submit_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
                "session.permission.get",
                "session.permission.update",
            ]);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(
                &branches,
                directory("main", &[("main", None), ("line-1", Some("main"))]),
            );
            loop {
                let request = next(wire);
                match request["method"].as_str().unwrap_or_default() {
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.history" => {
                        wire.reply(&request, history(8, vec![]));
                        break;
                    }
                    other => panic!("unexpected opening request: {other}: {request}"),
                }
            }
            let mut usage = |_request: &Value| None;
            let mut submits: Vec<Value> = Vec::new();
            let mut probed = false;
            loop {
                let request = next_usage(wire, &mut usage);
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy(if branch == "line-1" {
                            "full-access"
                        } else {
                            "workspace-write"
                        })}),
                    ),
                    "session.branch_set_default" => {
                        assert_eq!(branch, "line-1");
                        move_seen.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    }
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                    "session.history" => {
                        let through = if branch == "line-1" { 3 } else { 8 };
                        wire.reply(&request, history(through, vec![]));
                    }
                    "session.submit" => {
                        submits.push(request["params"].clone());
                        wire.reply(&request, json!({"event":{}}));
                        if probed {
                            submit.send(request["params"].clone()).unwrap();
                            wire.hold();
                            return;
                        }
                    }
                    "session.permission.update" if !probed => {
                        probed = true;
                        probe.send(submits.clone()).unwrap();
                        wire.reply(&request, json!({"version":2}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.until("⏵⏵ Workspace write");
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        terminal.write(b"\x1b[B");
        terminal.write(b"p");
        moved.recv_timeout(PATIENCE).unwrap();
        terminal.write(b"\x1b[27u");
        terminal.write(b"hello");
        terminal.until("hello");
        terminal.write(b"\r");
        release_move.send(()).unwrap();
        terminal.until_anywhere("⏵⏵ Full access");
        // A command queued after the draft's submit lets the daemon report
        // which submits the submit decision let out.
        terminal.write(b"\x1b[Z");
        let before = probe_seen.recv_timeout(PATIENCE).unwrap();
        assert_eq!(
            before,
            Vec::<Value>::new(),
            "the draft typed on the outgoing line is not submitted to the new one"
        );
        terminal.write(b"\r");
        let sent = submit_seen.recv_timeout(PATIENCE).unwrap();
        assert_eq!(sent["branchId"], "line-1");
        assert_eq!(sent["text"], "hello", "the kept draft is whole");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// `/resume` into a Session whose policy read is refused shows that Session's
/// policy as unavailable: the outgoing Session's preset is not left in the
/// status line, Settings or the Permissions panel (design §14).
#[test]
fn tui_resuming_into_a_refused_policy_read_shows_no_outgoing_preset() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.catalog",
            "session.branches",
            "session.permission.get",
            "session.permission.update",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let mut usage = |_request: &Value| None;
        loop {
            let request = next_usage(wire, &mut usage);
            let session = request["params"]["sessionId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.branches" => wire.reply(&request, directory("main", &[("main", None)])),
                "session.history" => wire.reply(&request, history(8, vec![])),
                "session.permission.get" => {
                    if session == "other" {
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                            "code":-32000,"message":"fixture denied",
                            "data":{"appError":"denied"}}}));
                    } else {
                        wire.reply(
                            &request,
                            json!({"version":1,"policy":policy("full-access")}),
                        );
                    }
                }
                "session.catalog" => wire.reply(
                    &request,
                    json!({"sessions":[{"sessionId":"other","cwd":"/workspace",
                        "preview":"second conversation",
                        "model":{"providerId":"test","modelId":"model"},"updatedAt":9}]}),
                ),
                "session.detach" => wire.reply(&request, json!({})),
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                    wire.reply(&request, record);
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 110, 34);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Full access");
    terminal.write(b"/resume\r");
    terminal.until("second conversation");
    terminal.write(b"\r");
    terminal.until("permission: unavailable");
    // A keystroke repaints the status line from the state the failed read
    // left.
    terminal.write(b"probe");
    terminal.until("probe");
    terminal.write(b"\x7f\x7f\x7f\x7f\x7f");
    let shown = strip_ansi(&terminal.transcript);
    let footer = shown.rsplit("⏵⏵").next().expect("the footer was painted");
    assert!(
        footer.trim_start().starts_with("Permission unavailable"),
        "the outgoing Session's preset is not left in the status line: {footer}"
    );

    terminal.write(b"/permissions\r");
    terminal.until("Permission mode");
    let shown = strip_ansi(&terminal.transcript);
    let panel = &shown[shown.rfind("Permission mode").expect("the panel opened")..];
    assert!(
        !panel.contains("(*)"),
        "the panel marks no outgoing preset as current: {panel}"
    );
    terminal.write(b"\x1b[27u");
    terminal.write(b"/settings\r");
    terminal.until_row("Permission", "Unavailable");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// `/compact` typed before a queued move reaches the centre compacts nothing:
/// the command carries the line it was typed on and is cancelled once that
/// line has moved, instead of compacting the new centre (design §14).
#[test]
fn tui_compact_typed_before_a_queued_move_compacts_nothing() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (move_seen, moved) = std::sync::mpsc::channel();
        let (release_move, release) = std::sync::mpsc::channel::<()>();
        let (compact, compact_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
                "session.permission.get",
            ]);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(
                &branches,
                directory("main", &[("main", None), ("line-1", Some("main"))]),
            );
            loop {
                let request = next(wire);
                match request["method"].as_str().unwrap_or_default() {
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.history" => {
                        wire.reply(&request, history(8, vec![]));
                        break;
                    }
                    other => panic!("unexpected opening request: {other}: {request}"),
                }
            }
            let mut usage = |_request: &Value| None;
            loop {
                let request = next_usage(wire, &mut usage);
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy(if branch == "line-1" {
                            "full-access"
                        } else {
                            "workspace-write"
                        })}),
                    ),
                    "session.branch_set_default" => {
                        assert_eq!(branch, "line-1");
                        move_seen.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    }
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                    "session.history" => {
                        let through = if branch == "line-1" { 3 } else { 8 };
                        wire.reply(&request, history(through, vec![]));
                    }
                    "session.context.compact" => {
                        compact.send(request["params"].clone()).unwrap();
                        wire.reply(&request, json!({"status":"completed"}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.until("⏵⏵ Workspace write");
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        terminal.write(b"\x1b[B");
        terminal.write(b"p");
        moved.recv_timeout(PATIENCE).unwrap();
        // The panel closes while the move is still in flight, so the command
        // is typed and submitted before the centre changes.
        terminal.write(b"\x1b[27u");
        terminal.write(b"/compact");
        terminal.until("/compact");
        terminal.write(b"\r");
        // The echo of a later key proves the interface handled Enter and sent
        // the command while the centre was still the line it showed; the move
        // replies only after that.
        terminal.write(b"probe");
        terminal.until("probe");
        release_move.send(()).unwrap();
        terminal.until("The centre moved before /compact ran");
        assert!(
            compact_seen.try_recv().is_err(),
            "the line the centre moved to is not compacted"
        );
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// `/usage` typed before a queued move reaches the centre reports nothing: the
/// command carries the default branch it was typed on and is cancelled once
/// that line has moved, instead of reading the new centre's usage (design §14).
#[test]
fn tui_usage_typed_before_a_queued_move_reads_no_new_centre() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (move_seen, moved) = std::sync::mpsc::channel();
        let (release_move, release) = std::sync::mpsc::channel::<()>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
                "session.permission.get",
            ]);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(
                &branches,
                directory("main", &[("main", None), ("line-1", Some("main"))]),
            );
            loop {
                let request = next(wire);
                match request["method"].as_str().unwrap_or_default() {
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.history" => {
                        wire.reply(&request, history(8, vec![]));
                        break;
                    }
                    other => panic!("unexpected opening request: {other}: {request}"),
                }
            }
            let mut usage = |_request: &Value| Some(reading(1849.0));
            loop {
                let request = next_usage(wire, &mut usage);
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy(if branch == "line-1" {
                            "full-access"
                        } else {
                            "workspace-write"
                        })}),
                    ),
                    "session.branch_set_default" => {
                        assert_eq!(branch, "line-1");
                        move_seen.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    }
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                    "session.history" => {
                        let through = if branch == "line-1" { 3 } else { 8 };
                        wire.reply(&request, history(through, vec![]));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.until("⏵⏵ Workspace write");
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        terminal.write(b"\x1b[B");
        terminal.write(b"p");
        moved.recv_timeout(PATIENCE).unwrap();
        // The panel closes while the move is still in flight, so the command
        // is typed and submitted before the centre changes.
        terminal.write(b"\x1b[27u");
        terminal.write(b"/usage");
        terminal.until("/usage");
        terminal.write(b"\r");
        // The echo of a later key proves the interface handled Enter and sent
        // the command while the centre was still the line it showed; the move
        // replies only after that.
        terminal.write(b"probe");
        terminal.until("probe");
        release_move.send(()).unwrap();
        terminal.until("The centre moved before /usage ran");
        let shown = strip_ansi(&terminal.transcript);
        assert!(
            !shown.contains("Context:"),
            "the cancelled command prints no usage report: {shown}"
        );
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// `/earlier` typed before a queued move reaches the centre loads nothing from
/// the new centre: the command carries the line it was typed on and is
/// cancelled once that line has moved (design §14).
#[test]
fn tui_earlier_typed_before_a_queued_move_loads_no_new_centre() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (move_seen, moved) = std::sync::mpsc::channel();
        let (release_move, release) = std::sync::mpsc::channel::<()>();
        let (earlier, earlier_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.methods",
                "session.branches",
                "session.branch.get",
                "session.branch_set_default",
                "session.permission.get",
            ]);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            let branches = next(wire);
            assert_eq!(branches["method"], "session.branches");
            wire.reply(
                &branches,
                directory("main", &[("main", None), ("line-1", Some("main"))]),
            );
            let root = next(wire);
            assert_eq!(root["method"], "session.history");
            // The drawn top holds an older page, so both TUI modes ask for it.
            let mut page = history(8, vec![said(2, "root window")]);
            page["nextBefore"] = json!(2);
            wire.reply(&root, page);
            let mut usage = |_request: &Value| None;
            loop {
                let request = next_usage(wire, &mut usage);
                let branch = request["params"]["branchId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy(if branch == "line-1" {
                            "full-access"
                        } else {
                            "workspace-write"
                        })}),
                    ),
                    "session.branch_set_default" => {
                        assert_eq!(branch, "line-1");
                        move_seen.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                    }
                    "session.branches" => wire.reply(
                        &request,
                        directory("main", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                    "session.history" => {
                        let through = if branch == "line-1" { 3 } else { 8 };
                        if request["params"].get("before").is_some() {
                            earlier.send(request["params"].clone()).unwrap();
                        }
                        wire.reply(&request, history(through, vec![]));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.until("⏵⏵ Workspace write");
        terminal.write(b"/lines\r");
        terminal.until("Main line");
        terminal.write(b"\x1b[B");
        terminal.write(b"p");
        moved.recv_timeout(PATIENCE).unwrap();
        // The panel closes while the move is still in flight, so the command
        // is typed and submitted before the centre changes.
        terminal.write(b"\x1b[27u");
        terminal.write(b"/earlier");
        terminal.until("/earlier");
        terminal.write(b"\r");
        // The echo of a later key proves the interface handled Enter and sent
        // the command while the centre was still the line it showed; the move
        // replies only after that.
        terminal.write(b"probe");
        terminal.until("probe");
        release_move.send(()).unwrap();
        terminal.until("The centre moved before /earlier ran");
        assert!(
            earlier_seen.try_recv().is_err(),
            "the line the centre moved to loads no earlier page"
        );
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A permission change typed before a queued move reaches the centre changes
/// nothing: the gesture names the default line it saw, and the change is
/// cancelled once that line has moved (design §14).
#[test]
fn tui_a_permission_change_typed_before_a_queued_move_changes_nothing() {
    let home = Home::new();
    let (move_seen, moved) = std::sync::mpsc::channel();
    let (release_move, release) = std::sync::mpsc::channel::<()>();
    let (changed, updates) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.branches",
            "session.branch.get",
            "session.branch_set_default",
            "session.permission.get",
            "session.permission.update",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(
            &branches,
            directory("main", &[("main", None), ("line-1", Some("main"))]),
        );
        loop {
            let request = next(wire);
            match request["method"].as_str().unwrap_or_default() {
                "session.branches" => wire.reply(
                    &request,
                    directory("main", &[("main", None), ("line-1", Some("main"))]),
                ),
                "session.history" => {
                    wire.reply(&request, history(8, vec![]));
                    break;
                }
                other => panic!("unexpected opening request: {other}: {request}"),
            }
        }
        let mut usage = |_request: &Value| None;
        loop {
            let request = next_usage(wire, &mut usage);
            let branch = request["params"]["branchId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.permission.get" => wire.reply(
                    &request,
                    json!({"version":1,"policy":policy(if branch == "line-1" {
                        "full-access"
                    } else {
                        "workspace-write"
                    })}),
                ),
                "session.permission.update" => {
                    changed.send(request["params"].clone()).unwrap();
                    wire.reply(&request, json!({"version":2}));
                }
                "session.branch_set_default" => {
                    assert_eq!(branch, "line-1");
                    move_seen.send(()).unwrap();
                    release.recv_timeout(PATIENCE).unwrap();
                    wire.reply(&request, json!({"defaultBranchId":"line-1"}));
                }
                "session.branches" => wire.reply(
                    &request,
                    directory("main", &[("main", None), ("line-1", Some("main"))]),
                ),
                "session.branch.get" => wire.reply(&request, entry("line-1", "main")),
                "session.history" => {
                    let through = if branch == "line-1" { 3 } else { 8 };
                    wire.reply(&request, history(through, vec![]));
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 120, 36);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Workspace write");
    terminal.write(b"/lines\r");
    terminal.until("Main line");
    terminal.write(b"\x1b[B");
    terminal.write(b"p");
    moved.recv_timeout(PATIENCE).unwrap();
    // The panel closes before the move replies, so the keys that follow act on
    // the view that still shows the outgoing default line.
    terminal.write(b"\x1b[27u");
    terminal.write(b"\x1b[Z");
    release_move.send(()).unwrap();
    terminal.until("The session or line moved before the permission change ran");
    assert!(
        updates.try_recv().is_err(),
        "the permission change typed on the outgoing default line writes nothing"
    );
    // A second gesture names the line the centre moved to; its update is the
    // only one the daemon sees.
    terminal.write(b"\x1b[Z");
    let written = updates.recv_timeout(PATIENCE).unwrap();
    assert_eq!(written["branchId"], "line-1");
    assert!(
        updates.try_recv().is_err(),
        "the cancelled change and the later gesture do not both write"
    );
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A draft submitted before a Session handoff replies is not written into the
/// Session that replaces it, even when both call their line `main`: it is
/// kept, reported, and a second Enter sends it to the Session on screen
/// (design §14).
#[test]
fn tui_a_draft_typed_before_a_session_handoff_does_not_go_to_the_new_session() {
    let home = Home::new();
    let (attached, attach_seen) = std::sync::mpsc::channel();
    let (release_attach, release) = std::sync::mpsc::channel::<()>();
    let (probe, probed) = std::sync::mpsc::channel::<Vec<Value>>();
    let (submit, submit_seen) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.methods",
            "session.catalog",
            "session.branches",
            "session.permission.get",
            "session.permission.update",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        let branches = next(wire);
        assert_eq!(branches["method"], "session.branches");
        wire.reply(&branches, directory("main", &[("main", None)]));
        let history_read = next(wire);
        assert_eq!(history_read["method"], "session.history");
        wire.reply(&history_read, history(8, vec![]));
        let mut usage = |_request: &Value| None;
        let mut submits: Vec<Value> = Vec::new();
        loop {
            let request = next_usage(wire, &mut usage);
            let session = request["params"]["sessionId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.catalog" => wire.reply(
                    &request,
                    json!({"sessions":[{"sessionId":"other","cwd":"/workspace",
                        "preview":"second conversation",
                        "model":{"providerId":"test","modelId":"model"},"updatedAt":9}]}),
                ),
                "session.detach" => wire.reply(&request, json!({})),
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                    if session == "other" {
                        attached.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                    }
                    wire.reply(&request, record);
                }
                "session.branches" => wire.reply(&request, directory("main", &[("main", None)])),
                "session.history" => wire.reply(&request, history(8, vec![])),
                "session.permission.get" => wire.reply(
                    &request,
                    json!({"version":1,"policy":policy("workspace-write")}),
                ),
                "session.permission.update" => {
                    probe.send(submits.clone()).unwrap();
                    wire.reply(&request, json!({"version":2}));
                }
                "session.submit" => {
                    submits.push(request["params"].clone());
                    wire.reply(&request, json!({"event":{}}));
                    submit.send(request["params"].clone()).unwrap();
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 120, 36);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Workspace write");
    terminal.write(b"/resume\r");
    terminal.until("second conversation");
    terminal.write(b"\r");
    attach_seen.recv_timeout(PATIENCE).unwrap();
    // The new attachment has not replied, so the interface still shows the
    // outgoing Session: this draft belongs to that Session's view.
    terminal.write(b"hello");
    terminal.until("hello");
    terminal.write(b"\r");
    release_attach.send(()).unwrap();
    terminal.until("The centre moved before the command ran");
    // The permission read the probe starts flushes every command queued ahead
    // of it; no submit may come out before that.
    terminal.write(b"\x1b[Z");
    let before = probed.recv_timeout(PATIENCE).unwrap();
    assert!(
        before.is_empty(),
        "a draft typed in the outgoing Session is not submitted to the new one"
    );
    terminal.write(b"\r");
    let sent = submit_seen.recv_timeout(PATIENCE).unwrap();
    assert_eq!(sent["sessionId"], "other");
    assert_eq!(sent["branchId"], "main");
    assert_eq!(sent["text"], "hello", "the kept draft is whole");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// An interrupt pressed while a Session handoff is still in flight does not
/// stop the Session that replaces it: the gesture names the Session and line
/// it saw, and a handoff cancels it instead (design §14). A held `session.fork`
/// is the command in front of the handoff, so the interface still shows the
/// outgoing Session and its run when Esc is pressed.
#[test]
fn tui_an_interrupt_typed_before_a_session_handoff_does_not_stop_the_new_session() {
    let home = Home::new();
    let (forked, fork_seen) = std::sync::mpsc::channel();
    let (release_fork, release) = std::sync::mpsc::channel::<()>();
    let (updated, update_seen) = std::sync::mpsc::channel::<Value>();
    let (stop, stop_seen) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.branches",
            "session.fork",
            "session.permission.get",
            "session.permission.update",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":true}),
        );
        let mut usage = |_request: &Value| None;
        loop {
            let request = next_usage(wire, &mut usage);
            let session = request["params"]["sessionId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.branches" => wire.reply(&request, directory("main", &[("main", None)])),
                "session.history" if session == "forked" => wire.reply(
                    &request,
                    history(8, vec![said(3, "after the fork"), applied(4, "input-3")]),
                ),
                "session.history" => wire.reply(&request, history(8, vec![])),
                "session.permission.get" => wire.reply(
                    &request,
                    json!({"version":1,"policy":policy("workspace-write")}),
                ),
                "session.fork" => {
                    forked.send(()).unwrap();
                    release.recv_timeout(PATIENCE).unwrap();
                    wire.reply(&request, json!({"sessionId":"forked"}));
                }
                "session.detach" => wire.reply(&request, json!({})),
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                    wire.reply(&request, record);
                }
                "session.permission.update" => {
                    updated.send(request["params"].clone()).unwrap();
                    wire.reply(&request, json!({"version":2}));
                }
                "session.stop" => {
                    stop.send(request["params"].clone()).unwrap();
                    wire.reply(&request, json!({"sequence":9}));
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 120, 36);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Workspace write");
    terminal.until("esc interrupt");
    terminal.write(b"/fork\r");
    terminal.until("Fork the session");
    terminal.write(b"y");
    fork_seen.recv_timeout(PATIENCE).unwrap();
    // The fork's reply is held, so the interface still shows the outgoing
    // Session's running turn: this Esc belongs to it.
    terminal.write(b"\x1b[27u");
    terminal.until("Stopping");
    release_fork.send(()).unwrap();
    terminal.until("after the fork");
    // The permission change the probe starts flushes every command queued
    // ahead of it; the stop must have been decided by then.
    terminal.write(b"\x1b[Z");
    let changed = update_seen.recv_timeout(PATIENCE).unwrap();
    assert_eq!(changed["sessionId"], "forked");
    assert!(
        stop_seen.try_recv().is_err(),
        "the interrupt pressed on the outgoing Session does not stop the new one"
    );
    terminal.until("The session moved before the stop ran");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// The `Ctrl+Enter` gesture is on the same boundary: a handoff while its stop
/// is still queued cancels the stop without wedging the send key, and a later
/// gesture reaches the Session on screen (design §14).
#[test]
fn tui_a_send_now_typed_before_a_session_handoff_does_not_stop_the_new_session() {
    let home = Home::new();
    let (forked, fork_seen) = std::sync::mpsc::channel();
    let (release_fork, release) = std::sync::mpsc::channel::<()>();
    let (stopped, stop_seen) = std::sync::mpsc::channel::<Value>();
    let (submitted, submit_seen) = std::sync::mpsc::channel::<Value>();
    let (updated, update_seen) = std::sync::mpsc::channel::<Value>();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.branches",
            "session.fork",
            "session.permission.get",
            "session.permission.update",
        ]);
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
        wire.reply(&attach, record);
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":true}),
        );
        let mut usage = |_request: &Value| None;
        loop {
            let request = next_usage(wire, &mut usage);
            let session = request["params"]["sessionId"].as_str().unwrap_or_default();
            match request["method"].as_str().unwrap_or_default() {
                "session.branches" => wire.reply(&request, directory("main", &[("main", None)])),
                "session.history" if session == "forked" => wire.reply(
                    &request,
                    history(8, vec![said(3, "after the fork"), applied(4, "input-3")]),
                ),
                "session.history" => wire.reply(&request, history(8, vec![])),
                "session.permission.get" => wire.reply(
                    &request,
                    json!({"version":1,"policy":policy("workspace-write")}),
                ),
                "session.fork" => {
                    forked.send(()).unwrap();
                    release.recv_timeout(PATIENCE).unwrap();
                    wire.reply(&request, json!({"sessionId":"forked"}));
                    wire.notice(
                        "session.state",
                        json!({"sessionId":"forked","branchId":"main","running":true}),
                    );
                }
                "session.detach" => wire.reply(&request, json!({})),
                "session.attach" => {
                    let mut record = attach_record(&request);
                    record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                    wire.reply(&request, record);
                }
                "session.stop" => {
                    stopped.send(request["params"].clone()).unwrap();
                    wire.reply(&request, json!({"sequence":9}));
                }
                "session.permission.update" => {
                    updated.send(request["params"].clone()).unwrap();
                    wire.reply(&request, json!({"version":2}));
                }
                "session.submit" => {
                    submitted.send(request["params"].clone()).unwrap();
                    wire.reply(&request, json!({"event":{}}));
                    wire.hold();
                    return;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 120, 36);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.until("⏵⏵ Workspace write");
    terminal.until("esc interrupt");
    terminal.write(b"/fork\r");
    terminal.until("Fork the session");
    terminal.write(b"y");
    fork_seen.recv_timeout(PATIENCE).unwrap();
    // The fork's reply is held: the draft and the gesture that stops its run
    // belong to the Session still on screen.
    terminal.write(b"carry on");
    terminal.until("carry on");
    terminal.write(b"\x1b[13;5u");
    terminal.write(b"x");
    terminal.until("carry onx");
    let painted_from = terminal.transcript.len();
    release_fork.send(()).unwrap();
    terminal.until("after the fork");
    // The permission change the probe starts flushes every command queued
    // ahead of it; the send-now stop must have been decided by then.
    terminal.write(b"\x1b[Z");
    let changed = update_seen.recv_timeout(PATIENCE).unwrap();
    assert_eq!(changed["sessionId"], "forked");
    assert!(
        stop_seen.try_recv().is_err(),
        "the send-now stop pressed on the outgoing Session does not stop the new one"
    );
    terminal.until("Send now failed: The session moved before the send ran");
    terminal.until("esc interrupt");
    let notice = strip_ansi(&terminal.transcript[painted_from..]);
    assert!(
        !notice
            .contains("Send now failed: The session moved before the send ran; the draft was kept"),
        "the handoff cleared the composer, so the notice must not claim the draft was kept"
    );
    terminal.write(b"again");
    terminal.until("again");
    terminal.write(b"\x1b[13;5u");
    let second = stop_seen.recv_timeout(PATIENCE).unwrap();
    assert_eq!(second["sessionId"], "forked");
    assert_eq!(second["branchId"], "main");
    let sent = submit_seen.recv_timeout(PATIENCE).unwrap();
    assert_eq!(sent["sessionId"], "forked");
    assert_eq!(sent["text"], "again");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}

/// A message and a permission change typed on Home while a `/resume` is still
/// attaching belong to Home: neither is submitted to nor written on the
/// Session that attachment brings in (design §14).
#[test]
fn tui_a_home_send_and_permission_change_do_not_reach_the_resumed_session() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (attaching, attach_seen) = std::sync::mpsc::channel();
        let (release_attach, release) = std::sync::mpsc::channel::<()>();
        let (submitted, submit_seen) = std::sync::mpsc::channel::<Value>();
        let (updated, update_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.catalog",
                "session.branches",
                "session.permission.get",
                "session.permission.update",
            ]);
            let mut usage = |_request: &Value| None;
            let mut version = 1;
            loop {
                let request = next_usage(wire, &mut usage);
                let session = request["params"]["sessionId"].as_str().unwrap_or_default();
                match request["method"].as_str().unwrap_or_default() {
                    "session.catalog" => wire.reply(
                        &request,
                        json!({"sessions":[{"sessionId":"other","cwd":"/workspace",
                            "preview":"second conversation","model":{"providerId":"test","modelId":"model"},
                            "updatedAt":9}]}),
                    ),
                    "session.attach" => {
                        assert_eq!(request["params"]["sessionId"], "other");
                        attaching.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        let mut record = attach_record(&request);
                        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                        wire.reply(&request, record);
                    }
                    "session.branches" => {
                        wire.reply(&request, directory("main", &[("main", None)]))
                    }
                    "session.history" => wire.reply(
                        &request,
                        history(
                            8,
                            vec![said(3, "resumed conversation"), applied(4, "input-3")],
                        ),
                    ),
                    "session.permission.get" => {
                        let preset = if version == 1 { "read-only" } else { "workspace-write" };
                        wire.reply(
                            &request,
                            json!({"version":version,"policy":policy(preset)}),
                        );
                    }
                    "session.permission.update" => {
                        assert_eq!(session, "other");
                        updated.send(request["params"].clone()).unwrap();
                        version += 1;
                        wire.reply(&request, json!({"version":version}));
                    }
                    "session.submit" => {
                        submitted.send(request["params"].clone()).unwrap();
                        wire.reply(&request, json!({"event":{}}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("Ask anything");
        terminal.write(b"/resume\r");
        terminal.until("second conversation");
        terminal.write(b"\r");
        attach_seen.recv_timeout(PATIENCE).unwrap();
        // The attachment has not replied, so the interface still shows Home:
        // this message and Shift-Tab belong to it, not to the Session coming.
        terminal.write(b"hello\r");
        terminal.write(b"\x1b[Z");
        // Typed after the gestures, so reading it proves the interface has
        // taken both before the attachment is released.
        terminal.write(b"qzx");
        terminal.until("qzx");
        release_attach.send(()).unwrap();
        terminal.until("resumed conversation");
        // The permission change the probe starts flushes every command queued
        // ahead of it: the Home send and Shift-Tab must be decided by then.
        terminal.write(b"\x1b[Z");
        let changed = update_seen.recv_timeout(PATIENCE).unwrap();
        assert_eq!(changed["sessionId"], "other");
        assert_eq!(changed["branchId"], "main");
        assert_eq!(
            changed["expectedVersion"], 1,
            "the Home gesture must not have written the resumed line first"
        );
        assert!(
            submit_seen.try_recv().is_err(),
            "a message typed on Home is not submitted to the resumed Session"
        );
        // The two notices may have been painted in the frame that carried
        // the resumed view, before the title echo this flow waited on.
        terminal.until_anywhere("the draft was kept");
        terminal.until_anywhere("nothing was changed");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A send left over from the Session `/new` left does not start a Session at
/// Home, and is not submitted there: the command names the Session it was
/// typed in, not Home (design §14).
#[test]
fn tui_a_stale_send_behind_new_does_not_start_a_session() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (forking, fork_seen) = std::sync::mpsc::channel();
        let (release_fork, release) = std::sync::mpsc::channel::<()>();
        let (created, create_seen) = std::sync::mpsc::channel::<Value>();
        let (submitted, submit_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.branches",
                "session.fork",
                "session.permission.get",
                "session.permission.update",
            ]);
            let mut usage = |_request: &Value| None;
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            loop {
                let request = next_usage(wire, &mut usage);
                match request["method"].as_str().unwrap_or_default() {
                    "session.branches" => {
                        wire.reply(&request, directory("main", &[("main", None)]))
                    }
                    "session.history" => wire.reply(
                        &request,
                        history(
                            8,
                            vec![
                                said(3, "the first turn"),
                                applied(4, "input-3"),
                                json!({"id":"end-5","sequence":5,"create_at":1,"type":"turn",
                                    "name":"end","data":{"turn_id":"turn","status":"completed",
                                    "reason":"done"}}),
                            ],
                        ),
                    ),
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    ),
                    "session.permission.update" => wire.reply(&request, json!({"version":2})),
                    "session.fork" => {
                        forking.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, json!({"sessionId":"forked"}));
                    }
                    "session.attach" => {
                        let mut record = attach_record(&request);
                        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                        wire.reply(&request, record);
                    }
                    "session.detach" => wire.reply(&request, json!({})),
                    "session.create" => {
                        created.send(request["params"].clone()).unwrap();
                        wire.reply(
                            &request,
                            json!({"sessionId":"unexpected",
                                "model":{"providerId":"test","modelId":"model"}}),
                        );
                    }
                    "session.submit" => {
                        submitted.send(request["params"].clone()).unwrap();
                        wire.reply(&request, json!({"event":{}}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("the first turn");
        terminal.until("test / model");
        terminal.write(b"/fork\r");
        terminal.until("Fork the session");
        terminal.write(b"y");
        fork_seen.recv_timeout(PATIENCE).unwrap();
        // The fork's reply is held, so the interface still shows the Session
        // `/new` is about to leave: this draft belongs to that Session.
        terminal.write(b"/new\r");
        terminal.write(b"stale");
        terminal.until("stale");
        terminal.write(b"\r");
        // Typed after the send, so reading it proves the interface has taken
        // the send before the fork is released.
        terminal.write(b"qzx");
        terminal.until("qzx");
        release_fork.send(()).unwrap();
        terminal.until("The centre moved before the command ran");
        assert!(
            create_seen.try_recv().is_err(),
            "a send from the Session `/new` left does not start a Session"
        );
        assert!(
            submit_seen.try_recv().is_err(),
            "a send from the Session `/new` left is not submitted at Home"
        );
        terminal.until("the draft was kept");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// The invocation's first message on a resumed Session is submitted on the
/// line the attach reply names, even when the first frame still shows the
/// line the attachment invalidated and the reply has not arrived (design §14).
#[test]
fn tui_the_first_prompt_of_a_resumed_session_follows_the_line_its_attach_names() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (attaching, attach_seen) = std::sync::mpsc::channel();
        let (release_attach, release) = std::sync::mpsc::channel::<()>();
        let (submitted, submit_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["sessionId"], "session");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"line-1","sequence":3});
            record["sequence"] = json!(3);
            attaching.send(()).unwrap();
            release.recv_timeout(PATIENCE).unwrap();
            wire.reply(&attach, record);
            let mut usage = |_request: &Value| None;
            loop {
                let request = next_usage(wire, &mut usage);
                match request["method"].as_str().unwrap_or_default() {
                    "session.history" => wire.reply(&request, history(3, vec![])),
                    "session.submit" => {
                        submitted.send(request["params"].clone()).unwrap();
                        wire.reply(&request, json!({"event":{}}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.args([
            "tui",
            if fullscreen {
                "--fullscreen"
            } else {
                "--scrollback"
            },
            "--resume",
            "session",
            "-p",
            "continue",
        ]);
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::launch_tui(command);
        attach_seen.recv_timeout(PATIENCE).unwrap();
        // The reply is held, so the first frame drains the invalidated
        // projection: the invocation's first message reads that frame's line,
        // not the line the attachment is about to name.
        terminal.until("Welcome to DIM");
        release_attach.send(()).unwrap();
        let sent = submit_seen.recv_timeout(PATIENCE).unwrap();
        assert_eq!(sent["sessionId"], "session");
        assert_eq!(
            sent["branchId"], "line-1",
            "the first message follows the attach reply's line"
        );
        assert_eq!(sent["text"], "continue");
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A permission change typed on the Home screen `--resume` shows before its
/// attachment replies is Home's: no line of the resumed Session is written
/// (design §14). The `/compact` that follows the attachment arrives after
/// that gesture is decided, and the gesture typed on the attached Session is
/// the one whose write the daemon sees.
#[test]
fn tui_a_launch_home_permission_change_does_not_reach_the_resumed_session() {
    for fullscreen in [false, true] {
        for default in ["main", "line-1"] {
            let home = Home::new();
            let (attaching, attach_seen) = std::sync::mpsc::channel();
            let (release_attach, release) = std::sync::mpsc::channel::<()>();
            let (updated, update_seen) = std::sync::mpsc::channel::<Value>();
            let (compacting, compact_seen) = std::sync::mpsc::channel::<()>();
            let daemon = home.daemon(move |wire| {
                wire.hello(&[
                    "session.branches",
                    "session.permission.get",
                    "session.permission.update",
                ]);
                let mut usage = |_request: &Value| None;
                let mut version = 1;
                loop {
                    let request = next_usage(wire, &mut usage);
                    let session = request["params"]["sessionId"].as_str().unwrap_or_default();
                    match request["method"].as_str().unwrap_or_default() {
                        "session.attach" => {
                            assert_eq!(session, "session");
                            let mut record = attach_record(&request);
                            record["defaultBranch"] = json!({"branchId":default,"sequence":8});
                            attaching.send(()).unwrap();
                            release.recv_timeout(PATIENCE).unwrap();
                            wire.reply(&request, record);
                        }
                        "session.branches" => wire.reply(
                            &request,
                            directory(default, &[("main", None), ("line-1", Some("main"))]),
                        ),
                        "session.history" => wire.reply(
                            &request,
                            history(
                                8,
                                vec![said(3, "resumed conversation"), applied(4, "input-3")],
                            ),
                        ),
                        "session.permission.get" => wire.reply(
                            &request,
                            json!({"version":version,"policy":policy("read-only")}),
                        ),
                        "session.permission.update" => {
                            updated.send(request["params"].clone()).unwrap();
                            version += 1;
                            wire.reply(&request, json!({"version":version}));
                        }
                        "session.submit" => {
                            panic!("a Home message reached the resumed Session: {request}")
                        }
                        "session.context.compact" => {
                            assert_eq!(request["params"]["branchId"], default);
                            compacting.send(()).unwrap();
                            wire.reply(&request, json!({"status":"completed"}));
                        }
                        other => panic!("unexpected {other}: {request}"),
                    }
                }
            });
            let mut command = home.command();
            command.args([
                "tui",
                if fullscreen {
                    "--fullscreen"
                } else {
                    "--scrollback"
                },
                "--resume",
                "session",
            ]);
            sized(&mut command, 120, 36);
            let mut terminal = Terminal::launch_tui(command);
            attach_seen.recv_timeout(PATIENCE).unwrap();
            // The screen is Home until the reply lands; the gesture read its
            // text, so it is decided before the attachment is released.
            terminal.until("Welcome to DIM");
            terminal.write(b"\x1b[Z");
            terminal.write(b"qzx");
            terminal.until("qzx");
            release_attach.send(()).unwrap();
            // The boundary row is out once the attachment's own projection is
            // applied, so the commands below name the resumed Session.
            terminal.until("resumed session");
            terminal.write(b"/compact\r");
            compact_seen.recv_timeout(PATIENCE).unwrap();
            assert!(
                update_seen.try_recv().is_err(),
                "the launch Home gesture writes no resumed line"
            );
            // A gesture typed after the attachment names its line: its write
            // is the only one, against the version the opening read reported.
            terminal.write(b"\x1b[Z");
            let changed = update_seen.recv_timeout(PATIENCE).unwrap();
            assert_eq!(changed["sessionId"], "session");
            assert_eq!(changed["branchId"], default);
            assert_eq!(changed["expectedVersion"], 1);
            assert!(
                update_seen.try_recv().is_err(),
                "the launch Home gesture writes no resumed line of its own"
            );
            terminal.until("nothing was changed");
            home.kill(&terminal);
            terminal.wait();
            daemon.join().unwrap();
        }
    }
}

/// A shell command typed on a Session a `/resume` is about to leave does not
/// run in the Session the attachment brings in: the command names the Session
/// and line it was typed on, and the attachment has replaced both
/// (design §14).
#[test]
fn tui_a_shell_command_typed_before_a_resume_does_not_run_in_the_new_session() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (attaching, attach_seen) = std::sync::mpsc::channel();
        let (release_attach, release) = std::sync::mpsc::channel::<()>();
        let (shells, shell_seen) = std::sync::mpsc::channel::<Value>();
        let (compacting, compact_seen) = std::sync::mpsc::channel::<()>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.catalog",
                "session.branches",
                "session.permission.get",
                "session.permission.update",
            ]);
            let mut usage = |_request: &Value| None;
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            loop {
                let request = next_usage(wire, &mut usage);
                match request["method"].as_str().unwrap_or_default() {
                    "session.catalog" => wire.reply(
                        &request,
                        json!({"sessions":[{"sessionId":"other","cwd":"/workspace",
                            "preview":"second conversation",
                            "model":{"providerId":"test","modelId":"model"},
                            "updatedAt":9}]}),
                    ),
                    "session.branches" => {
                        wire.reply(&request, directory("main", &[("main", None)]))
                    }
                    "session.history" => {
                        let session = request["params"]["sessionId"].as_str().unwrap_or_default();
                        wire.reply(
                            &request,
                            if session == "other" {
                                history(8, vec![said(3, "other conversation")])
                            } else {
                                history(8, vec![said(3, "the first turn"), applied(4, "input-3")])
                            },
                        );
                    }
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    ),
                    "session.permission.update" => wire.reply(&request, json!({"version":2})),
                    "session.attach" => {
                        assert_eq!(request["params"]["sessionId"], "other");
                        let mut record = attach_record(&request);
                        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                        attaching.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, record);
                    }
                    "session.detach" => wire.reply(&request, json!({})),
                    "shell.run" => {
                        shells.send(request["params"].clone()).unwrap();
                        wire.reply(
                            &request,
                            json!({"runId":request["params"]["runId"],
                                "status":"completed","exitCode":0,"output":"stale-output\n",
                                "truncated":false,"durationMs":1}),
                        );
                    }
                    "session.context.compact" => {
                        assert_eq!(request["params"]["branchId"], "main");
                        compacting.send(()).unwrap();
                        wire.reply(&request, json!({"status":"completed"}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("the first turn");
        terminal.until("test / model");
        terminal.write(b"/resume\r");
        terminal.until("second conversation");
        terminal.write(b"\r");
        attach_seen.recv_timeout(PATIENCE).unwrap();
        // The attachment's reply is held, so the interface still shows the
        // Session the shell command is typed on.
        terminal.write(b"!printf stale\r");
        terminal.until("Ctrl+C to interrupt");
        release_attach.send(()).unwrap();
        terminal.until("other conversation");
        // `/compact` on the attached Session is the boundary after the stale
        // command: the daemon reads it whether that command ran or not, so an
        // empty `shell.run` here is the refusal, not a race.
        terminal.write(b"/compact\r");
        compact_seen.recv_timeout(PATIENCE).unwrap();
        assert!(
            shell_seen.try_recv().is_err(),
            "a shell command from the Session a resume left does not run in the new Session"
        );
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A shell command left over from the Session `/new` left does not start a
/// Session at Home: the command names the Session it was typed in, and the
/// guard runs before Home can create one (design §14).
#[test]
fn tui_a_stale_shell_command_behind_new_does_not_start_a_session() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (attaching, attach_seen) = std::sync::mpsc::channel();
        let (release_attach, release) = std::sync::mpsc::channel::<()>();
        let (created, create_seen) = std::sync::mpsc::channel::<Value>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[
                "session.catalog",
                "session.branches",
                "session.permission.get",
                "session.permission.update",
            ]);
            let mut usage = |_request: &Value| None;
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["defaultBranch"] = json!({"branchId":"main","sequence":8});
            wire.reply(&attach, record);
            loop {
                let request = next_usage(wire, &mut usage);
                match request["method"].as_str().unwrap_or_default() {
                    "session.catalog" => wire.reply(
                        &request,
                        json!({"sessions":[{"sessionId":"other","cwd":"/workspace",
                            "preview":"second conversation",
                            "model":{"providerId":"test","modelId":"model"},
                            "updatedAt":9}]}),
                    ),
                    "session.branches" => {
                        wire.reply(&request, directory("main", &[("main", None)]))
                    }
                    "session.history" => wire.reply(
                        &request,
                        history(8, vec![said(3, "the first turn"), applied(4, "input-3")]),
                    ),
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    ),
                    "session.permission.update" => wire.reply(&request, json!({"version":2})),
                    "session.attach" => {
                        assert_eq!(request["params"]["sessionId"], "other");
                        let mut record = attach_record(&request);
                        record["defaultBranch"] = json!({"branchId":"main","sequence":8});
                        attaching.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, record);
                    }
                    "session.detach" => wire.reply(&request, json!({})),
                    "session.create" => {
                        created.send(request["params"].clone()).unwrap();
                        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                            "code":-32603,"message":"no Session was created"}}));
                    }
                    "provider.catalog" => wire.reply(&request, json!({})),
                    "shell.run" => panic!("a stale shell command runs at Home: {request}"),
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("the first turn");
        terminal.until("test / model");
        terminal.write(b"/resume\r");
        terminal.until("second conversation");
        terminal.write(b"\r");
        attach_seen.recv_timeout(PATIENCE).unwrap();
        // The attachment's reply is held, so the interface still shows the
        // Session `/new` is about to leave: this shell command belongs to it.
        terminal.write(b"/new\r");
        terminal.write(b"!printf stale\r");
        terminal.until("Ctrl+C to interrupt");
        release_attach.send(()).unwrap();
        // The refusal is a `ShellDone` the interface draws; an unfixed command
        // instead reaches `session.create` and shows its error there.
        terminal.until("Shell: ");
        assert!(
            terminal
                .transcript
                .contains("The centre moved before the shell command ran"),
            "the stale shell command is cancelled, not started at Home: {}",
            terminal.transcript
        );
        assert!(
            create_seen.try_recv().is_err(),
            "a shell command from the Session `/new` left starts no Session"
        );
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A shell command typed on the Home screen `--resume` shows before its
/// attachment replies is Home's: once the reply lands, it does not run on the
/// resumed Session's default line (design §14).
#[test]
fn tui_a_launch_home_shell_command_does_not_run_in_the_resumed_session() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (attaching, attach_seen) = std::sync::mpsc::channel();
        let (release_attach, release) = std::sync::mpsc::channel::<()>();
        let (shells, shell_seen) = std::sync::mpsc::channel::<Value>();
        let (compacting, compact_seen) = std::sync::mpsc::channel::<()>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.branches", "session.permission.get"]);
            let mut usage = |_request: &Value| None;
            loop {
                let request = next_usage(wire, &mut usage);
                match request["method"].as_str().unwrap_or_default() {
                    "session.attach" => {
                        let mut record = attach_record(&request);
                        record["defaultBranch"] = json!({"branchId":"line-1","sequence":8});
                        record["sequence"] = json!(8);
                        attaching.send(()).unwrap();
                        release.recv_timeout(PATIENCE).unwrap();
                        wire.reply(&request, record);
                    }
                    "session.branches" => wire.reply(
                        &request,
                        directory("line-1", &[("main", None), ("line-1", Some("main"))]),
                    ),
                    "session.history" => wire.reply(
                        &request,
                        history(
                            8,
                            vec![said(3, "resumed conversation"), applied(4, "input-3")],
                        ),
                    ),
                    "session.permission.get" => wire.reply(
                        &request,
                        json!({"version":1,"policy":policy("workspace-write")}),
                    ),
                    "shell.run" => {
                        shells.send(request["params"].clone()).unwrap();
                        wire.reply(
                            &request,
                            json!({"runId":request["params"]["runId"],
                                "status":"completed","exitCode":0,"output":"stale-output\n",
                                "truncated":false,"durationMs":1}),
                        );
                    }
                    "session.context.compact" => {
                        assert_eq!(request["params"]["branchId"], "line-1");
                        compacting.send(()).unwrap();
                        wire.reply(&request, json!({"status":"completed"}));
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let mut command = home.command();
        command.args([
            "tui",
            if fullscreen {
                "--fullscreen"
            } else {
                "--scrollback"
            },
            "--resume",
            "session",
        ]);
        sized(&mut command, 120, 36);
        let mut terminal = Terminal::launch_tui(command);
        attach_seen.recv_timeout(PATIENCE).unwrap();
        terminal.until("Welcome to DIM");
        // The screen is Home until the reply lands; this command is Home's.
        terminal.write(b"!printf stale\r");
        terminal.until("Ctrl+C to interrupt");
        release_attach.send(()).unwrap();
        // The boundary row is drawn from the attachment's own projection. The
        // stale command was queued before it, so `/compact` below is read only
        // after that command was decided.
        terminal.until("resumed session");
        // `/compact` is the boundary after the stale command, as in the
        // `/resume` flow above.
        terminal.write(b"/compact\r");
        compact_seen.recv_timeout(PATIENCE).unwrap();
        assert!(
            shell_seen.try_recv().is_err(),
            "a shell command typed on the launch Home does not run on the resumed line"
        );
        home.kill(&terminal);
        terminal.wait();
        daemon.join().unwrap();
    }
}
