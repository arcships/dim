//! Line-mode answers to `ask_user` (Questions design A.5) against a scripted
//! daemon: choices, custom text, a decline, questions of another branch read
//! from its history, every `session.question.respond` reply, closing on
//! another client's answer or a cancel, Ctrl+C and EOF, also with an answer
//! in flight, and `exec` without a terminal.
use super::*;
use std::sync::mpsc;

fn step(wire: &mut Wire, branch: &str, sequence: u64, name: &str, data: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":event(branch, sequence, name, data)}),
    );
}
fn event(branch: &str, sequence: u64, name: &str, data: Value) -> Value {
    json!({"id":format!("{branch}-{sequence}"),"sequence":sequence,"create_at":1,"type":"step","name":name,"data":data})
}
/// `step/tool-call` data of model request `request` asking `call`.
fn asked(request: &str, call: &str, questions: Value) -> Value {
    json!({"turn_id":"turn","request_id":request,"calls":[{"id":call,"name":"ask_user","input":{"questions":questions}}]})
}
fn answered(call_event: &str, request: &str, call: &str, is_error: bool) -> Value {
    let mut result = json!({"content":"fixture result"});
    if is_error {
        result["isError"] = json!(true);
    }
    json!({"turn_id":"turn","request_id":request,"call_event_id":call_event,"results":[{"call_id":call,"result":result}]})
}
fn end(wire: &mut Wire, sequence: u64, status: &str) {
    wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":format!("end-{sequence}"),"sequence":sequence,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"turn","status":status,"reason":"fixture"}}}));
}
fn fail(wire: &mut Wire, request: &Value, code: i32, app_error: &str) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":code,"message":format!("fixture {app_error}"),"data":{"appError":app_error}}}));
}
fn database() -> Value {
    json!({"header":"Database","question":"Which database should the service use?",
        "options":[{"label":"PostgreSQL","description":"managed, relational"},{"label":"SQLite","description":"single file"}]})
}
fn features() -> Value {
    json!({"question":"Which features ship first?","multiSelect":true,
        "options":[{"label":"Auth"},{"label":"Billing"},{"label":"Search"}]})
}
fn today() -> Value {
    json!({"question":"Ship today?","allowCustom":false,"options":[{"label":"Yes"},{"label":"No"}]})
}
/// `ask_user` inputs the handler rejects at once (A.1), one rule each, and
/// the text that would show if one were asked. An empty list, the remaining
/// rule, has nothing to show.
fn rejected() -> Vec<(Value, &'static str)> {
    let question = |text: &str| json!({"question":text,"options":[{"label":"a"},{"label":"b"}]});
    let with = |text: &'static str, key: &str, value: Value| {
        let mut question = question(text);
        question[key] = value;
        (json!({"questions":[question]}), text)
    };
    let mut same_id = question("Same id?");
    same_id["id"] = json!("x");
    let many: Vec<_> = (0..7).map(|n| json!({"label":n.to_string()})).collect();
    vec![
        (json!({"questions":vec![question("Five?"); 5]}), "Five?"),
        (
            json!({"questions":[question("Extra?")],"timeoutMs":1}),
            "Extra?",
        ),
        (json!({"questions":[same_id.clone(), same_id]}), "Same id?"),
        with("Empty id?", "id", json!("")),
        with("Long header?", "header", json!("h".repeat(41))),
        with("Unknown field?", "timeoutMs", json!(1)),
        with("Too few?", "options", json!([{"label":"a"}])),
        with("Too many?", "options", json!(many)),
        with(
            "Blank label?",
            "options",
            json!([{"label":" "},{"label":"b"}]),
        ),
        with(
            "Same label?",
            "options",
            json!([{"label":"a"},{"label":"a"}]),
        ),
        (
            json!({"questions":[{"question":" ","options":[{"label":"blank question"},{"label":"b"}]}]}),
            "blank question",
        ),
    ]
}
fn deploy() -> Value {
    json!({"header":"Deploy","question":"Where to deploy?","options":[{"label":"production"},{"label":"staging"}]})
}
/// The opening `session.state` notices and the first `child` history read.
/// `main` was read at the attach.
fn child_state(wire: &mut Wire) -> Value {
    for branch in ["main", "child"] {
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":branch,"running":true,"stopping":false}),
        );
    }
    let history = wire.read("session.history");
    assert_eq!(
        history["params"],
        json!({"sessionId":"session","branchId":"child","after":0,"limit":256})
    );
    history
}
/// Two history pages of `child`; the second holds a `step/tool-call` with
/// `calls`, asked before this client attached.
fn child_history(wire: &mut Wire, history: &Value, calls: Value) {
    wire.reply(
        history,
        json!({"events":[],"through":3,"nextAfter":2,"done":false}),
    );
    let next = wire.read("session.history");
    assert_eq!(
        next["params"],
        json!({"sessionId":"session","branchId":"child","after":2,"limit":256,"through":3})
    );
    wire.reply(
        &next,
        json!({"events":[event("child", 3, "tool-call", calls)],"through":3,"nextAfter":3,"done":true}),
    );
}

/// A.5: one question at a time with its options; an invalid line asks
/// again; several labels, "Other" text and an empty first answer (decline)
/// become `session.question.respond` params. Input the handler rejects and
/// other tools ask nothing. An approval waits for an open question and a
/// question for an open approval; other notices wait too. A result read
/// before the reply leaves the reply to decide; a call id used again is
/// asked again. A branch whose history cannot be read is reported, unless
/// it closed. A shell result waits for the question too.
#[test]
fn interactive_questions_take_choices_custom_text_and_a_decline() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working"}));
        let run = wire.read("shell.run");
        assert_eq!(run["params"]["command"], "inspect");
        let mut calls: Vec<_> = rejected()
            .into_iter()
            .map(|(input, _)| input)
            .chain([json!({"questions":[]})])
            .enumerate()
            .map(|(index, input)| json!({"id":format!("r{index}"),"name":"ask_user","input":input}))
            .collect();
        calls.push(json!({"id":"e1","name":"echo","input":{"questions":[deploy()]}}));
        calls.push(json!({"id":"c1","name":"ask_user","input":{"questions":[database(), features()]}}));
        step(wire, "main", 10, "tool-call", json!({"turn_id":"turn","request_id":"r1","calls":calls}));
        step(wire, "main", 11, "text", json!({"turn_id":"turn","request_id":"later","delta":"deferred text"}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"approval","call":{"name":"exec","input":{"command":"true"}},"reason":"shell command"}));
        wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"authoritative output","truncated":false,"durationMs":1}));
        let respond = wire.read("session.question.respond");
        assert_eq!(
            respond["params"],
            json!({"sessionId":"session","branchId":"main","requestId":"r1","callId":"c1","answers":[
                {"questionId":"1","selected":["PostgreSQL"]},
                {"questionId":"2","selected":["Auth","Search"],"custom":"audit log"}]})
        );
        step(wire, "main", 12, "tool-result", answered("main-10", "r1", "c1", false));
        wire.reply(&respond, json!({}));
        let approval = wire.read("approval.respond");
        assert_eq!(approval["params"]["decision"], "deny");
        wire.reply(&approval, json!({}));
        end(wire, 13, "completed");
        wire.reply(&idle, json!({"sequence":13}));

        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"approval2","call":{"name":"exec","input":{"command":"true"}},"reason":"shell command"}));
        for branch in ["child", "gone"] {
            wire.notice(
                "session.state",
                json!({"sessionId":"session","branchId":branch,"running":true,"stopping":false}),
            );
        }
        step(wire, "main", 14, "tool-call", asked("r2", "c1", json!([today()])));
        let child = wire.read("session.history");
        assert_eq!(child["params"]["branchId"], "child");
        let gone = wire.read("session.history");
        assert_eq!(gone["params"]["branchId"], "gone");
        fail(wire, &gone, -32000, "branch_missing");
        // Printed after the call above was read, while the approval waits.
        fail(wire, &child, -32000, "internal_error");
        let approval = wire.read("approval.respond");
        assert_eq!(approval["params"]["requestId"], "approval2");
        wire.reply(&approval, json!({}));
        let respond = wire.read("session.question.respond");
        assert_eq!(
            respond["params"],
            json!({"sessionId":"session","branchId":"main","requestId":"r2","callId":"c1","declined":true})
        );
        wire.reply(&respond, json!({}));
        step(wire, "main", 15, "tool-result", answered("main-14", "r2", "c1", false));
        end(wire, 16, "completed");
        wire.reply(&idle, json!({"sequence":16}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"plan\n");
    terminal.until("model working");
    terminal.write(b"!!inspect\n");
    terminal.until("Choose 1-2, 0 for other, empty to decline: ");
    for invalid in ["9", "1,2"] {
        terminal.write(format!("{invalid}\n").as_bytes());
        terminal.until("Choose 1-2, 0 for other, empty to decline: ");
    }
    terminal.write(b"1\n");
    terminal.until("Choose numbers, comma separated, 0 for other: ");
    terminal.write(b"1,3,0\n");
    terminal.until("Other: ");
    terminal.write(b"audit log\n");
    terminal.until("[question answered]");
    terminal.until("Allow once? [y/N] ");
    terminal.write(b"\n");
    terminal.until("\n> ");
    terminal.write(b"again\n");
    terminal.until("Allow once? [y/N] ");
    terminal.until("[question] Cannot read the questions of branch child: ");
    terminal.write(b"\n");
    terminal.until("Choose 1-2, empty to decline: ");
    terminal.write(b"\n");
    terminal.until("[question answered]");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(wait(child).status.success(), "{}", terminal.transcript);
    daemon.join().unwrap();
    let transcript = &terminal.transcript;
    for line in [
        "[question] Database",
        "1. Which database should the service use?",
        "   1. PostgreSQL  managed, relational",
        "   2. SQLite      single file",
        "   0. Other (type your answer)",
        "2. Which features ship first?",
        "   3. Search",
        "1. Ship today?",
    ] {
        assert!(transcript.contains(line), "{line}: {transcript}");
    }
    for absent in ["answered elsewhere", "Where to deploy?", "branch gone"] {
        assert!(!transcript.contains(absent), "{absent}: {transcript}");
    }
    for (_, text) in rejected() {
        assert!(!transcript.contains(text), "{text}: {transcript}");
    }
    let answered = transcript.find("[question answered]").unwrap();
    for later in ["[approval] exec", "deferred text", "authoritative output"] {
        assert!(
            answered < transcript.find(later).unwrap(),
            "{later}: {transcript}"
        );
    }
}

/// A9 and A.3: a question asked on another branch before the attach is read
/// from that branch's history once and answered with its `branchId`.
/// `invalid_params` asks again; other failures and `question_not_pending`
/// keep the answer for a resend. A prompt closes on its call's
/// `step/tool-result` when another client answered or the call was
/// cancelled, also while this client's answer is in flight.
#[test]
fn questions_of_every_branch_are_answered_and_close_on_their_results() {
    let home = Home::new();
    let (shown, prompt_shown) = mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        let history = child_state(wire);
        child_history(wire, &history, asked("rk", "k1", json!([deploy()])));
        // The same call again, live: still one prompt.
        step(wire, "child", 3, "tool-call", asked("rk", "k1", json!([deploy()])));
        let respond = wire.read("session.question.respond");
        assert_eq!(
            respond["params"],
            json!({"sessionId":"session","branchId":"child","requestId":"rk","callId":"k1","answers":[{"questionId":"1","selected":["staging"]}]})
        );
        fail(wire, &respond, -32602, "invalid_params");
        let again = wire.read("session.question.respond");
        assert_eq!(again["params"], respond["params"]);
        fail(wire, &again, -32000, "service_reloading");
        let again = wire.read("session.question.respond");
        fail(wire, &again, -32000, "question_not_pending");
        let again = wire.read("session.question.respond");
        assert_eq!(again["params"], respond["params"]);
        wire.reply(&again, json!({}));
        step(wire, "child", 4, "tool-result", answered("child-3", "rk", "k1", false));

        step(wire, "main", 10, "tool-call", asked("rm1", "m1", json!([today()])));
        prompt_shown.recv().unwrap();
        step(wire, "main", 11, "tool-result", answered("main-10", "rm1", "m1", false));
        step(wire, "main", 12, "tool-call", asked("rm2", "m2", json!([today()])));
        prompt_shown.recv().unwrap();
        step(wire, "main", 13, "tool-result", answered("main-12", "rm2", "m2", true));
        // Another client's answer wins while this one is in flight.
        step(wire, "main", 14, "tool-call", asked("rm3", "m3", json!([today()])));
        let respond = wire.read("session.question.respond");
        step(wire, "main", 15, "tool-result", answered("main-14", "rm3", "m3", false));
        fail(wire, &respond, -32000, "question_not_pending");
        // Any failure after the result closes the prompt the same way.
        step(wire, "main", 16, "tool-call", asked("rm4", "m4", json!([today()])));
        let respond = wire.read("session.question.respond");
        step(wire, "main", 17, "tool-result", answered("main-16", "rm4", "m4", true));
        fail(wire, &respond, -32000, "service_reloading");
        end(wire, 18, "completed");
        wire.reply(&idle, json!({"sequence":18}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"go\n");
    terminal.until("[question] Deploy (branch child)");
    terminal.until("Choose 1-2, 0 for other, empty to decline: ");
    terminal.write(b"2\n");
    terminal.until("[question rejected] fixture invalid_params");
    terminal.until("Choose 1-2, 0 for other, empty to decline: ");
    terminal.write(b"2\n");
    terminal.until("service_reloading); press Enter to send again");
    terminal.write(b"\n");
    terminal.until("[question not ready; press Enter to send again]");
    terminal.write(b"\n");
    terminal.until("[question answered]");
    terminal.until("Choose 1-2, empty to decline: ");
    shown.send(()).unwrap();
    terminal.until("[question answered elsewhere]");
    terminal.until("Choose 1-2, empty to decline: ");
    shown.send(()).unwrap();
    terminal.until("[question cancelled]");
    terminal.until("Choose 1-2, empty to decline: ");
    terminal.write(b"1\n");
    terminal.until("[question answered elsewhere]");
    terminal.until("Choose 1-2, empty to decline: ");
    terminal.write(b"2\n");
    terminal.until("[question cancelled]");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(wait(child).status.success(), "{}", terminal.transcript);
    daemon.join().unwrap();
    assert_eq!(
        terminal.transcript.matches("Where to deploy?").count(),
        2,
        "asked, and asked again after the rejection only: {}",
        terminal.transcript
    );
}

/// §4.3: a later model request asks with a call id an earlier one used. The
/// later call arrives live before the history page holding the earlier call
/// and its result: that result settles only the earlier call, the later
/// prompt stays open, and the answer names the later call's request.
#[test]
fn a_reused_call_id_is_answered_with_its_own_request() {
    let home = Home::new();
    let (read, page_read) = mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        let history = child_state(wire);
        step(wire, "child", 5, "tool-call", asked("r2", "k1", json!([deploy()])));
        wire.reply(
            &history,
            json!({"events":[
                event("child", 3, "tool-call", asked("r1", "k1", json!([today()]))),
                event("child", 4, "tool-result", answered("child-3", "r1", "k1", false))],
                "through":5,"nextAfter":4,"done":false}),
        );
        // The next page is read once the first one was applied.
        let next = wire.read("session.history");
        read.send(()).unwrap();
        wire.reply(
            &next,
            json!({"events":[event("child", 5, "tool-call", asked("r2", "k1", json!([deploy()])))],
                "through":5,"nextAfter":5,"done":true}),
        );
        let respond = wire.read("session.question.respond");
        assert_eq!(
            respond["params"],
            json!({"sessionId":"session","branchId":"child","requestId":"r2","callId":"k1","answers":[{"questionId":"1","selected":["staging"]}]})
        );
        wire.reply(&respond, json!({}));
        step(wire, "child", 6, "tool-result", answered("child-5", "r2", "k1", false));
        end(wire, 7, "completed");
        wire.reply(&idle, json!({"sequence":7}));
        wire.detach();
    });
    let (mut terminal, child) = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"go\n");
    terminal.until("[question] Deploy (branch child)");
    terminal.until("Choose 1-2, 0 for other, empty to decline: ");
    page_read.recv().unwrap();
    terminal.write(b"2\n");
    terminal.until("[question answered]");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(wait(child).status.success(), "{}", terminal.transcript);
    daemon.join().unwrap();
    for absent in ["Ship today?", "answered elsewhere"] {
        assert!(
            !terminal.transcript.contains(absent),
            "{absent}: {}",
            terminal.transcript
        );
    }
    assert_eq!(
        terminal.transcript.matches("Where to deploy?").count(),
        1,
        "{}",
        terminal.transcript
    );
}

/// A.5: Ctrl+C or EOF at a question of another branch stops that branch
/// once, which ends its other call too, and exits with 130; nothing else is
/// asked. While `main` runs, Ctrl+C stops `main` too, before a shell run, as
/// at an approval. The reply to an answer in flight is drained. A call read
/// after `main` settled is still asked.
#[test]
fn ctrl_c_or_eof_at_a_question_stops_its_branch() {
    for case in ["eof", "ctrl-c", "running", "in-flight"] {
        let home = Home::new();
        let (sent, answer_sent) = mpsc::channel::<()>();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let idle = wire.read("session.idle");
            let run = (case == "running").then(|| {
                wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working"}));
                wire.read("shell.run")
            });
            let history = child_state(wire);
            if case != "running" {
                wire.reply(&idle, json!({"sequence":8}));
            }
            let mut calls = asked("rk", "k1", json!([deploy()]));
            calls["calls"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":"k2","name":"ask_user","input":{"questions":[today()]}}));
            child_history(wire, &history, calls);
            if case == "running" {
                step(
                    wire,
                    "main",
                    9,
                    "tool-call",
                    asked("rm", "m1", json!([database()])),
                );
            }
            let respond = (case == "in-flight").then(|| {
                let respond = wire.read("session.question.respond");
                sent.send(()).unwrap();
                respond
            });
            let stop = wire.read("session.stop");
            assert_eq!(
                stop["params"],
                json!({"sessionId":"session","branchId":"child","reason":"CLI interrupted"})
            );
            if let Some(respond) = respond {
                wire.reply(&respond, json!({}));
            }
            wire.reply(&stop, json!({"sequence":5}));
            if case == "running" {
                let stop = wire.read("session.stop");
                assert_eq!(
                    stop["params"],
                    json!({"sessionId":"session","branchId":"main","reason":"CLI interrupted"})
                );
                let run = run.unwrap();
                wire.reply(&run, json!({"runId":run["params"]["runId"],"status":"completed","exitCode":0,"output":"","truncated":false,"durationMs":1}));
                end(wire, 10, "cancelled");
                wire.reply(&stop, json!({"sequence":10}));
                wire.reply(&idle, json!({"sequence":10}));
            }
            wire.detach();
        });
        let (mut terminal, child) = Terminal::launch(home.command());
        if case == "running" {
            terminal.until("model working");
            terminal.write(b"!!inspect\n");
        }
        terminal.until("[question] Deploy (branch child)");
        terminal.until("empty to decline: ");
        if case == "in-flight" {
            terminal.write(b"2\n");
            answer_sent.recv().unwrap();
        }
        if case == "eof" {
            terminal.write(&[4]);
        } else {
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
        }
        assert_eq!(
            wait(child).status.code(),
            Some(130),
            "{case}: {}",
            terminal.transcript
        );
        daemon.join().unwrap();
        for absent in ["Ship today?", "Which database"] {
            assert!(
                !terminal.transcript.contains(absent),
                "{case}: {}",
                terminal.transcript
            );
        }
    }
}

/// A8: without terminal input every question read stops its branch, and
/// `main` while it runs, and the CLI exits with 3 after every stop reply. A
/// call whose result was read before its history asks nothing.
#[test]
fn exec_without_a_terminal_stops_at_a_question_with_exit_code_3() {
    for case in ["main", "child", "child after main", "settled"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            match case {
                "main" => {
                    step(wire, "main", 10, "tool-call", asked("rc", "c1", json!([database()])));
                    let stop = wire.read("session.stop");
                    assert_eq!(
                        stop["params"],
                        json!({"sessionId":"session","branchId":"main","reason":"Interactive answer required"})
                    );
                    // A question read after `main` settled is given up too.
                    let history = child_state(wire);
                    end(wire, 11, "cancelled");
                    wire.reply(&stop, json!({"sequence":11}));
                    wire.reply(&idle, json!({"sequence":11}));
                    child_history(wire, &history, asked("rk", "k1", json!([deploy()])));
                    let child = wire.read("session.stop");
                    assert_eq!(child["params"]["branchId"], "child");
                    wire.reply(&child, json!({"sequence":5}));
                }
                "child" => {
                    let history = child_state(wire);
                    child_history(wire, &history, asked("rk", "k1", json!([deploy()])));
                    let child = wire.read("session.stop");
                    assert_eq!(
                        child["params"],
                        json!({"sessionId":"session","branchId":"child","reason":"Interactive answer required"})
                    );
                    let stop = wire.read("session.stop");
                    assert_eq!(stop["params"]["branchId"], "main");
                    end(wire, 11, "cancelled");
                    wire.reply(&stop, json!({"sequence":11}));
                    wire.reply(&idle, json!({"sequence":11}));
                    wire.reply(&child, json!({"sequence":5}));
                }
                "child after main" => {
                    let history = child_state(wire);
                    end(wire, 11, "completed");
                    wire.reply(&idle, json!({"sequence":11}));
                    child_history(wire, &history, asked("rk", "k1", json!([deploy()])));
                    let stop = wire.read("session.stop");
                    assert_eq!(
                        stop["params"],
                        json!({"sessionId":"session","branchId":"child","reason":"Interactive answer required"})
                    );
                    wire.reply(&stop, json!({"sequence":5}));
                }
                _ => {
                    let history = child_state(wire);
                    step(wire, "child", 4, "tool-result", answered("child-3", "rk", "k1", true));
                    child_history(wire, &history, asked("rk", "k1", json!([deploy()])));
                    end(wire, 11, "completed");
                    wire.reply(&idle, json!({"sequence":11}));
                }
            }
            wire.detach();
        });
        let output = wait(
            home.command()
                .args(["exec", "--prompt", "plan"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        daemon.join().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let required = "Interactive answer required; stopping. Rerun in a terminal to answer it";
        if case == "settled" {
            assert_eq!(output.status.code(), Some(0), "{case}: {stderr}");
            assert!(!stderr.contains(required), "{stderr}");
        } else {
            assert_eq!(output.status.code(), Some(3), "{case}: {stderr}");
            assert_eq!(stderr.matches(required).count(), 1, "{stderr}");
        }
        assert!(!stderr.contains("Which database"), "{stderr}");
    }
}
