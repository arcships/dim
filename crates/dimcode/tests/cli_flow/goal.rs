//! `/goal`: the goal of the shown line, folded from the branch's events and
//! written through the methods Goal registers (cli-commands.md).
use super::*;

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
    terminal.until("No goal on main.");
    terminal.until("\n> ");

    terminal.write(b"/goal Make the tests pass\n");
    terminal.until("Goal set on main · active");
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
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        started(wire);

        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Ship it"})
        );
        wire.reply(&write, json!({"sequence":9}));
        let history = wire.read("session.history");
        wire.reply(&history, page(9, vec![set(9, "Ship it")]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(9, vec![]));
        let write = wire.read("session.goal.edit");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"main","objective":"Ship it fully"})
        );
        wire.reply(&write, json!({"sequence":12}));
        let history = wire.read("session.history");
        wire.reply(&history, page(12, vec![edit(12, "Ship it fully")]));

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        wire.reply(&history, page(12, vec![]));
        let write = wire.read("session.goal.pause");
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
        wire.reply(&history, page(21, vec![]));
        drained(wire);
    });
    let mut command = home.command();
    command.arg("--scrollback");
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::launch_tui(command);
    // The scrollback status line names the approval mode in force.
    terminal.until("⏵⏵ Ask (legacy)");
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

    terminal.write(b"/goal\r");
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
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        started(wire);

        let history = wire.read("session.history");
        wire.reply(&history, page(8, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(write["params"]["branchId"], "main");
        wire.reply(&write, json!({"sequence":9}));
        let history = wire.read("session.history");
        wire.reply(&history, page(9, vec![set(9, "Ship it")]));

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

        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["branchId"], "line-1");
        wire.reply(&history, page(11, vec![]));
        let read = wire.read("session.methods");
        wire.reply(&read, methods(true));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["branchId"], "line-1");
        wire.reply(&history, page(11, vec![]));
        let write = wire.read("session.goal.set");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"line-1","objective":"Child goal"})
        );
        wire.reply(&write, json!({"sequence":12}));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["branchId"], "line-1");
        wire.reply(
            &history,
            page(
                12,
                vec![change(
                    12,
                    vec![objective("line-1", "set", "Child goal"), active("line-1")],
                )],
            ),
        );
        drained(wire);
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("test / model");

    terminal.write(b"/goal Ship it\r");
    terminal.until("Goal set on main · active");

    terminal.until("line-1");
    terminal.write(b"/lines\r");
    terminal.until("the session itself");
    terminal.write(b"\x1b[B");
    terminal.write(b"\r");
    terminal.until("⑂ main ▸ child work");

    terminal.write(b"/goal\r");
    terminal.until("No goal on line-1.");

    terminal.write(b"/goal Child goal\r");
    terminal.until("Goal set on line-1 · active");
    terminal.until("  Child goal");

    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A line whose discovery page has not landed yet. `/goal OBJECTIVE` reads
/// the line to its head before it picks `set` or `edit`, so an active goal
/// the interface has not folded yet still gets `edit`.
#[test]
fn tui_goal_reads_the_line_before_it_picks_set_or_edit() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.methods"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
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
        let read = until_request(wire, "session.history");
        assert_eq!(read["params"]["branchId"], "line-1");
        assert_eq!(read["params"]["after"], 0);
        let events = vec![
            json!({"id":"start-line-1","sequence":11,"create_at":1,
                "type":"branch","name":"start","data":{"parent":"main"}}),
            change(
                12,
                vec![objective("line-1", "set", "Old goal"), active("line-1")],
            ),
        ];
        wire.reply(&read, page(12, events.clone()));
        let write = wire.read("session.goal.edit");
        assert_eq!(
            write["params"],
            json!({"sessionId":"session","branchId":"line-1","objective":"New objective"})
        );
        wire.reply(&write, json!({"sequence":13}));
        let read = wire.read("session.history");
        wire.reply(
            &read,
            page(
                13,
                vec![change(
                    13,
                    vec![objective("line-1", "edit", "New objective")],
                )],
            ),
        );

        // The discovery page lands late; its events predate the edit.
        wire.reply(&delayed, page(12, events));
        drained(wire);
    });
    let mut command = home.command();
    sized(&mut command, 100, 32);
    let mut terminal = Terminal::launch_tui(command);
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
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
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

        let read = until_request(wire, "session.methods");
        wire.reply(&read, methods(true));
        let read = until_request(wire, "session.history");
        assert_eq!(read["params"]["branchId"], "line-1");
        failed(wire, &read);

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
    let mut terminal = Terminal::launch_tui(command);
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
