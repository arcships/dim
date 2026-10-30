//! The composer's context reading for a Session method the hello does not
//! list: the reading is asked for like `/usage` asks, and a failed call
//! leaves the border without one and adds no transcript line.
use super::*;
use std::sync::mpsc;

fn usage(input: f64) -> Value {
    json!({"contextWindow":256000.0,"outputTokens":32000.0,"thresholdTokens":200000.0,
        "estimatedInputTokens":input,"baselineSource":"estimate"})
}

fn failed(wire: &mut Wire, request: &Value) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":
        {"code":-32601,"message":"Unknown method: session.context.usage",
         "data":{"appError":"unknown_method"}}}));
}

/// The hello omits `session.context.usage`, which a Session contributes, and
/// the daemon answers it: the border shows `input / window` once the Session
/// is entered and reads it again when a turn on `main` ends.
#[test]
fn tui_shows_context_usage_the_hello_does_not_list() {
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
                Some("session.context.usage") => {
                    assert_eq!(
                        request["params"],
                        json!({"sessionId":"session","branchId":"main"})
                    );
                    wire.reply(&request, usage(1849.0));
                }
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "hello");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":true}),
        );
        wire.event(
            9,
            "text",
            json!({"turn_id":"turn","request_id":"request","delta":"answered"}),
        );
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"fixture"}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":false}),
        );
        let refresh = wire.read("session.context.usage");
        assert_eq!(
            refresh["params"],
            json!({"sessionId":"session","branchId":"main"})
        );
        wire.reply(&refresh, usage(64000.0));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(
                !line.contains("session.context.usage"),
                "extra read: {line}"
            );
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("1.8K / 256K");
    shown.send(()).unwrap();
    terminal.write(b"hello\r");
    terminal.until("answered");
    terminal.until("64K / 256K");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A daemon that answers `session.context.usage` with an error leaves the
/// border without a reading and prints nothing into the transcript, and the
/// next message still goes through.
#[test]
fn tui_shows_no_reading_and_keeps_working_when_context_usage_fails() {
    let home = Home::new();
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
        let mut asked = false;
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => {
                    asked = true;
                    failed(wire, &request);
                }
                method => panic!("unexpected request {method:?}"),
            }
        }
        assert!(asked, "the TUI did not ask for the context reading");
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "still here");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":true}),
        );
        wire.event(
            9,
            "text",
            json!({"turn_id":"turn","request_id":"request","delta":"the session still answers"}),
        );
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"fixture"}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":false}),
        );
        let refresh = wire.read("session.context.usage");
        failed(wire, &refresh);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"still here\r");
    terminal.until("the session still answers");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    for absent in [
        "K / 256K",
        "near the compaction threshold",
        "This daemon does not track context usage",
        "Context usage is known once a model has answered",
        "Context usage needs the model's capabilities",
    ] {
        assert!(
            !terminal.transcript.contains(absent),
            "{absent}: {}",
            terminal.transcript
        );
    }
}
