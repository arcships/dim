//! Prompt suggestions in the terminal client: the request follows a completed
//! turn the interface witnessed, the offer is drawn in the empty composer of
//! both layouts, and the keys take it, dismiss it or leave it alone
//! ([PromptSuggestion §11](../../../../docs/design/plugins/PromptSuggestion.md)).
use super::*;

/// The `session.notice` that offers `text` for `turn`.
fn offer(wire: &mut Wire, turn: &str, text: &str) {
    wire.notice(
        "session.notice",
        json!({"sessionId":"session","source":"PromptSuggestion","kind":"offer",
            "data":{"turnId":turn,"text":text}}),
    );
}

/// The startup a terminal keeps: a resumed and attached Session holding
/// `history`, and the reads the interface makes after attaching.
fn opened(wire: &mut Wire, features: &[&str], history: Vec<Value>) {
    opened_with_methods(wire, features, history, true);
}

/// [`opened`] with a method table that lists the suggestion methods, or
/// without them.
fn opened_with_methods(wire: &mut Wire, features: &[&str], history: Vec<Value>, listed: bool) {
    let mut features = features.to_vec();
    features.push("session.methods");
    wire.hello(&features);
    let attach = wire.read("session.attach");
    wire.reply(&attach, attach_record(&attach));
    // The interface reads the method table once per Session, before
    // anything can ask for an offer or probe the settings source.
    methods(wire, listed);
    let page = wire.read("session.history");
    wire.reply(
        &page,
        json!({"events":history,"through":8,"nextAfter":8,"done":true}),
    );
    wire.2 = false;
    let (mut startup, mut owners) = (0, 0);
    while startup < 2 || owners < 3 {
        let request = wire.read_any();
        match request["method"].as_str() {
            Some("account.providers") => {
                wire.reply(&request, json!([]));
                startup += 1;
            }
            Some("session.input.pending" | "session.exec.pending" | "session.turn.get") => {
                assert!(wire.empty_state(&request));
                owners += 1;
            }
            Some("session.context.usage") => {
                startup += 1;
                wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method",
                    "data":{"appError":"unknown_method"}}}));
            }
            method => panic!("unexpected request {method:?}"),
        }
    }
    wire.2 = true;
}

/// Answers the Session's method table, listing the suggestion methods when
/// `listed` and no methods otherwise.
fn methods(wire: &mut Wire, listed: bool) {
    let request = wire.read("session.methods");
    let mut methods = if listed {
        json!([{"name":"session.suggestion.request","mutating":true},{"name":"session.suggestion.respond","mutating":true}])
    } else {
        json!([])
    };
    for name in [
        "session.input.pending",
        "session.exec.pending",
        "session.turn.get",
    ] {
        methods
            .as_array_mut()
            .unwrap()
            .push(json!({"name":name,"mutating":false}));
    }
    wire.reply(&request, json!({"methods":methods}));
    wire.2 = true;
}

/// A turn that ended `Completed` on `main`.
fn completed(wire: &mut Wire, turn: &str) {
    wire.end(json!({"turn_id":turn,"status":"completed","reason":"done"}));
}

/// Waits for the request of `turn` and answers that it was accepted.
fn accepts(wire: &mut Wire, turn: &str) {
    let request = wire.read("session.suggestion.request");
    assert_eq!(
        request["params"],
        json!({"sessionId":"session","turnId":turn})
    );
    wire.reply(&request, json!({"accepted":true}));
}

/// Waits for the offer's reply and answers it.
fn responds(wire: &mut Wire, turn: &str, action: &str) {
    let request = wire.read("session.suggestion.respond");
    assert_eq!(
        request["params"],
        json!({"sessionId":"session","turnId":turn,"action":action})
    );
    wire.reply(&request, json!({}));
}

#[test]
fn a_completed_turn_asks_for_the_next_message_and_draws_the_offer() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            opened(wire, &[], vec![]);
            completed(wire, "turn-1");
            accepts(wire, "turn-1");
            offer(wire, "turn-1", "run the tests");
            responds(wire, "turn-1", "dismiss");
            wire.hold();
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        if fullscreen {
            sized(&mut command, 100, 32);
        }
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("run the tests");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

#[test]
fn a_daemon_that_does_not_list_the_method_is_never_asked() {
    let home = Home::new();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with_methods(wire, &[], vec![], false);
        completed(wire, "turn-1");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn a_draft_keeps_the_interface_from_asking() {
    let home = Home::new();
    let (arm, ready) = std::sync::mpsc::channel();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        ready.recv_timeout(PATIENCE).unwrap();
        completed(wire, "turn-1");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"a draft");
    terminal.until("a draft");
    arm.send(()).unwrap();
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.raw(b"\x03");
    terminal.until("Press Ctrl+C again to exit");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn a_pending_question_keeps_the_interface_from_asking() {
    let home = Home::new();
    let (arm, ready) = std::sync::mpsc::channel();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main","event":{
                "id":"ask-9","sequence":9,"create_at":1,"type":"step","name":"tool-call",
                "data":{"turn_id":"turn-1","request_id":"request-1","calls":[{
                    "id":"call-1","name":"ask_user","input":{"questions":[{
                        "header":"Database",
                        "question":"Which database should the service use?",
                        "options":[{"label":"PostgreSQL"},{"label":"SQLite"}]}]}}]}}}),
        );
        ready.recv_timeout(PATIENCE).unwrap();
        completed(wire, "turn-1");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        let decline = wire.read("session.question.respond");
        assert_eq!(decline["params"]["declined"], true);
        wire.reply(&decline, json!({}));
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("Which database should the service use?");
    arm.send(()).unwrap();
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.raw(b"\x1b");
    terminal.until("Ask anything...");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn an_active_goal_keeps_the_interface_from_asking() {
    let home = Home::new();
    let (arm, ready) = std::sync::mpsc::channel();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.methods"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let table = wire.read("session.methods");
        wire.reply(&table, json!({"methods":[
            {"name":"session.input.pending","mutating":false},
            {"name":"session.exec.pending","mutating":false},
            {"name":"session.turn.get","mutating":false},
            {"name":"session.goal.get","mutating":false},
            {"name":"session.suggestion.request","mutating":true},
            {"name":"session.suggestion.respond","mutating":true}
        ]}));
        wire.2 = true;
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let (mut startup, mut goal_read) = (0, false);
        while startup < 2 || !goal_read {
            let request = wire.read_any();
            match request["method"].as_str().unwrap() {
                "account.providers" => { wire.reply(&request, json!([])); startup += 1; },
                "session.context.usage" => { startup += 1; wire.reply(&request, json!({"contextWindow":256000.0,"outputTokens":32000.0,"thresholdTokens":200000.0,"estimatedInputTokens":1849.0,"baselineSource":"estimate"})); },
                "session.goal.get" => {
                    wire.reply(&request, json!({"branchId":"main","through":8,"goal":{"objective":"Make the tests pass","status":{"kind":"active"},"evaluator":false}}));
                    goal_read = true;
                }
                method => panic!("unexpected {method}"),
            }
        }
        ready.recv_timeout(PATIENCE).unwrap();
        completed(wire, "turn-1");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("◎ goal active");
    arm.send(()).unwrap();
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn right_takes_the_offer_into_the_draft_without_sending() {
    let home = Home::new();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        accepts(wire, "turn-1");
        offer(wire, "turn-1", "run the tests");
        responds(wire, "turn-1", "accept");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "run the tests");
        wire.reply(&submit, json!({"event":{}}));
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("run the tests");
    terminal.raw(b"\x1b[C");
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\r");
    terminal.raw(b"\x03");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tab_takes_the_offer_in_scrollback() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        accepts(wire, "turn-1");
        offer(wire, "turn-1", "run the tests");
        responds(wire, "turn-1", "accept");
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("run the tests");
    terminal.raw(b"\t");
    terminal.raw(b"\x03");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tab_keeps_the_fullscreen_focus_and_right_still_takes_the_offer() {
    let home = Home::new();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        accepts(wire, "turn-1");
        offer(wire, "turn-1", "run the tests");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        responds(wire, "turn-1", "accept");
        wire.hold();
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("run the tests");
    terminal.raw(b"\t");
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.raw(b"\x1b[C");
    terminal.raw(b"\x03");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn enter_on_an_empty_composer_leaves_the_offer() {
    let home = Home::new();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        accepts(wire, "turn-1");
        offer(wire, "turn-1", "run the tests");
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        responds(wire, "turn-1", "accept");
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("run the tests");
    terminal.raw(b"\r");
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.raw(b"\x1b[C");
    terminal.raw(b"\x03");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn escape_and_an_edit_withdraw_the_offer() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        accepts(wire, "turn-1");
        offer(wire, "turn-1", "run the tests");
        responds(wire, "turn-1", "dismiss");
        offer(wire, "turn-1", "check the logs");
        responds(wire, "turn-1", "dismiss");
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "x");
        wire.reply(&submit, json!({"event":{}}));
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("run the tests");
    terminal.raw(b"\x1b");
    terminal.until("check the logs");
    terminal.write(b"x");
    terminal.write(b"\r");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn a_narrow_terminal_truncates_the_offer() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        accepts(wire, "turn-1");
        offer(wire, "turn-1", "run the tests and commit the change");
        responds(wire, "turn-1", "dismiss");
        wire.hold();
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 30, 24);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("run the tests and co…");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A `throttled` refusal is said once per Session: the second completed turn
/// asks again and its refusal stays quiet.
#[test]
fn the_pause_is_announced_once_per_session() {
    let home = Home::new();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened(wire, &[], vec![]);
        completed(wire, "turn-1");
        let request = wire.read("session.suggestion.request");
        wire.reply(&request, json!({"accepted":false,"reason":"throttled"}));
        completed(wire, "turn-2");
        let request = wire.read("session.suggestion.request");
        wire.reply(&request, json!({"accepted":false,"reason":"throttled"}));
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("Suggestions are paused in this session");
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert_eq!(
        terminal
            .transcript
            .matches("Suggestions are paused in this session")
            .count(),
        1
    );
}

/// A daemon that does not list the suggestion methods opens `/settings`
/// with no read of the source and no error notice, and the panel has no
/// Prompt suggestion switch.
#[test]
fn settings_shows_no_suggestion_row_without_the_methods() {
    let home = Home::new();
    let (arm, ready) = std::sync::mpsc::channel();
    let (settled, settle) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        opened_with_methods(wire, &["settings.read", "settings.update"], vec![], false);
        ready.recv_timeout(PATIENCE).unwrap();
        // The settings panel reads the prompt texts too; this test is about
        // the absent suggestion source.
        let read = wire.read("settings.read");
        assert_eq!(read["params"]["source"], "prompts");
        wire.reply(&read, json!({"revision":"r1","document":{}}));
        wire.quiet(Duration::from_millis(300));
        settled.send(()).unwrap();
        wire.hold();
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 100, 40);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/settings\r");
    arm.send(()).unwrap();
    settle.recv_timeout(PATIENCE).unwrap();
    terminal.until("Startup UI form");
    terminal.raw(b"\x1b");
    terminal.until("Ask anything...");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    assert!(
        !terminal.transcript.contains("Prompt suggestion"),
        "{}",
        terminal.transcript
    );
}

#[test]
fn settings_toggles_the_switch_through_the_daemon() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        opened(wire, &["settings.read", "settings.update"], vec![]);
        let read = wire.read("settings.read");
        assert_eq!(
            read["params"],
            json!({"source":"prompt-suggestion","scope":"user"})
        );
        wire.reply(
            &read,
            json!({"revision":"r1","document":{"enabled":false,"maxChars":80}}),
        );
        // `/settings` also reads the prompt texts when it opens.
        let prompts = wire.read("settings.read");
        assert_eq!(prompts["params"]["source"], "prompts");
        wire.reply(&prompts, json!({"revision":"r2","document":{}}));
        let update = wire.read("settings.update");
        assert_eq!(
            update["params"],
            json!({"source":"prompt-suggestion","scope":"user","expectedRevision":"r1",
                "document":{"enabled":true,"maxChars":80}})
        );
        wire.reply(
            &update,
            json!({"revision":"r2","document":{"enabled":true,"maxChars":80}}),
        );
        wire.hold();
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 100, 40);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");
    terminal.write(b"/settings\r");
    // The row shows the value the daemon reported; pressing before it is
    // applied makes the client read the source again.
    terminal.until_row("Prompt suggestion", "off");
    for _ in 0..9 {
        terminal.raw(b"\x1b[B");
    }
    terminal.raw(b"\r");
    terminal.raw(b"\x1b");
    terminal.until("Ask anything...");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}
