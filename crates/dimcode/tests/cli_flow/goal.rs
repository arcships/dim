//! `/goal`: the goal of the shown line, folded from the branch's events and
//! written through the methods Goal registers (cli-commands.md).
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const USAGE: &str =
    "Usage: /goal [OBJECTIVE | set OBJECTIVE | edit OBJECTIVE | pause | resume | clear]";
const NOT_AVAILABLE: &str = "Goal is not available on this daemon.";

/// The Session's method table, as `session.methods` lists it.
fn methods(goal: bool) -> Value {
    let mut methods = vec![json!({"name":"session.approval.respond","mutating":true})];
    if goal {
        for name in ["set", "edit", "pause", "resume", "clear"] {
            methods.push(json!({"name":format!("session.goal.{name}"),"mutating":true}));
        }
    }
    json!({"methods": methods})
}

fn objective(branch: &str, op: &str, text: &str) -> Value {
    json!({"type":"raw","key":"goal:objective","text":format!(
        "<dim-goal kind=\"goal\" op=\"{op}\" branch=\"{branch}\">\nObjective:\n{text}\n\nGoal text.\n</dim-goal>"
    )})
}

fn status(text: &str) -> Value {
    json!({"type":"raw","key":"goal:status","text":text})
}

fn active(branch: &str) -> Value {
    status(&format!(
        "<dim-goal kind=\"status\" status=\"active\" branch=\"{branch}\">\nThe goal for branch {branch} is active.\n</dim-goal>"
    ))
}

/// A change input, as Goal writes it.
fn change(sequence: u64, blocks: Vec<Value>) -> Value {
    json!({"id":format!("goal-{sequence}"),"sequence":sequence,"create_at":1,
        "type":"input","name":"message","data":{"blocks":blocks}})
}

fn set(sequence: u64, text: &str) -> Value {
    change(
        sequence,
        vec![objective("main", "set", text), active("main")],
    )
}

fn edit(sequence: u64, text: &str) -> Value {
    change(sequence, vec![objective("main", "edit", text)])
}

fn paused(sequence: u64) -> Value {
    change(
        sequence,
        vec![status(
            "<dim-goal kind=\"goal\" op=\"pause\" reason=\"user\" branch=\"main\">\nThe user set the goal status for branch main to paused.\n</dim-goal>",
        )],
    )
}

fn resumed(sequence: u64) -> Value {
    change(
        sequence,
        vec![status(
            "<dim-goal kind=\"goal\" op=\"resume\" branch=\"main\">\nThe user set the goal status for branch main to active.\n</dim-goal>",
        )],
    )
}

fn cleared(sequence: u64) -> Value {
    change(
        sequence,
        vec![
            json!({"type":"raw","key":"goal:objective","retire":true,"text":
                "<dim-goal kind=\"goal\" op=\"clear\" branch=\"main\">\nThe previous goal was cleared by the user.\n</dim-goal>"}),
            json!({"type":"raw","key":"goal:status","retire":true,"text":
                "<dim-goal kind=\"goal\" status=\"cleared\" branch=\"main\">\nThe goal for branch main has no status.\n</dim-goal>"}),
        ],
    )
}

fn completed(sequence: u64) -> Value {
    change(
        sequence,
        vec![status(
            "<dim-goal kind=\"goal\" op=\"completed\" branch=\"main\">\nResult:\nDone\n\nFinished.\n</dim-goal>",
        )],
    )
}

/// The fork's own opening: the facts before it are copied from the parent.
fn forked(sequence: u64) -> Value {
    json!({"id":format!("forked-{sequence}"),"sequence":sequence,"create_at":1,
        "type":"session","name":"forked","data":{}})
}

/// A history page that fixes the head at `through` and holds `events`.
fn page(through: u64, events: Vec<Value>) -> Value {
    json!({"events":events,"through":through,"nextAfter":through,"done":true})
}

fn usage() -> Value {
    json!({"contextWindow":256000.0,"outputTokens":32000.0,"thresholdTokens":200000.0,
        "estimatedInputTokens":1849.0,"baselineSource":"estimate"})
}

/// Fails `request` as a store that cannot be read does.
fn failed(wire: &mut Wire, request: &Value) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
        "code":-32000,"message":"the store cannot be read",
        "data":{"appError":"internal"}}}));
}

/// A public event of `branch`, live.
fn emit(wire: &mut Wire, branch: &str, sequence: u64, kind: &str, name: &str, data: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":
            {"id":format!("{kind}-{sequence}"),"sequence":sequence,"create_at":1,
             "type":kind,"name":name,"data":data}}),
    );
}

/// A change input Goal wrote, live: a line whose history is done folds it
/// through the live stream, not a history page.
fn change_notice(wire: &mut Wire, branch: &str, change: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":change}),
    );
}

/// Answers what the terminal UI reads while it starts and returns the
/// request for `method`.
fn until_request(wire: &mut Wire, method: &str) -> Value {
    loop {
        let request = wire.read_any();
        match request["method"].as_str() {
            Some("session.context.usage") => wire.reply(&request, usage()),
            Some("account.providers") => wire.reply(&request, json!([])),
            Some(found) if found == method => return request,
            other => panic!("unexpected request {other:?}"),
        }
    }
}

/// Answers what the terminal UI reads while it starts, until the first
/// `/goal` asks the Session's method table.
fn started(wire: &mut Wire) {
    let request = until_request(wire, "session.methods");
    wire.reply(&request, methods(true));
}

fn drained(wire: &mut Wire) {
    let mut line = String::new();
    while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
        line.clear();
    }
}

#[test]
fn line_goal_forms_write_the_machine_they_name_and_show_the_events() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        wire.created();
        wire.attached();

        // The read form makes no call; it reads the line to its head.
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["after"], 8);
        wire.reply(&history, page(8, vec![]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Make the tests pass"})
        );
        wire.reply(&write, json!({"sequence":9}));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["after"], 8);
        wire.reply(&history, page(9, vec![set(9, "Make the tests pass")]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(9, vec![]));

        // A goal is in force: the same form changes only its objective.
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(9, vec![]));
        let write = wire.read("session.goal.edit");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Make the tests pass quickly"})
        );
        wire.reply(&write, json!({"sequence":12}));
        let history = wire.read("session.history");
        wire.reply(&history, page(12, vec![edit(12, "Make the tests pass quickly")]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(12, vec![]));
        let write = wire.read("session.goal.pause");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main"})
        );
        wire.reply(&write, json!({"sequence":15}));
        let history = wire.read("session.history");
        wire.reply(&history, page(15, vec![paused(15)]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(15, vec![]));
        let write = wire.read("session.goal.resume");
        wire.reply(&write, json!({"sequence":18}));
        let history = wire.read("session.history");
        wire.reply(&history, page(18, vec![resumed(18)]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(18, vec![]));
        let write = wire.read("session.goal.clear");
        wire.reply(&write, json!({"sequence":21}));
        let history = wire.read("session.history");
        wire.reply(&history, page(21, vec![cleared(21)]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["after"], 21);
        wire.reply(&history, page(21, vec![]));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");

    terminal.write(b"/goal\n");
    terminal.until(USAGE);
    terminal.until("No goal on main.");
    terminal.until("\n> ");

    terminal.write(b"/goal Make the tests pass\n");
    terminal.until("Goal set on main · active");
    terminal.until("  Make the tests pass");
    terminal.until("\n> ");

    terminal.write(b"/goal\n");
    terminal.until(USAGE);
    terminal.until("Goal on main · active");
    terminal.until("  Make the tests pass");
    terminal.until("\n> ");

    terminal.write(b"/goal Make the tests pass quickly\n");
    terminal.until("Goal edited on main · active");
    terminal.until("  Make the tests pass quickly");
    terminal.until("\n> ");

    terminal.write(b"/goal pause\n");
    terminal.until("Goal paused on main · paused (user)");
    terminal.until("\n> ");

    terminal.write(b"/goal resume\n");
    terminal.until("Goal resumed on main · active");
    terminal.until("\n> ");

    terminal.write(b"/goal clear\n");
    terminal.until("Goal cleared on main.");
    terminal.until("\n> ");

    terminal.write(b"/goal\n");
    terminal.until("No goal on main.");
    terminal.until("\n> ");

    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_goal_takes_an_objective_spelled_like_a_form_after_set() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        wire.created();
        wire.attached();
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(write["params"]["objective"], "pause");
        wire.reply(&write, json!({"sequence":9}));
        let history = wire.read("session.history");
        wire.reply(&history, page(9, vec![set(9, "pause")]));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/goal set pause\n");
    terminal.until("Goal set on main · active");
    terminal.until("  pause");
    terminal.until("\n> ");
    terminal.write(b"/goal edit\n");
    terminal.until(USAGE);
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_goal_needs_a_daemon_that_installs_goal() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        wire.created();
        wire.attached();
        let read = wire.read("session.methods");
        wire.reply(&read, methods(false));
        // Neither form calls a goal method nor submits anything.
        let read = wire.read("session.methods");
        wire.reply(&read, methods(false));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"/goal Ship it\n");
    terminal.until(NOT_AVAILABLE);
    terminal.until("\n> ");
    terminal.write(b"/goal\n");
    terminal.until(NOT_AVAILABLE);
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_goal_sets_a_new_goal_after_a_completed_one() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        wire.created();
        wire.attached();
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"First goal"})
        );
        wire.reply(&write, json!({"sequence":9}));
        // The worker reported the goal completed before the next command.
        let history = wire.read("session.history");
        wire.reply(
            &history,
            page(10, vec![set(9, "First goal"), completed(10)]),
        );

        // A typed objective after a finished goal starts a new one.
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(10, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Next goal"})
        );
        wire.reply(&write, json!({"sequence":11}));
        let history = wire.read("session.history");
        wire.reply(&history, page(11, vec![set(11, "Next goal")]));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");

    terminal.write(b"/goal First goal\n");
    terminal.until("Goal set on main · completed");
    terminal.until("\n> ");

    terminal.write(b"/goal Next goal\n");
    terminal.until("Goal set on main · active");
    terminal.until("  Next goal");
    terminal.until("\n> ");

    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_goal_shows_no_copied_goal_after_a_fork() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        // The copied facts carry a live goal; the fork's own opening follows
        // and no clear does.
        let history = wire.read("session.history");
        wire.reply(&history, page(10, vec![set(9, "Parent goal"), forked(10)]));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":10}));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["after"], 10);
        wire.reply(&history, page(10, vec![]));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");

    terminal.write(b"/goal\n");
    terminal.until("No goal on main.");
    terminal.until("\n> ");

    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_goal_shows_the_state_and_marks_the_status_area() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let table = wire.read("session.methods");
        wire.reply(&table, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        started(wire);

        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Ship it"})
        );
        wire.reply(&write, json!({"sequence":9}));
        change_notice(wire, "main", set(9, "Ship it"));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let write = wire.read("session.goal.edit");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Ship it fully"})
        );
        wire.reply(&write, json!({"sequence":12}));
        change_notice(wire, "main", edit(12, "Ship it fully"));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let write = wire.read("session.goal.pause");
        wire.reply(&write, json!({"sequence":15}));
        change_notice(wire, "main", paused(15));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let write = wire.read("session.goal.resume");
        wire.reply(&write, json!({"sequence":18}));
        change_notice(wire, "main", resumed(18));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let write = wire.read("session.goal.clear");
        wire.reply(&write, json!({"sequence":21}));
        change_notice(wire, "main", cleared(21));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        drained(wire);
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("⏵⏵ Workspace write");
    terminal.until("test / model");

    terminal.write(b"/goal Ship it\r");
    terminal.until("Goal set on main · active");
    terminal.until("  Ship it");
    terminal.until("◎ goal active");

    terminal.write(b"/goal Ship it fully\r");
    terminal.until("Goal edited on main · active");
    terminal.until("  Ship it fully");

    terminal.write(b"/goal pause\r");
    terminal.until("Goal paused on main · paused (user)");
    terminal.until("◎ goal paused");

    terminal.write(b"/goal resume\r");
    terminal.until("Goal resumed on main · active");
    terminal.until("◎ goal active");

    terminal.write(b"/goal clear\r");
    terminal.until("Goal cleared on main.");

    terminal.write(b"/goal\r\r");
    terminal.until(USAGE);
    terminal.until("No goal on main.");

    terminal.write(b"/goal edit\r");
    terminal.until(USAGE);

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn tui_goal_acts_on_the_shown_line_and_a_new_line_has_none() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let table = wire.read("session.methods");
        wire.reply(&table, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        started(wire);

        let write = wire.read("session.goal.set");
        assert_eq!(write["params"]["branchId"], "main");
        wire.reply(&write, json!({"sequence":9}));
        change_notice(wire, "main", set(9, "Ship it"));

        // A line opens from main; its own history starts at its branch/start.
        emit(
            wire,
            "main",
            10,
            "branch",
            "create",
            json!({"branch":"line-1","at":{"kind":"create"},
                "source":{"kind":"host","operation_id":"op-1"},"prompt":"child work"}),
        );
        let history = wire.read("session.history");
        assert_eq!(history["params"]["branchId"], "line-1");
        wire.reply(
            &history,
            page(
                11,
                vec![json!({"id":"start-line-1","sequence":11,"create_at":1,
                    "type":"branch","name":"start","data":{"parent":"main"}})],
            ),
        );

        let transcript = wire.read("session.history");
        assert_eq!(transcript["params"]["after"], 0);
        wire.reply(
            &transcript,
            page(
                11,
                vec![json!({"id":"start-line-1","sequence":11,"create_at":1,
            "type":"branch","name":"start","data":{"parent":"main"}})],
            ),
        );
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"line-1","objective":"Child goal"})
        );
        wire.reply(&write, json!({"sequence":12}));
        change_notice(
            wire,
            "line-1",
            change(
                12,
                vec![objective("line-1", "set", "Child goal"), active("line-1")],
            ),
        );
        drained(wire);
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/goal Ship it\r");
    terminal.until("Goal set on main · active");

    terminal.until_anywhere("line-1");
    terminal.write(b"/lines\r");
    terminal.until("the session itself");
    terminal.write(b"\x1b[B");
    terminal.write(b"\r");
    terminal.until("⑂ main ▸ child work");

    terminal.write(b"/goal\r\r");
    terminal.until(USAGE);
    terminal.until("No goal on line-1.");

    terminal.write(b"/goal Child goal\r");
    terminal.until("Goal set on line-1 · active");
    terminal.until("  Child goal");

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A line whose discovery page has not landed yet. `/goal OBJECTIVE` waits
/// for the reader to finish the line before it picks `set` or `edit`, so an
/// active goal the interface has not folded yet still gets `edit`.
#[test]
fn tui_goal_reads_the_line_before_it_picks_set_or_edit() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let table = wire.read("session.methods");
        wire.reply(&table, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));

        // line-1 carries an active goal; its discovery page is delayed.
        emit(
            wire,
            "main",
            9,
            "branch",
            "create",
            json!({"branch":"line-1","at":{"kind":"create"},
                "source":{"kind":"host","operation_id":"op-1"},"prompt":"child work"}),
        );
        let delayed = until_request(wire, "session.history");
        assert_eq!(delayed["params"]["branchId"], "line-1");

        let read = until_request(wire, "session.methods");
        wire.reply(&read, methods(true));
        wire.quiet(Duration::from_millis(150));

        // The discovery page lands with the active goal the interface had not
        // folded: only now may the command choose `edit`.
        let events = vec![
            json!({"id":"start-line-1","sequence":11,"create_at":1,
                "type":"branch","name":"start","data":{"parent":"main"}}),
            change(
                12,
                vec![objective("line-1", "set", "Old goal"), active("line-1")],
            ),
        ];
        wire.reply(&delayed, page(12, events));
        let write = wire.read("session.goal.edit");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"line-1","objective":"New objective"})
        );
        wire.reply(&write, json!({"sequence":13}));
        change_notice(
            wire,
            "line-1",
            change(13, vec![objective("line-1", "edit", "New objective")]),
        );
        drained(wire);
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/lines\r");
    terminal.until("line-1");
    terminal.write(b"\x1b[B");
    terminal.write(b"\r");
    terminal.until("⑂ main ▸ child work");

    terminal.write(b"/goal New objective\r");
    terminal.until("Goal edited on line-1 · active");
    terminal.until("  New objective");
    assert!(
        !terminal.transcript.contains("Goal set on line-1"),
        "the command never decides on a fold it has not read: {}",
        terminal.transcript
    );

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A read the command needs fails. `/goal OBJECTIVE` reports it and sends no
/// goal method, instead of deciding from the fold it already has.
#[test]
fn tui_goal_reports_a_failed_read_and_sends_nothing() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let table = wire.read("session.methods");
        wire.reply(&table, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));

        emit(
            wire,
            "main",
            9,
            "branch",
            "create",
            json!({"branch":"line-1","at":{"kind":"create"},
                "source":{"kind":"host","operation_id":"op-1"},"prompt":"child work"}),
        );
        let discovery = until_request(wire, "session.history");
        failed(wire, &discovery);

        let transcript = until_request(wire, "session.history");
        failed(wire, &transcript);
        let read = until_request(wire, "session.methods");
        wire.reply(&read, methods(true));

        // No goal method is sent before the client leaves.
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_ne!(request["method"], json!("session.goal.set"));
            assert_ne!(request["method"], json!("session.goal.edit"));
            line.clear();
        }
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/lines\r");
    terminal.until("line-1");
    terminal.write(b"\x1b[B");
    terminal.write(b"\r");
    terminal.until("⑂ main ▸ child work");

    terminal.write(b"/goal New objective\r");
    terminal.until("Goal: the store cannot be read (RPC -32000, internal)");
    assert!(
        !terminal.transcript.contains("Goal set on line-1")
            && !terminal.transcript.contains("Goal edited on line-1"),
        "no goal method ran: {}",
        terminal.transcript
    );

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// `/goal` while the shown line's initial history is still loading: the
/// command waits for the task-history reader, so exactly one
/// `session.history` page is outstanding for the line, the fold it decides on
/// is the reader's whole fixed window, and it repeats no page from zero. A
/// second `/goal` while the first waits is reported and changes nothing.
#[test]
fn tui_goal_during_the_initial_history_read_opens_no_second_pager() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let table = wire.read("session.methods");
        wire.reply(&table, methods(true));

        // main's first history page is outstanding when `/goal` arrives.
        let held = wire.read("session.history");
        assert_eq!(held["params"]["after"], 0);

        let read = until_request(wire, "session.methods");
        wire.reply(&read, methods(true));
        // Only one reader: while that page is outstanding, no second history
        // request for the line and no goal method travels.
        wire.quiet(Duration::from_millis(500));

        // The first page fixes the window, and the active goal is on the
        // second: only a command that waits for the reader's `done` folds it,
        // so `edit` proves the wait.
        wire.reply(
            &held,
            json!({"events":[],"through":9,"nextAfter":4,"done":false}),
        );
        let second = wire.read("session.history");
        assert_eq!(second["params"]["after"], 4);
        assert_eq!(second["params"]["through"], 9);
        wire.reply(&second, page(9, vec![set(9, "Old goal")]));

        let write = wire.read("session.goal.edit");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"New objective"})
        );
        wire.reply(&write, json!({"sequence":10}));
        change_notice(wire, "main", edit(10, "New objective"));

        // The queue sends no page after the one that finished the line: the
        // only frames left are what the interface reads while it exits.
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_ne!(request["method"], json!("session.history"), "{request}");
            line.clear();
        }
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/goal New objective\r");
    terminal.write(b"/goal Another objective\r");
    terminal.until("Goal: a /goal is still in progress on this line");
    terminal.until("Goal edited on main · active");
    terminal.until("  New objective");

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A JSON-RPC call over the real daemon's socket, skipping notices.
fn rpc(wire: &mut BufReader<UnixStream>, id: &mut u64, method: &str, params: Value) -> Value {
    *id += 1;
    writeln!(
        wire.get_mut(),
        "{}",
        json!({"jsonrpc":"2.0","id":*id,"method":method,"params":params})
    )
    .unwrap();
    wire.get_mut().flush().unwrap();
    loop {
        let mut line = String::new();
        assert_ne!(
            wire.read_line(&mut line).unwrap(),
            0,
            "the daemon answered {method}"
        );
        let reply: Value = serde_json::from_str(&line).unwrap();
        if reply["id"] == *id {
            return reply;
        }
    }
}

/// Every message a chat request carries: its role and its content as sent.
fn messages(request: &Value) -> Vec<(String, String)> {
    request["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    (
                        message["role"].as_str().unwrap_or_default().to_owned(),
                        match &message["content"] {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a message of `role` carries `needle`. The System prompt holds
/// Goal's guide entry, which names every marker, so a marker the model is
/// asked to act on is always a user or tool message.
fn said(request: &Value, role: &str, needle: &str) -> bool {
    messages(request)
        .iter()
        .any(|(message_role, text)| message_role == role && text.contains(needle))
}

/// One SSE frame of the fixture model.
fn frame(delta: Value, finish: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id":"goal-fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}

/// The fixture model's reply: a plain turn, then the `goal_update` call the
/// continuation asks for, then the closing reply to that call's result. A
/// later request is answered with plain text, so a stray continuation cannot
/// keep the conversation going.
fn model_reply(request: &Value, index: usize) -> String {
    let data = if index >= 3 {
        frame(json!({"content":"No further work."}), json!("stop"))
    } else if said(request, "tool", "goal_update recorded") {
        frame(json!({"content":"Goal completed."}), json!("stop"))
    } else if said(request, "user", r#"kind="continuation""#) {
        let arguments = json!({"status":"completed","summary":"all done"}).to_string();
        frame(
            json!({"tool_calls":[{"index":0,"id":"goal-update","type":"function",
                "function":{"name":"goal_update","arguments":arguments}}]}),
            Value::Null,
        ) + &frame(json!({}), json!("tool_calls"))
    } else {
        frame(json!({"content":"Continuing."}), json!("stop"))
    };
    data + "data: [DONE]\n\n"
}

/// A scripted OpenAI-compatible endpoint on loopback: it records every chat
/// request and answers each with [`model_reply`].
fn model_endpoint() -> (String, Arc<Mutex<Vec<Value>>>, Arc<AtomicBool>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (seen, end) = (requests.clone(), stop.clone());
    thread::spawn(move || {
        while !end.load(Ordering::Acquire) {
            let (stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut stream = BufReader::new(stream);
            let mut request_line = String::new();
            assert!(stream.read_line(&mut request_line).unwrap() > 0);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            if !request_line.starts_with("POST /v1/chat/completions") {
                let head =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                stream.get_mut().write_all(head.as_bytes()).unwrap();
                stream.get_mut().flush().unwrap();
                continue;
            }
            let request: Value = serde_json::from_slice(&body).unwrap();
            let index = seen.lock().unwrap().len();
            let reply = model_reply(&request, index);
            seen.lock().unwrap().push(request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.len()
            );
            stream.get_mut().write_all(head.as_bytes()).unwrap();
            stream.get_mut().write_all(reply.as_bytes()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    (format!("http://{address}/v1"), requests, stop)
}

/// The base CLI's default profile serves Goal natively: with no Node or
/// Python on `PATH`, no staged package and no runtime acquisition, the
/// daemon lists `session.goal.*`, reads the `goal` Settings source, and a
/// completed turn drives a continuation that reaches the model. The model
/// then reports completion, and no further continuation follows.
#[test]
fn the_product_profile_installs_native_goal_without_a_foreign_runtime() {
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let path = home.0.join("path");
    fs::create_dir_all(&path).unwrap();
    assert!(!path.join("node").exists() && !path.join("python").exists());
    let (base_url, requests, stop) = model_endpoint();
    let output = home
        .command()
        .env("PATH", &path)
        .args(["mcp", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = BufReader::new(stream);
    let mut id = 0;
    let hello = rpc(
        &mut wire,
        &mut id,
        "hello",
        json!({"protocolVersion":3,"clientKind":"cli"}),
    );
    assert_eq!(
        hello["result"]["daemonProfile"]["kind"], "product",
        "{hello}"
    );

    let deadline = Instant::now() + PATIENCE;
    let settings = loop {
        let reply = rpc(
            &mut wire,
            &mut id,
            "settings.read",
            json!({"source":"goal","scope":"user"}),
        );
        if reply.get("result").is_some() {
            break reply;
        }
        assert!(
            ["service_reloading", "unknown_source"]
                .iter()
                .any(|code| reply["error"]["data"]["appError"] == *code),
            "{reply}"
        );
        assert!(
            Instant::now() < deadline,
            "the goal Settings source registers"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(settings["result"]["document"], json!({}), "{settings}");
    let revision = settings["result"]["revision"]
        .as_str()
        .expect("the settings revision")
        .to_owned();
    let updated = rpc(
        &mut wire,
        &mut id,
        "settings.update",
        json!({"source":"goal","scope":"user","expectedRevision":revision,
            "document":{"maxContinuations":5}}),
    );
    assert_eq!(
        updated["result"]["document"]["maxContinuations"], 5,
        "{updated}"
    );
    let cleared = rpc(
        &mut wire,
        &mut id,
        "settings.update",
        json!({"source":"goal","scope":"user",
            "expectedRevision":updated["result"]["revision"],"document":{}}),
    );
    assert_eq!(cleared["result"]["document"], json!({}), "{cleared}");

    let provider = "018ec73f-0000-7000-8000-000000000097";
    let configured = rpc(
        &mut wire,
        &mut id,
        "provider.create",
        json!({"providerId":provider,"displayName":"Goal profile fixture",
        "connection":{"driverKind":"openai-compatible","baseUrl":base_url,"auth":{"type":"anonymous"}},
        "models":[{"modelId":"goal","displayName":"Goal","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(32_000,1024)}],"enabled":true}),
    );
    assert!(configured.get("result").is_some(), "{configured}");
    let selected = rpc(
        &mut wire,
        &mut id,
        "provider.select",
        json!({"providerId":provider,"modelId":"goal"}),
    );
    assert!(selected.get("result").is_some(), "{selected}");
    let created = rpc(
        &mut wire,
        &mut id,
        "session.create",
        json!({"cwd":home.0,"interactive":true,"tools":["goal_update"],
        "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
    );
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("the product creates a Session")
        .to_owned();
    assert_eq!(
        created["result"]["model"]["modelId"], "goal",
        "the selected provider serves the Session: {created}"
    );
    let deadline = Instant::now() + PATIENCE;
    loop {
        let set = rpc(
            &mut wire,
            &mut id,
            "session.goal.set",
            json!({"sessionId":session,"objective":"ship the native goal"}),
        );
        if let Some(result) = set.get("result") {
            assert!(result["sequence"].is_u64(), "{set}");
            break;
        }
        assert!(
            ["service_reloading", "unknown_method"]
                .iter()
                .any(|code| set["error"]["data"]["appError"] == *code),
            "{set}"
        );
        assert!(
            Instant::now() < deadline,
            "the Goal part activates in the product profile"
        );
        thread::sleep(Duration::from_millis(5));
    }

    let listed = rpc(
        &mut wire,
        &mut id,
        "session.methods",
        json!({"sessionId":session}),
    );
    let names: Vec<&str> = listed["result"]["methods"]
        .as_array()
        .expect("the method table")
        .iter()
        .filter_map(|method| method["name"].as_str())
        .collect();
    for name in [
        "session.goal.set",
        "session.goal.edit",
        "session.goal.pause",
        "session.goal.resume",
        "session.goal.clear",
    ] {
        assert!(names.contains(&name), "{name} is listed: {listed}");
    }

    // The set's turn, the continuation's turn and that turn's tool result.
    let deadline = Instant::now() + PATIENCE;
    while requests.lock().unwrap().len() < 3 {
        if Instant::now() >= deadline {
            let seen = requests.lock().unwrap().clone();
            let dump: Vec<String> = seen
                .iter()
                .map(|request| {
                    messages(request)
                        .iter()
                        .map(|(role, text)| {
                            format!("{role}: {}", text.chars().take(300).collect::<String>())
                        })
                        .collect::<Vec<_>>()
                        .join("\n--\n")
                })
                .collect();
            let history = rpc(
                &mut wire,
                &mut id,
                "session.history",
                json!({"sessionId":session,"branchId":"main","after":0,"limit":256}),
            );
            panic!(
                "the goal's turns reach the model ({} requests)\nrequests: {dump:#?}\nhistory: {history}",
                seen.len()
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    let idle = rpc(
        &mut wire,
        &mut id,
        "session.idle",
        json!({"sessionId":session,"branchId":"main"}),
    );
    assert!(idle.get("result").is_some(), "{idle}");
    thread::sleep(Duration::from_millis(500));
    let seen = requests.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        3,
        "a completed goal asks for no further turn: {seen:#?}"
    );
    assert!(
        said(&seen[1], "user", r#"kind="continuation" count="1""#),
        "the continuation reaches the model: {:#?}",
        seen[1]
    );
    assert!(
        said(&seen[2], "tool", "goal_update recorded: completed."),
        "the tool result reaches the model: {:#?}",
        seen[2]
    );

    let history = rpc(
        &mut wire,
        &mut id,
        "session.history",
        json!({"sessionId":session,"branchId":"main","after":0,"limit":256}),
    );
    assert_eq!(history["result"]["done"], true, "{history}");
    let blocks: Vec<String> = history["result"]["events"]
        .as_array()
        .expect("the branch's events")
        .iter()
        .flat_map(|event| {
            event["data"]["blocks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|block| block["text"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect();
    let continuation = blocks
        .iter()
        .find(|text| text.contains(r#"kind="continuation""#))
        .expect("the completed turn writes a continuation");
    assert!(
        continuation.starts_with(r#"<dim-goal kind="continuation" count="1" after=""#)
            && continuation.contains(r#"branch="main">"#),
        "{continuation}"
    );
    let report = blocks
        .iter()
        .find(|text| text.contains(r#"op="completed""#))
        .expect("goal_update writes its report");
    assert!(
        report.contains("Result:\nall done") && report.contains(r#"branch="main""#),
        "{report}"
    );
    assert!(
        !home.0.join("plugins").exists(),
        "no staged package directory exists"
    );
    assert!(
        !home.0.join("runtimes").exists() && !home.0.join("rust/runtimes").exists(),
        "no runtime was acquired"
    );
    stop.store(true, Ordering::Release);
    assert!(
        home.command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
