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

fn hold_initial_usage(wire: &mut Wire) -> Value {
    let mut usage = None;
    for _ in 0..2 {
        let request = wire.read_any();
        match request["method"].as_str() {
            Some("account.providers") => wire.reply(&request, json!([])),
            Some("session.context.usage") => usage = Some(request),
            method => panic!("unexpected request {method:?}"),
        }
    }
    usage.expect("initial context request")
}

fn event_on(wire: &mut Wire, branch: &str, sequence: u64, name: &str, data: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":
            {"id":format!("event-{branch}-{sequence}"),"sequence":sequence,"create_at":1,
             "type":"step","name":name,"data":data}}),
    );
}

#[test]
fn tui_display_details_use_one_typed_reply_and_retry_after_collapse() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.history.display", "session.history.detail"]);
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["display"], true);
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history.display");
            assert_eq!(history["params"]["latestTurn"], true);
            assert_eq!(history["params"]["limit"], 256);
            let reading = hold_initial_usage(wire);
            wire.reply(&reading, usage(40000.));
            wire.reply(&history, json!({"items":[{
                "id":"compact-7","sequence":7,"create_at":1,"type":"display","name":"compaction",
                "data":{"strategy":"summary"}
            }],"through":8,"nextAfter":8,"nextBefore":1,"done":true}));
            let submit = loop {
                let request = wire.read_any();
                if request["method"] == "session.context.usage" {
                    wire.reply(&request, usage(40000.));
                } else {
                    assert_eq!(request["method"], "session.submit", "collapsed details cannot read body");
                    break request;
                }
            };
            assert_eq!(submit["params"]["text"], "collapsed fence");
            wire.reply(&submit, json!({"event":{}}));
            let data = json!({"through":6,"status":"completed","retain_from":"compact-7","strategy":"summary",
                "replacement":[{"role":"user","content":"Details arrived: 中文🙂"},{"role":"user","content":"x".repeat(1_100_000)}],
                "replacement_parts":{"strategy":1,"digest":0},
                "diagnostics":{"reason":"auto","trigger":"threshold","estimatedInputTokens":300.0,
                    "entriesReplaced":3,"modelCallCount":1,"durationMs":10,"strategy":{"kind":"summary"}}});
            let first = wire.read("session.history.detail");
            assert_eq!(first["params"], json!({"sessionId":"session","branchId":"main","eventId":"compact-7","sequence":7}));
            let responsive = wire.read("session.submit");
            assert_eq!(responsive["params"]["text"], "detail still pending");
            wire.reply(&responsive, json!({"event":{}}));
            wire.reply(&first, json!({"eventId":"compact-7","sequence":7,"data":data}));
            let retry = wire.read("session.history.detail");
            assert!(retry["params"].get("offset").is_none(), "detail is a single typed read");
            wire.send(json!({"jsonrpc":"2.0","id":retry["id"],"error":{"code":-32000,"message":"Export the Timeline to read this large detail","data":{"appError":"detail_too_large"}}}));
            let cancelled = wire.read("session.history.detail");
            assert!(cancelled["params"].get("offset").is_none());
            let fence = wire.read("session.submit");
            assert_eq!(fence["params"]["text"], "collapsed pending fence");
            wire.reply(&fence, json!({"event":{}}));
            wire.reply(&cancelled, json!({"eventId":"compact-7","sequence":7,"data":data}));
            wire.quiet(Duration::from_millis(100));
            wire.event(9, "text", json!({"turn_id":"turn","request_id":"request","delta":"cancelled details settled"}));
            assert!(wire.trailing_request().is_none());
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("Context compacted");
        terminal.write(b"collapsed fence\r");
        terminal.write(b"/tools\r/tools\r");
        terminal.until("Loading");
        terminal.write(b"detail still pending\r");
        terminal.until("Details arrived");
        terminal.write(b"/tools\r/tools\r/tools\r");
        terminal.until("Details unavailable");
        terminal.write(b"/tools\r/tools\r/tools\r");
        terminal.until("Loading");
        terminal.write(b"/tools\rcollapsed pending fence\r");
        terminal.until("cancelled details settled");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

#[test]
fn tui_displays_history_and_handles_input_while_context_usage_is_pending() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (release, waiting) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            let pending = hold_initial_usage(wire);
            wire.reply(&history, json!({"events":[{
                "id":"old-text","sequence":7,"create_at":1,"type":"step","name":"text",
                "data":{"turn_id":"old","request_id":"old","delta":"restored while usage waits"}
            }],"through":8,"nextAfter":8,"done":true}));
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], "still responsive");
            wire.reply(&submit, json!({"event":{}}));
            wire.event(9, "text", json!({"turn_id":"new","request_id":"new","delta":"live while usage waits"}));
            event_on(wire, "main", 10, "model-output-end", json!({
                "turn_id":"new","request_id":"new","status":"completed","finish_reason":"end_turn"
            }));
            waiting.recv_timeout(PATIENCE).unwrap();
            wire.reply(&pending, usage(40000.));
            let refresh = wire.read("session.context.usage");
            wire.reply(&refresh, usage(64000.));
            assert!(wire.trailing_request().is_none(), "refreshes must coalesce");
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("restored while usage waits");
        terminal.write(b"still responsive\r");
        terminal.until("live while usage waits");
        release.send(()).unwrap();
        terminal.until("64K / 256K");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

#[test]
fn tui_discards_a_previous_sessions_delayed_context_report() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&["session.create.modelFromSession"]);
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            let old = hold_initial_usage(wire);
            let create = wire.read("session.create");
            assert_eq!(create["params"]["modelFromSession"], "session");
            wire.reply(&create, json!({"sessionId":"next","sequence":8,"model":{"providerId":"test","modelId":"model"}}));
            let detach = wire.read("session.detach");
            wire.reply(&detach, json!({}));
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["sessionId"], "next");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["sessionId"], "next");
            wire.reply(&submit, json!({"event":{}}));
            let current = wire.read("session.context.usage");
            assert_eq!(current["params"]["sessionId"], "next");
            wire.reply(&current, usage(64000.));
            wire.reply(&old, usage(128000.));
            wire.notice("session.event", json!({"sessionId":"next","branchId":"main","event":{
                "id":"new-session-text","sequence":9,"create_at":1,"type":"step","name":"text",
                "data":{"turn_id":"next-turn","request_id":"next-request","delta":"next session ready"}
            }}));
            assert!(wire.trailing_request().is_none(), "no stale context refresh");
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("test / model");
        terminal.write(b"/usage\r");
        terminal.write(b"/new\r");
        terminal.until("─ new session ─");
        terminal.write(b"next\r");
        terminal.until("next session ready");
        terminal.until_anywhere("64K / 256K");
        assert!(
            !terminal.transcript.contains("128K / 256K"),
            "{}",
            terminal.transcript
        );
        assert!(
            !terminal.transcript.contains("Context:"),
            "{}",
            terminal.transcript
        );
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

#[test]
fn tui_compaction_reports_committed_failures_once_and_keeps_call_errors() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
            let mut attempt = 0;
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                let request: Value = serde_json::from_str(&line).unwrap();
                line.clear();
                if wire.2 && wire.empty_state(&request) { continue; }
                match request["method"].as_str().unwrap() {
                    "account.providers" => wire.reply(&request, json!([])),
                    "session.context.usage" => wire.reply(&request, usage(1849.0)),
                    "session.detach" => wire.reply(&request, json!({})),
                    "session.context.compact" => {
                        assert_eq!(request["params"]["branchId"], "main");
                        match attempt {
                            0 | 1 => {
                                let error = format!("candidate {} does not reduce input", attempt + 1);
                                let reply = json!({"jsonrpc":"2.0","id":request["id"],"error":{
                                    "code":-32000,"message":"duplicate RPC outcome","data":{"appError":"compaction_failed"}}});
                                if attempt == 0 { wire.send(reply.clone()); }
                                let start = 9 + attempt * 2;
                                for (sequence, name, data) in [
                                    (start, "compaction-start", json!({"through":8,"trigger":"manual","strategy":"fixture"})),
                                    (start + 1, "compaction", json!({"through":8,"start":start,"status":"failed","error":error})),
                                ] {
                                    wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{
                                        "id":format!("compact-{sequence}"),"sequence":sequence,"create_at":1,
                                        "type":"context","name":name,"data":data}}));
                                }
                                if attempt == 1 { wire.send(reply); }
                            }
                            2 => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                                "code":-32602,"message":"compact request rejected","data":{"appError":"invalid_params"}}})),
                            3 => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                                "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                            4 => wire.reply(&request, json!({"status":"unexpected"})),
                            _ => panic!("unexpected compaction request"),
                        }
                        attempt += 1;
                    }
                    method => panic!("unexpected request {method}"),
                }
            }
            assert_eq!(attempt, 5);
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("1.8K / 256K");
        for expected in [
            "candidate 1 does not reduce input",
            "candidate 2 does not reduce input",
            "compact request rejected",
            "This daemon does not support context compaction",
            "Invalid session.context.compact response",
        ] {
            terminal.write(b"/compact\r");
            terminal.until(expected);
        }
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        assert!(
            !terminal.transcript.contains("duplicate RPC outcome"),
            "{}",
            terminal.transcript
        );
    }
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
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("1.8K / 256K");
    shown.send(()).unwrap();
    terminal.write(b"hello\r");
    terminal.until("answered");
    // The border is redrawn as the turn ends, while the answer's text is
    // still revealing.
    terminal.until_anywhere("64K / 256K");
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
    let mut terminal = Terminal::resume_tui(home.command());
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

/// A `model/usage` fact: the request's counters as the provider reported
/// them, with the cache read count only when the request reported one.
fn model_usage(wire: &mut Wire, branch: &str, sequence: u64, prompt: f64, cache_read: Option<f64>) {
    let mut usage =
        json!({"promptTokens":prompt,"completionTokens":1.0,"totalTokens":prompt + 1.0});
    if let Some(cache_read) = cache_read {
        usage["cacheReadTokens"] = json!(cache_read);
    }
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":{
            "id":format!("event-{sequence}"),"sequence":sequence,"create_at":1,
            "type":"model","name":"usage",
            "data":{"turn_id":"turn","request_id":"request","usage":usage}}}),
    );
}

/// A `model/usage` event as a history page carries it: the same fact, in the
/// page's own shape.
fn history_usage(sequence: u64, cache_read: f64) -> Value {
    json!({
        "id":format!("event-{sequence}"),"sequence":sequence,"create_at":1,
        "type":"model","name":"usage",
        "data":{"turn_id":"turn","request_id":"request","usage":{
            "promptTokens":10000.0,"completionTokens":1.0,
            "totalTokens":10001.0,"cacheReadTokens":cache_read}}})
}

/// `/usage` reports the last request's cache read share beside the context
/// reading, follows a newer request's usage, and drops the segment when the
/// newest usage reports no cache field.
#[test]
fn tui_usage_shows_the_last_requests_cache_hit_rate() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
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
            for (sequence, cache_read, delta, input) in [
                (9, Some(9200.0), "first request", 64000.0),
                (11, Some(0.0), "second request", 32000.0),
                (13, None, "third request", 16000.0),
            ] {
                model_usage(wire, "main", sequence, 10000.0, cache_read);
                wire.event(
                    sequence + 1,
                    "text",
                    json!({"turn_id":"turn","request_id":"request","delta":delta}),
                );
                let request = wire.read("session.context.usage");
                wire.reply(&request, usage(input));
            }
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                assert!(
                    !line.contains("session.context.usage"),
                    "extra read: {line}"
                );
                line.clear();
            }
        });
        let mut command = home.command();
        command.arg(if fullscreen {
            "--fullscreen"
        } else {
            "--scrollback"
        });
        let mut terminal = Terminal::resume_tui(command);
        terminal.until("1.8K / 256K");
        shown.send(()).unwrap();
        terminal.until("first request");
        terminal.write(b"/usage\r");
        terminal.until("last request cached 92%");
        terminal.until("second request");
        terminal.write(b"/usage\r");
        terminal.until("last request cached 0%");
        terminal.until("third request");
        terminal.write(b"/usage\r");
        terminal.until("tokens (6%)");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        let last = terminal
            .transcript
            .rfind("Context:")
            .expect("a /usage line");
        assert!(
            !terminal.transcript[last..].contains("last request cached"),
            "{}",
            terminal.transcript
        );
    }
}

/// Every rounding row reaches the report: `100%` for a full hit, one decimal
/// below it, an integer down to `1%`, `<1%` for a positive share under one
/// percent, and no segment when the counters cannot produce one.
#[test]
fn tui_usage_rounds_the_cache_hit_rate_down_and_hides_an_impossible_usage() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
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
                Some("session.context.usage") => wire.reply(&request, usage(1849.0)),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        for (sequence, cache_read, delta, input) in [
            (9, Some(10000.0), "full hit", 64000.0),
            (11, Some(9999.0), "almost full", 32000.0),
            (13, Some(9900.0), "ninety-nine", 40000.0),
            (15, Some(100.0), "one percent", 20000.0),
            (17, Some(1.0), "sliver", 16000.0),
            (19, Some(10001.0), "impossible", 8000.0),
        ] {
            model_usage(wire, "main", sequence, 10000.0, cache_read);
            wire.event(
                sequence + 1,
                "text",
                json!({"turn_id":"turn","request_id":"request","delta":delta}),
            );
            let request = wire.read("session.context.usage");
            wire.reply(&request, usage(input));
        }
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("1.8K / 256K");
    shown.send(()).unwrap();
    terminal.until("full hit");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 100%");
    terminal.until("almost full");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 99.9%");
    terminal.until("ninety-nine");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 99.0%");
    terminal.until("one percent");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 1%");
    terminal.until("sliver");
    terminal.write(b"/usage\r");
    terminal.until("last request cached <1%");
    terminal.until("impossible");
    terminal.write(b"/usage\r");
    terminal.until("tokens (3%)");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let last = terminal
        .transcript
        .rfind("Context:")
        .expect("a /usage line");
    assert!(
        !terminal.transcript[last..].contains("last request cached"),
        "{}",
        terminal.transcript
    );
}

/// A resumed Session shows the share of the last request it recorded before
/// any new request: the attach history folds like the live events do.
#[test]
fn tui_usage_shows_the_cache_hit_rate_a_resumed_session_recorded() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        assert_eq!(history["params"]["branchId"], "main");
        wire.reply(
            &history,
            json!({"events":[{
                "id":"event-5","sequence":5,"create_at":1,
                "type":"model","name":"usage",
                "data":{"turn_id":"turn","request_id":"request","usage":{
                    "promptTokens":10000.0,"completionTokens":1.0,
                    "totalTokens":10001.0,"cacheReadTokens":9200.0}}}],
            "through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.reply(&request, usage(64000.0)),
                method => panic!("unexpected request {method:?}"),
            }
        }
        let request = wire.read("session.context.usage");
        wire.reply(&request, usage(64000.0));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("64K / 256K");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 92%");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A `model/usage` on another branch never touches the figure `main` shows.
#[test]
fn tui_usage_cache_hit_rate_ignores_events_off_main() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
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
                Some("session.context.usage") => wire.reply(&request, usage(64000.0)),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        model_usage(wire, "main", 9, 10000.0, Some(9200.0));
        wire.event(
            10,
            "text",
            json!({"turn_id":"turn","request_id":"request","delta":"main request"}),
        );
        let request = wire.read("session.context.usage");
        wire.reply(&request, usage(32000.0));
        model_usage(wire, "child", 9, 10000.0, Some(10000.0));
        let child = wire.read("session.turn.get");
        assert_eq!(child["params"]["branchId"], "child");
        wire.reply(
            &child,
            json!({"branchId":"child","through":8,"turn":null,"output":null}),
        );
        wire.event(
            12,
            "text",
            json!({"turn_id":"turn","request_id":"request","delta":"child request"}),
        );
        let request = wire.read("session.context.usage");
        wire.reply(&request, usage(16000.0));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("64K / 256K");
    shown.send(()).unwrap();
    terminal.until("main request");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 92%");
    terminal.until("child request");
    terminal.write(b"/usage\r");
    terminal.until("tokens (6%)");
    assert!(
        !terminal.transcript.contains("last request cached 100%"),
        "{}",
        terminal.transcript
    );
    let last = terminal
        .transcript
        .rfind("Context:")
        .expect("a /usage line");
    assert!(
        terminal.transcript[last..].contains("last request cached 92%"),
        "{}",
        terminal.transcript
    );
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// `/usage` leaves the segment out while `main`'s history is still paging, so
/// it never reports a page's older request; the second page's newer usage is
/// what the next read shows.
#[test]
fn tui_usage_withholds_the_cache_hit_rate_until_main_history_loads() {
    let home = Home::new();
    let (read, wait_read) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let first = wire.read("session.history");
        assert_eq!(
            first["params"],
            json!({"sessionId":"session","branchId":"main","latestTurn":true,"limit":256})
        );
        wire.reply(
            &first,
            json!({"events":[history_usage(7, 9200.0),
                {"id":"event-8","sequence":8,"create_at":1,"type":"step","name":"text",
                 "data":{"turn_id":"turn","request_id":"request","delta":"history loaded"}}],
                "through":8,"nextBefore":7,"nextAfter":8,"done":false}),
        );
        let mut second = None;
        let mut started = 0;
        while second.is_none() || started < 2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => {
                    wire.reply(&request, json!([]));
                    started += 1;
                }
                Some("session.context.usage") => {
                    wire.reply(&request, usage(1849.0));
                    started += 1;
                }
                Some("session.history") => {
                    assert!(second.is_none(), "one history page at a time");
                    second = Some(request);
                }
                method => panic!("unexpected request {method:?}"),
            }
        }
        let second = second.expect("main history pages");
        assert_eq!(
            second["params"],
            json!({"sessionId":"session","branchId":"main","before":7,"limit":256,"through":8})
        );
        // The first `/usage` reads the context while the second page is
        // outstanding, and only then does the page land.
        let pending = wire.read("session.context.usage");
        wire.reply(&pending, usage(64000.0));
        wait_read.recv_timeout(PATIENCE).unwrap();
        wire.reply(
            &second,
            json!({"events":[history_usage(5, 5000.0)],"through":8,"nextBefore":1,"nextAfter":6,"done":true}),
        );
        let loaded = wire.read("session.context.usage");
        wire.reply(&loaded, usage(64000.0));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("1.8K / 256K");
    terminal.write(b"/usage\r");
    terminal.until("tokens (25%)");
    read.send(()).unwrap();
    terminal.until("history loaded");
    terminal.write(b"/usage\r");
    terminal.until("last request cached 92%");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let first = terminal.transcript.find("Context:").expect("a /usage line");
    let second = terminal
        .transcript
        .rfind("Context:")
        .expect("the second /usage line");
    assert_ne!(first, second, "{}", terminal.transcript);
    assert!(
        !terminal.transcript[first..second].contains("last request cached"),
        "{}",
        terminal.transcript
    );
    assert!(
        !terminal.transcript.contains("last request cached 50%"),
        "{}",
        terminal.transcript
    );
}

/// A finished model request on `main` reads the context again while the run
/// continues, once per request; a line's request does not read it; the run's
/// end still reads it; and a replayed history page that carries finished
/// requests spends no read of its own (§4).
#[test]
fn tui_context_reading_follows_each_finished_model_request_on_main() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let (first_rendered, wait_first_rendered) = mpsc::channel();
    let (ended, wait_ended) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[
                {"sequence":4,"create_at":1,"type":"step","name":"model-output-end",
                 "data":{"turn_id":"old-turn","request_id":"old-request",
                     "status":"completed","finish_reason":"end_turn"}}
            ],"through":8,"nextAfter":8,"done":true}),
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
        let submit = wire.read_any();
        assert_eq!(
            submit["method"], "session.submit",
            "a replayed history page read the context"
        );
        assert_eq!(submit["params"]["text"], "hello");
        wire.reply(&submit, json!({"event":{}}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":true}),
        );
        event_on(
            wire,
            "main",
            9,
            "model-output-end",
            json!({"turn_id":"turn","request_id":"request-1",
                "status":"completed","finish_reason":"tool_use"}),
        );
        let first = wire.read("session.context.usage");
        assert_eq!(
            first["params"],
            json!({"sessionId":"session","branchId":"main"})
        );
        wire.reply(&first, usage(40000.0));
        // The next request end follows only once the border drew the first
        // reading: the interface drains both updates into one frame
        // otherwise.
        wait_first_rendered.recv_timeout(PATIENCE).unwrap();
        event_on(
            wire,
            "main",
            12,
            "model-output-end",
            json!({"turn_id":"turn","request_id":"request-2",
                "status":"completed","finish_reason":"tool_use"}),
        );
        let second = wire.read("session.context.usage");
        wire.reply(&second, usage(120000.0));
        // A line's finished request is the line's context, not main's.
        event_on(
            wire,
            "line",
            13,
            "model-output-end",
            json!({"turn_id":"line-turn","request_id":"line-request",
                "status":"completed","finish_reason":"end_turn"}),
        );
        let line_state = wire.read("session.turn.get");
        assert_eq!(line_state["params"]["branchId"], "line");
        wire.reply(
            &line_state,
            json!({"branchId":"line","through":13,"turn":null,"output":null}),
        );
        for method in [
            "session.input.pending",
            "session.exec.pending",
            "session.turn.get",
        ] {
            let request = wire.read(method);
            assert!(wire.empty_state(&request));
        }
        wire.quiet(Duration::from_millis(300));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"fixture"}));
        wire.notice(
            "session.state",
            json!({"sessionId":"session","branchId":"main","running":false}),
        );
        let last = wire.read("session.context.usage");
        wire.reply(&last, usage(121000.0));
        ended.send(()).unwrap();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::resume_tui(home.command());
    terminal.until("1.8K / 256K");
    shown.send(()).unwrap();
    terminal.write(b"hello\r");
    terminal.until("40K / 256K");
    first_rendered.send(()).unwrap();
    terminal.until("120K / 256K");
    wait_ended.recv_timeout(PATIENCE).unwrap();
    terminal.until("121K / 256K");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}
