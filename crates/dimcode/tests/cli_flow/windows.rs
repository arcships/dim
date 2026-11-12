use super::*;

fn text(sequence: u64, request: &str, delta: &str) -> Value {
    json!({"id":format!("e{sequence}"),"sequence":sequence,"create_at":1,
        "type":"step","name":"text","data":{"turn_id":"turn","request_id":request,"delta":delta}})
}

#[test]
fn line_effort_uses_attach_and_new_configuration_even_outside_the_transcript() {
    for concurrent in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["sequence"] = json!(if concurrent { 8500 } else { 10000 });
            record["model"]["providerId"] = json!("adopted");
            wire.reply(&attach, record);
            for page in 0..4 {
                let request = wire.read("session.history");
                let events = if page == 0 && !concurrent {
                    vec![configured_event(9999, "historical", json!({}))]
                } else {
                    Vec::new()
                };
                if page == 0 && concurrent {
                    let mut event = configured_event(8800, "updated", json!({}));
                    event["data"]["provider_id"] = json!("updated");
                    wire.notice(
                        "session.event",
                        json!({"sessionId":"session","branchId":"main","event":event}),
                    );
                }
                wire.reply(
                    &request,
                    json!({"events":events,"through":10000,
                    "nextBefore":10001-(page+1)*256,"nextAfter":10000-page*256,"done":true}),
                );
            }
            wire.no_pending_questions(10000);
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":10000}));
            let catch_up = wire.read("session.history");
            assert_eq!(catch_up["params"]["after"], 10000);
            wire.reply(
                &catch_up,
                json!({"events":[],"through":10000,"nextAfter":10000,"done":true}),
            );
            let catalog = wire.read("provider.catalog");
            assert_eq!(
                catalog["params"]["providerId"],
                if concurrent { "updated" } else { "adopted" }
            );
            wire.reply(&catalog, json!({"catalog":null}));
            wire.detach();
        });
        let mut command = home.command();
        command.args(["--resume", "session"]);
        let mut terminal = Terminal::launch(command);
        terminal.editor();
        terminal.write(b"/effort\r");
        terminal.until("Model preference catalog is unavailable");
        terminal.editor();
        terminal.write(b"/exit\r");
        assert!(terminal.wait().success(), "{}", terminal.transcript);
        daemon.join().unwrap();
    }
}

#[test]
fn line_windows_bound_sparse_history_and_apply_held_live_events_after_the_window() {
    for (head, display) in [
        (5_000_u64, false),
        (50_000, false),
        (5_000, true),
        (50_000, true),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(if display { &["session.history.display", "session.history.detail"] } else { &[] });
            let method = if display { "session.history.display" } else { "session.history" };
            let field = if display { "items" } else { "events" };
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"].get("display"), display.then_some(&json!(true)));
            wire.reply(&attach, attach_record(&attach));
            for page in 0..4 {
                let request = wire.read(method);
                let before = head + 1 - page * 256;
                let first = before - 256;
                let mut expected = json!({"sessionId":"session","branchId":"main","limit":256});
                if page == 0 {
                    expected["latestTurn"] = json!(true);
                } else {
                    expected["before"] = json!(before);
                    expected["through"] = json!(head);
                }
                assert_eq!(request["params"], expected);
                let mut events = match page {
                    0 => vec![text(head, "latest", "newest-window-text")],
                    1 => Vec::new(),
                    2 => (first..before).map(|sequence| json!({"id":format!("hidden{sequence}"),
                        "sequence":sequence,"create_at":1,"type":"input","name":"message","data":{"blocks":[]}})).collect(),
                    _ => vec![text(first, "older", "oldest-window-text")],
                };
                if display && page == 0 {
                    events.insert(0, json!({"id":"compact","sequence":head-1,"create_at":1,"type":"display","name":"compaction","data":{"strategy":"summary"}}));
                }
                if page == 1 {
                    wire.notice("session.event", json!({"sessionId":"session","branchId":"main",
                        "event":text(head + 1,"live","held-live-text")}));
                    if display {
                        wire.notice("session.display", json!({"sessionId":"session","branchId":"main","item":{"id":"live-compact","sequence":head+2,"create_at":1,"type":"display","name":"compaction","data":{"strategy":"summary"}}}));
                    }
                }
                wire.reply(&request, json!({(field):events,"through":head,
                    "nextBefore":first,"nextAfter":before-1,"done":true}));
            }
            wire.no_pending_questions(head + 1 + u64::from(display));
            let idle = wire.read("session.idle");
            wire.reply(&idle,json!({"sequence":head+1+u64::from(display)}));
            wire.detach();
        });
        let mut command = home.command();
        command.args(["--resume", "session"]);
        let mut terminal = Terminal::launch(command);
        terminal.until("held-live-text");
        terminal.editor();
        terminal.write(b"/exit\r");
        assert!(terminal.wait().success(), "{}", terminal.transcript);
        daemon.join().unwrap();
        let output = &terminal.transcript;
        assert!(output.contains("earlier content not loaded"), "{output}");
        assert!(
            output.find("oldest-window-text") < output.find("newest-window-text"),
            "{output}"
        );
        assert!(
            output.find("newest-window-text") < output.find("held-live-text"),
            "{output}"
        );
    }
}

#[test]
fn line_window_keeps_one_large_atomic_batch_whole() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let request = wire.read("session.history");
        assert_eq!(request["params"]["latestTurn"],true);
        let events: Vec<_> = (1..=400).map(|sequence| json!({
            "id":format!("input{sequence}"),"sequence":sequence,"create_at":1,"type":"input","name":"message",
            "data":{"user":{"content":[{"type":"text","text":format!("row-{sequence:03}-body")}]}}
        })).collect();
        wire.reply(&request,json!({"events":events,"through":400,"nextBefore":1,"nextAfter":400,"done":true}));
        wire.no_pending_questions(400);
        let idle = wire.read("session.idle");
        wire.reply(&idle,json!({"sequence":400}));
        wire.detach();
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    terminal.until("row-400-body");
    terminal.editor();
    terminal.write(b"/exit\r");
    assert!(terminal.wait().success(), "{}", terminal.transcript);
    daemon.join().unwrap();
    for row in 1..=400 {
        assert!(
            terminal.transcript.contains(&format!("row-{row:03}-body")),
            "the atomic batch lost row {row}: {}",
            terminal.transcript
        );
    }
}

#[test]
fn an_orphan_tool_result_reads_its_arguments_by_identity_without_loading_more_history() {
    let home = Home::new();
    fs::write(
        home.0.join("config.json"),
        json!({"locale":"en","tuiToolDetails":"expanded"}).to_string(),
    )
    .unwrap();
    let daemon=home.daemon(move |wire| {
        wire.hello(&["session.methods"]);
        let attach=wire.read("session.attach");wire.reply(&attach,attach_record(&attach));
        let methods=wire.read("session.methods");wire.reply(&methods,json!({"methods":[
            {"name":"session.input.pending","mutating":false},{"name":"session.exec.pending","mutating":false},
            {"name":"session.turn.get","mutating":false},{"name":"session.tool.get","mutating":false}
        ]}));
        wire.2=true;
        let history=wire.read("session.history");assert_eq!(history["params"]["latestTurn"],true);
        let events:Vec<_>=(0..100).map(|i|json!({"id":format!("result-{i}"),"sequence":100+i,"create_at":1000+i,"type":"step","name":"tool-result","data":{
            "turn_id":"turn","request_id":"request","results":[{"call_id":format!("call-{i}"),"result":{"content":format!("result {i}")}}]
        }})).collect();
        wire.reply(&history,json!({"events":events,"through":199,"nextAfter":199,"nextBefore":100,"done":true}));
        wire.no_context();
        let mut providers=false;
        let mut target=false;
        while !providers || !target {
            let request=wire.read_any();
            match request["method"].as_str() {
                Some("account.providers")=>{ providers=true;wire.reply(&request,json!([])); }
                Some("session.tool.get")=>{
                    let target_call=request["params"]["callId"]=="call-0";
                    let call=if target_call {
                        target=true;
                        assert_eq!(request["params"],json!({"sessionId":"session","branchId":"main","requestId":"request","callId":"call-0"}));
                        json!({"turn_id":"turn","request_id":"request","call":{"id":"call-0","name":"echo","input":{"text":"point-read origin arguments"}}})
                    } else { Value::Null };
                    wire.reply(&request,json!({"branchId":"main","through":199,"call":call}));
                }
                method=>panic!("unexpected request after a full window: {method:?}"),
            }
        }
        wire.hold();
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("point-read origin arguments");
    terminal.until("Earlier content not loaded");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
}
