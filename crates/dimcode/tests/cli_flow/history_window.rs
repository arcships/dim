use super::*;

#[test]
fn tui_first_view_reads_at_most_four_latest_turn_pages() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.history.latestTurn"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));

        let mut before = None;
        for page in 0..4u64 {
            let request = wire.read("session.history");
            assert_eq!(request["params"]["latestTurn"], true);
            assert_eq!(request["params"]["limit"], 256);
            assert_eq!(request["params"]["through"], 8);
            assert!(request["params"].get("after").is_none());
            if let Some(before) = before {
                assert_eq!(request["params"]["before"], before);
            }
            let done = page == 3;
            let next = 6 - page * 2;
            let mut reply = json!({
                "events": [],
                "through": 8,
                "nextAfter": 8,
                "done": done,
            });
            if !done {
                reply["nextBefore"] = json!(next);
                before = Some(next);
            }
            wire.reply(&request, reply);
        }

        let mut line = String::new();
        loop {
            line.clear();
            if wire.0.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            match request["method"].as_str() {
                Some("session.history") => panic!("a fifth first-view page was read"),
                Some("session.context.usage") => {
                    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method",
                        "data":{"appError":"unknown_method"}}}));
                }
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.methods") => wire.reply(&request, json!({"methods":[]})),
                Some("session.idle") => wire.reply(&request, json!({"sequence":8})),
                Some("session.detach") => {
                    wire.reply(&request, json!({}));
                    return;
                }
                other => panic!("unexpected request {other:?}: {request}"),
            }
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_first_view_reads_at_most_four_latest_turn_pages() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.history.latestTurn"]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));

        let mut before = None;
        for page in 0..4u64 {
            let request = wire.read("session.history");
            assert_eq!(request["params"]["latestTurn"], true);
            assert_eq!(request["params"]["limit"], 256);
            assert_eq!(request["params"]["through"], 8);
            assert!(request["params"].get("after").is_none());
            if let Some(before) = before {
                assert_eq!(request["params"]["before"], before);
            }
            let done = page == 3;
            let next = 6 - page * 2;
            let mut reply = json!({
                "events": [],
                "through": 8,
                "nextAfter": 8,
                "done": done,
            });
            if !done {
                reply["nextBefore"] = json!(next);
                before = Some(next);
            }
            wire.reply(&request, reply);
        }

        let mut line = String::new();
        loop {
            line.clear();
            if wire.0.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            match request["method"].as_str() {
                Some("session.history") => panic!("a fifth first-view page was read"),
                Some("session.context.usage") => {
                    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method",
                        "data":{"appError":"unknown_method"}}}));
                }
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.methods") => wire.reply(&request, json!({"methods":[]})),
                Some("session.idle") => wire.reply(&request, json!({"sequence":8})),
                Some("session.detach") => {
                    wire.reply(&request, json!({}));
                    return;
                }
                other => panic!("unexpected request {other:?}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_first_view_stops_after_one_hundred_messages() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.history.latestTurn"]);
        let attach = wire.read("session.attach");
        let mut attached = attach_record(&attach);
        attached["sequence"] = json!(100);
        wire.reply(&attach, attached);

        let request = wire.read("session.history");
        assert_eq!(request["params"]["latestTurn"], true);
        assert_eq!(request["params"]["through"], 100);
        let events: Vec<Value> = (1..=100)
            .map(|sequence| {
                json!({
                    "id": format!("text-{sequence}"),
                    "sequence": sequence,
                    "create_at": 1,
                    "type": "step",
                    "name": "text",
                    "data": {
                        "turn_id": "turn",
                        "request_id": format!("request-{sequence}"),
                        "delta": format!("message {sequence}")
                    }
                })
            })
            .collect();
        wire.reply(
            &request,
            json!({
                "events": events,
                "through": 100,
                "nextAfter": 100,
                "done": false,
                "nextBefore": 1
            }),
        );

        let mut line = String::new();
        loop {
            line.clear();
            if wire.0.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            match request["method"].as_str() {
                Some("session.history") => panic!("a second page was read after 100 messages"),
                Some("session.context.usage") => {
                    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method",
                        "data":{"appError":"unknown_method"}}}));
                }
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.methods") => wire.reply(&request, json!({"methods":[]})),
                Some("session.idle") => wire.reply(&request, json!({"sequence":100})),
                Some("session.detach") => {
                    wire.reply(&request, json!({}));
                    return;
                }
                other => panic!("unexpected request {other:?}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
fn line_first_view_caps_the_window_at_three_hundred_messages() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["session.history.latestTurn"]);
        let attach = wire.read("session.attach");
        let mut attached = attach_record(&attach);
        attached["sequence"] = json!(301);
        wire.reply(&attach, attached);

        let request = wire.read("session.history");
        assert_eq!(request["params"]["latestTurn"], true);
        assert_eq!(request["params"]["through"], 301);
        let events: Vec<Value> = (1..=301)
            .map(|sequence| {
                json!({
                    "id": format!("text-{sequence}"),
                    "sequence": sequence,
                    "create_at": 1,
                    "type": "step",
                    "name": "text",
                    "data": {
                        "turn_id": "turn",
                        "request_id": format!("request-{sequence}"),
                        "delta": format!("m{sequence:04}")
                    }
                })
            })
            .collect();
        wire.reply(
            &request,
            json!({
                "events": events,
                "through": 301,
                "nextAfter": 301,
                "done": false,
                "nextBefore": 1
            }),
        );

        let mut line = String::new();
        loop {
            line.clear();
            if wire.0.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            match request["method"].as_str() {
                Some("session.history") => panic!("a second page was read after the cap"),
                Some("session.context.usage") => {
                    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method",
                        "data":{"appError":"unknown_method"}}}));
                }
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.methods") => wire.reply(&request, json!({"methods":[]})),
                Some("session.idle") => wire.reply(&request, json!({"sequence":301})),
                Some("session.detach") => {
                    wire.reply(&request, json!({}));
                    return;
                }
                other => panic!("unexpected request {other:?}: {request}"),
            }
        }
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    assert!(!terminal.transcript.contains("m0001"));
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}
