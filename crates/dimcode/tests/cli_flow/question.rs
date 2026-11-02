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
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working","thinking":"","thinkingOffset":0}));
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
    let mut terminal = Terminal::launch(home.command());
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
    assert!(terminal.wait().success(), "{}", terminal.transcript);
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
        step(wire, "main", 14, "tool-call", asked("rm3", "m3", json!([today()])));
        let respond = wire.read("session.question.respond");
        step(wire, "main", 15, "tool-result", answered("main-14", "rm3", "m3", false));
        fail(wire, &respond, -32000, "question_not_pending");
        step(wire, "main", 16, "tool-call", asked("rm4", "m4", json!([today()])));
        let respond = wire.read("session.question.respond");
        step(wire, "main", 17, "tool-result", answered("main-16", "rm4", "m4", true));
        fail(wire, &respond, -32000, "service_reloading");
        end(wire, 18, "completed");
        wire.reply(&idle, json!({"sequence":18}));
        wire.detach();
    });
    let mut terminal = Terminal::launch(home.command());
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
    assert!(terminal.wait().success(), "{}", terminal.transcript);
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
    let mut terminal = Terminal::launch(home.command());
    terminal.until("\n> ");
    terminal.write(b"go\n");
    terminal.until("[question] Deploy (branch child)");
    terminal.until("Choose 1-2, 0 for other, empty to decline: ");
    page_read.recv().unwrap();
    terminal.write(b"2\n");
    terminal.until("[question answered]");
    terminal.until("\n> ");
    terminal.write(&[4]);
    assert!(terminal.wait().success(), "{}", terminal.transcript);
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
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let idle = wire.read("session.idle");
            let run = (case == "running").then(|| {
                wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","textOffset":0,"text":"model working","thinking":"","thinkingOffset":0}));
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
        let mut terminal = Terminal::launch(home.command());
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
            terminal.signal(libc::SIGINT);
        }
        assert_eq!(
            terminal.wait().code(),
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

#[test]
fn tui_questions_keep_structured_answers_and_the_leader_draft() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (proceed, control) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            control.recv_timeout(Duration::from_secs(10)).unwrap();
            let mut first = database();
            first["allowCustom"] = json!(false);
            let mut second = features();
            let label = "Auth\u{1b}]52;c;question-must-not-copy\u{7}";
            second["options"][0]["label"] = json!(label);
            step(wire, "main", 9, "tool-call", asked("request-one", "c1", json!([first, second])));
            let response = wire.read("session.question.respond");
            assert_eq!(response["params"], json!({
                "sessionId":"session","branchId":"main","requestId":"request-one","callId":"c1",
                "answers":[{"questionId":"1","selected":["SQLite"]},
                    {"questionId":"2","selected":[label,"Search"],"custom":"Auth"}],
            }));
            wire.reply(&response, json!({}));
            step(wire, "main", 10, "tool-result", answered("main-9", "request-one", "c1", false));
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], "kept leader draft and continue");
            wire.reply(&submit, json!({"event":{}}));
            step(wire, "main", 11, "text", json!({"turn_id":"turn","request_id":"followup","delta":"continued after question"}));
            let mut line = String::new();
            assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected action: {line}");
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        terminal.write(b"kept leader draft");
        proceed.send(()).unwrap();
        terminal.until("Which database should the service use?");
        terminal.until("SQLite");
        assert!(!terminal.transcript.contains("Type your own answer"));
        terminal.write(b"2");
        terminal.until("Which features ship first?");
        terminal.write(b"134Auth\r\t");
        terminal.until("Review your answers");
        terminal.write(b"\r");
        terminal.until("kept leader draft");
        terminal.write(b" and continue\r");
        terminal.until("continued after question");
        assert!(!terminal.transcript.contains("question-must-not-copy"));
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_questions_retry_and_keep_call_identity_across_late_replies() {
    let home = Home::new();
    let (proceed, control) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[event("main",9,"tool-call",asked("old-request","c1",json!([deploy()])))],"through":9,"nextAfter":9,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let first = wire.read("session.question.respond");
        fail(wire, &first, -32000, "question_not_pending");
        let retry = wire.read("session.question.respond");
        assert_eq!(retry["params"], first["params"]);
        fail(wire, &retry, -32602, "invalid_params");
        let held = wire.read("session.question.respond");
        assert_eq!(held["params"], first["params"]);
        step(wire, "main",10,"tool-result",answered("main-9","old-request","c1",false));
        let a = asked("new-request","c1",json!([deploy()]));
        let mut calls = a["calls"].as_array().unwrap().clone();
        calls.push(json!({"id":"c2","name":"ask_user","input":{"questions":[today()]}}));
        step(wire,"main",11,"tool-call",json!({"turn_id":"turn","request_id":"new-request","calls":calls}));
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        fail(wire, &held, -32000, "late-old-response");
        let decline = wire.read("session.question.respond");
        assert_eq!(decline["params"],json!({"sessionId":"session","branchId":"main","requestId":"new-request","callId":"c1","declined":true}));
        wire.reply(&decline,json!({}));
        let second = wire.read("session.question.respond");
        assert_eq!(second["params"],json!({"sessionId":"session","branchId":"main","requestId":"new-request","callId":"c2","answers":[{"questionId":"1","selected":["No"]}]}));
        wire.reply(&second,json!({}));
        step(wire,"main",12,"tool-result",json!({"turn_id":"turn","request_id":"new-request","call_event_id":"main-11","results":[
            {"call_id":"c1","result":{"content":"declined"}}, {"call_id":"c2","result":{"content":"answered"}},
        ]}));
        step(wire,"main",13,"text",json!({"turn_id":"turn","request_id":"after","delta":"all three calls settled"}));
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(),0,"unexpected retry: {line}");
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("Where to deploy?");
    terminal.write(b"1\r");
    terminal.until("Question not ready");
    terminal.write(b"\r");
    terminal.until("fixture invalid_params");
    terminal.write(b"\r");
    terminal.until("Where to deploy?");
    proceed.send(()).unwrap();
    terminal.write(b"\x1b[27u");
    terminal.until("Ship today?");
    terminal.write(b"2\r");
    terminal.until("all three calls settled");
    assert!(!terminal.transcript.contains("late-old-response"));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_questions_restore_a_child_and_interrupt_its_branch() {
    let home = Home::new();
    let (continue_shell, shell_completion) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["shell.run", "shell.stop"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let shell = wire.read("shell.run");
        assert_eq!(shell["params"]["mode"], "private");
        let child = child_state(wire);
        child_history(
            wire,
            &child,
            asked("child-request", "c1", json!([deploy()])),
        );
        let stop = wire.read("session.stop");
        assert_eq!(stop["params"]["sessionId"], "session");
        assert_eq!(stop["params"]["branchId"], "child");
        assert_eq!(stop["params"]["reason"], "User interrupted the run.");
        wire.reply(&stop, json!({}));
        let main = wire.read("session.stop");
        assert_eq!(main["params"]["sessionId"], "session");
        assert_eq!(main["params"]["branchId"], "main");
        assert_eq!(main["params"]["reason"], "User interrupted the run.");
        wire.reply(&main, json!({}));
        step(
            wire,
            "child",
            4,
            "tool-result",
            answered("child-3", "child-request", "c1", true),
        );
        step(
            wire,
            "main",
            9,
            "text",
            json!({"turn_id":"turn","request_id":"after","delta":"child question interrupted"}),
        );
        shell_completion.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.reply(&shell, json!({"runId":shell["params"]["runId"],"status":"completed","exitCode":0,"output":"separate shell completed normally","truncated":false,"durationMs":1}));
        let mut line = String::new();
        assert_eq!(
            wire.0.read_line(&mut line).unwrap(),
            0,
            "unexpected answer: {line}"
        );
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"!!inspect\r");
    terminal.until("Question on branch child");
    terminal.until("Where to deploy?");
    terminal.write(&[3]);
    terminal.until("child question interrupted");
    continue_shell.send(()).unwrap();
    terminal.until("separate shell completed normally");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_member_questions_stay_read_only_and_a_leader_question_takes_the_prompt() {
    let home = Home::new();
    let (proceed, control) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[
            {"id":"request","sequence":1,"create_at":1,"type":"collaboration","name":"request","data":{"turn_id":"t","request_id":"r","call_id":"agent","target":{"kind":"agent","agent_id":"reviewer"},"prompt":"Inspect readonly member question"}},
            {"id":"delivery","sequence":2,"create_at":2,"type":"collaboration","name":"delivered","data":{"op_event_id":"request","status":"ok","agent_id":"reviewer","session_id":"member"}},
            {"id":"result","sequence":3,"create_at":3,"type":"collaboration","name":"result","data":{"request_event_id":"request","agent_id":"reviewer","session_id":"member","status":"completed","text":"done"}},
        ],"through":3,"nextAfter":3,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([]));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["sessionId"],"member");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["sessionId"],"member");
        let mut peer = today();
        peer["question"] = json!("Member question must not be answered here");
        wire.reply(&history,json!({"events":[
            event("main",1,"tool-call",asked("peer-request","c1",json!([peer]))),
            event("main",2,"text",json!({"turn_id":"member-turn","request_id":"peer-request","delta":"member view is still read only"})),
        ],"through":2,"nextAfter":2,"done":true}));
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.quiet(Duration::from_millis(100));
        let mut leader = today();
        leader["question"] = json!("Leader question takes priority");
        step(wire,"main",4,"tool-call",asked("leader-request","c1",json!([leader])));
        let mut detached = false;
        let mut leader_answered = false;
        while !detached || !leader_answered {
            let mut line=String::new();
            wire.0.read_line(&mut line).unwrap();
            let request:Value=serde_json::from_str(&line).unwrap();
            match request["method"].as_str().unwrap() {
                "session.detach" => {
                    assert_eq!(request["params"],json!({"sessionId":"member"}));
                    assert!(!detached);
                    detached=true;
                    wire.reply(&request,json!({}));
                }
                "session.question.respond" => {
                    assert_eq!(request["params"],json!({"sessionId":"session","branchId":"main","requestId":"leader-request","callId":"c1","answers":[{"questionId":"1","selected":["Yes"]}]}));
                    assert!(!leader_answered);
                    leader_answered=true;
                    wire.reply(&request,json!({}));
                }
                other => panic!("Unexpected member/leader action: {other}"),
            }
        }
        step(wire,"main",5,"tool-result",answered("main-4","leader-request","c1",false));
        step(wire,"main",6,"text",json!({"turn_id":"leader-turn","request_id":"after","delta":"leader answer preserved readonly boundary"}));
        let mut line=String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(),0,"unexpected peer action: {line}");
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/tasks\r");
    terminal.until("Inspect readonly member question");
    terminal.write(b"\r");
    terminal.until("member view is still read only");
    terminal.write(b"not an answer\r");
    proceed.send(()).unwrap();
    terminal.until("Leader question takes priority");
    terminal.write(b"1\r");
    terminal.until("leader answer preserved readonly boundary");
    assert!(
        !terminal
            .transcript
            .contains("Member question must not be answered here")
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

/// A pending question does not block commands: `/` and `Ctrl+O` open the
/// palette over it, `/mcp list` prints its report and leaves the question
/// pending (nothing is answered), a command that opens a panel hands the
/// question back with its answer, and `Esc` in the palette returns to the
/// question instead of declining it.
#[test]
fn tui_commands_run_over_a_pending_question() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (proceed, control) = mpsc::channel();
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
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            control.recv_timeout(Duration::from_secs(10)).unwrap();
            step(
                wire,
                "main",
                9,
                "tool-call",
                asked("request-one", "c1", json!([deploy()])),
            );
            let catalog = wire.read("mcp.catalog");
            wire.reply(
                &catalog,
                json!({"userRevision":"1","projectRevision":null,"servers":[],
                    "userApplication":{},"projectApplication":null}),
            );
            let state = wire.read("session.mcp");
            wire.reply(&state, json!({"sessionId":"session","servers":[]}));
            let response = wire.read("session.question.respond");
            assert_eq!(
                response["params"],
                json!({"sessionId":"session","branchId":"main","requestId":"request-one","callId":"c1",
                    "answers":[{"questionId":"1","selected":["production"]}]}),
                "the question answers with what it kept over the commands"
            );
            wire.reply(&response, json!({}));
            step(
                wire,
                "main",
                10,
                "tool-result",
                answered("main-9", "request-one", "c1", false),
            );
            step(
                wire,
                "main",
                11,
                "text",
                json!({"turn_id":"turn","request_id":"after","delta":"question answered after commands"}),
            );
            let mut line = String::new();
            assert_eq!(
                wire.0.read_line(&mut line).unwrap(),
                0,
                "unexpected request: {line}"
            );
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        proceed.send(()).unwrap();
        terminal.until("Where to deploy?");
        terminal.write(b"1");
        terminal.until("Review your answers");

        terminal.write(b"/mcp list\r");
        terminal.until("No MCP server is configured.");

        terminal.write(b"\x0f");
        terminal.until(if fullscreen {
            "Command Palette"
        } else {
            "Commands"
        });
        terminal.write(&[0x1b]);
        terminal.until("Review your answers");

        terminal.write(b"\x0f");
        terminal.write(b"permissions\r");
        terminal.until("Process isolation");
        terminal.write(&[0x1b]);
        terminal.until("production");
        terminal.write(b"\r");
        terminal.until("question answered after commands");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// A command's argument is typed over a pending question: `/mcp logout`
/// carries the server id to the daemon, nothing answers the question before
/// its confirmation, and the question then answers normally.
#[test]
fn tui_command_arguments_run_over_a_pending_question() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (proceed, control) = mpsc::channel();
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
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            control.recv_timeout(Duration::from_secs(10)).unwrap();
            step(
                wire,
                "main",
                9,
                "tool-call",
                asked("request-one", "c1", json!([deploy()])),
            );
            let catalog = wire.read("mcp.catalog");
            wire.reply(
                &catalog,
                json!({"userRevision":"1","projectRevision":null,
                    "servers":[{"id":"fixture","source":"user","enabled":true,"transport":"stdio"}],
                    "userApplication":{},"projectApplication":null}),
            );
            let logout = wire.read("mcp.auth.logout");
            assert_eq!(
                logout["params"]["id"], "fixture",
                "the argument typed over the question reached the daemon"
            );
            wire.reply(&logout, json!({"revoked":true}));
            let response = wire.read("session.question.respond");
            assert_eq!(
                response["params"],
                json!({"sessionId":"session","branchId":"main","requestId":"request-one","callId":"c1",
                    "answers":[{"questionId":"1","selected":["production"]}]}),
                "the question answers with what it kept over the command"
            );
            wire.reply(&response, json!({}));
            step(
                wire,
                "main",
                10,
                "tool-result",
                answered("main-9", "request-one", "c1", false),
            );
            step(
                wire,
                "main",
                11,
                "text",
                json!({"turn_id":"turn","request_id":"after","delta":"question answered after the argument"}),
            );
            let mut line = String::new();
            assert_eq!(
                wire.0.read_line(&mut line).unwrap(),
                0,
                "unexpected request: {line}"
            );
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        proceed.send(()).unwrap();
        terminal.until("Where to deploy?");
        terminal.write(b"1");
        terminal.until("Review your answers");

        terminal.write(b"/mcp logout fixture\r");
        terminal.until("Signed out");

        terminal.write(b"\r");
        terminal.until("question answered after the argument");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

/// `/` inside a free-text answer is text, so an answer can start with a
/// path; and a question answered elsewhere while the palette is open leaves
/// the palette as the ordinary one, over which a message still submits.
#[test]
fn tui_free_text_answers_keep_their_slashes_and_the_palette_survives_a_dropped_question() {
    let home = Home::new();
    let (proceed, control) = mpsc::channel();
    let (typed, palette_typed) = mpsc::channel();
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
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        step(
            wire,
            "main",
            9,
            "tool-call",
            asked("request-one", "c1", json!([deploy()])),
        );
        let response = wire.read("session.question.respond");
        assert_eq!(
            response["params"],
            json!({"sessionId":"session","branchId":"main","requestId":"request-one","callId":"c1",
                "answers":[{"questionId":"1","selected":[],"custom":"/usr/local"}]}),
            "/ inside the free-text answer is text"
        );
        wire.reply(&response, json!({}));
        step(
            wire,
            "main",
            10,
            "tool-result",
            answered("main-9", "request-one", "c1", false),
        );
        step(
            wire,
            "main",
            11,
            "tool-call",
            asked("request-two", "c2", json!([database()])),
        );
        palette_typed.recv_timeout(Duration::from_secs(10)).unwrap();
        step(
            wire,
            "main",
            12,
            "tool-result",
            answered("main-11", "request-two", "c2", false),
        );
        step(
            wire,
            "main",
            13,
            "text",
            json!({"turn_id":"turn","request_id":"after","delta":"question dropped under the palette"}),
        );
        let submit = wire.read("session.submit");
        assert_eq!(
            submit["params"]["text"], "done",
            "the palette and the composer are the ordinary ones again"
        );
        wire.reply(&submit, json!({"event":{}}));
        step(
            wire,
            "main",
            14,
            "text",
            json!({"turn_id":"turn","request_id":"after-submit","delta":"submitted after the drop"}),
        );
        let mut line = String::new();
        assert_eq!(
            wire.0.read_line(&mut line).unwrap(),
            0,
            "unexpected request: {line}"
        );
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    proceed.send(()).unwrap();
    terminal.until("Where to deploy?");
    terminal.write(b"3");
    terminal.write(b"/usr/local\r");
    terminal.until("Review your answers");
    terminal.write(b"\r");
    terminal.until("Which database should the service use?");

    terminal.write(b"/keys");
    typed.send(()).unwrap();
    terminal.until("question dropped under the palette");
    terminal.write(b"\r");
    terminal.until("Application keys");
    terminal.write(b"done\r");
    terminal.until("submitted after the drop");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn native_tui_question_answer_reaches_the_next_model_request() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let model = thread::spawn(move || {
        for phase in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Native model phase {phase} was not requested"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = BufReader::new(stream);
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
            let request: Value = serde_json::from_slice(&body).unwrap();
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"question-fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = if phase == 0 {
                assert!(
                    request["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|tool| tool["function"]["name"] == "ask_user")
                );
                let mut question = today();
                question["question"] = json!("Native question reaches this TUI?");
                frame(
                    json!({"tool_calls":[{"index":0,"id":"native-question","type":"function","function":{"name":"ask_user","arguments":json!({"questions":[question]}).to_string()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } else {
                let answer = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|message| message["role"] == "tool")
                    .unwrap();
                assert!(
                    answer["content"]
                        .as_str()
                        .unwrap()
                        .contains("Native question reaches this TUI?\n   -> Yes"),
                    "{answer}"
                );
                frame(
                    json!({"content":"native answer reached the model"}),
                    json!("stop"),
                )
            } + "data: [DONE]\n\n";
            write!(stream.get_mut(),"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config,json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]}).to_string()).unwrap();
    let mut command = home.command();
    command.args([
        "--server",
        &binary,
        "--model-config",
        config.to_str().unwrap(),
        "--cwd",
        home.0.to_str().unwrap(),
        "--permission",
        "full-access",
    ]);
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("fixture / fixture-model");
    terminal.write(b"ask me a question\r");
    terminal.until("Native question reaches this TUI?");
    terminal.write(b"1\r");
    terminal.until("native answer reached the model");
    model.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}
