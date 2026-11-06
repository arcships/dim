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
