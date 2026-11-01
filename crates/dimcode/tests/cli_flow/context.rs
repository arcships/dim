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

fn event_on(wire: &mut Wire, branch: &str, sequence: u64, name: &str, data: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":
            {"id":format!("event-{branch}-{sequence}"),"sequence":sequence,"create_at":1,
             "type":"step","name":name,"data":data}}),
    );
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
        let mut terminal = Terminal::launch_tui(command);
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
    let mut terminal = Terminal::launch_tui(home.command());
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
        wire.created();
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
    let mut terminal = Terminal::launch_tui(home.command());
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
        // A full hit on a task branch is folded after its own history page,
        // before the next main-branch event renders.
        model_usage(wire, "child", 9, 10000.0, Some(10000.0));
        let child = wire.read("session.history");
        assert_eq!(child["params"]["branchId"], "child");
        assert_eq!(child["params"]["after"], 0);
        wire.reply(
            &child,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
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
    let mut terminal = Terminal::launch_tui(home.command());
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
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let first = wire.read("session.history");
        assert_eq!(
            first["params"],
            json!({"sessionId":"session","branchId":"main","after":0,"limit":256})
        );
        wire.reply(
            &first,
            json!({"events":[history_usage(5, 5000.0)],"through":8,"nextAfter":6,"done":false}),
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
            json!({"sessionId":"session","branchId":"main","after":6,"limit":256,"through":8})
        );
        // The first `/usage` reads the context while the second page is
        // outstanding, and only then does the page land.
        let pending = wire.read("session.context.usage");
        wire.reply(&pending, usage(64000.0));
        wait_read.recv_timeout(PATIENCE).unwrap();
        wire.reply(
            &second,
            json!({"events":[
                history_usage(7, 9200.0),
                {"id":"event-8","sequence":8,"create_at":1,"type":"step","name":"text",
                 "data":{"turn_id":"turn","request_id":"request","delta":"history loaded"}}],
                "through":8,"nextAfter":8,"done":true}),
        );
        let loaded = wire.read("session.context.usage");
        wire.reply(&loaded, usage(64000.0));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
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
        wire.created();
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
        let line_history = wire.read("session.history");
        assert_eq!(line_history["params"]["branchId"], "line");
        wire.reply(
            &line_history,
            json!({"events":[],"through":13,"nextAfter":13,"done":true}),
        );
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
    let mut terminal = Terminal::launch_tui(home.command());
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

#[test]
fn tui_usage_includes_account_quota_only_for_dim_oauth_models() {
    for fullscreen in [false, true] {
        for scenario in ["quota", "unlimited", "account_error", "other_provider"] {
            let provider = if scenario == "other_provider" {
                "test"
            } else {
                "dimcode-api-oauth"
            };
            let home = Home::new();
            let daemon = home.daemon(move |wire| {
                wire.hello(&[]);
                let create = wire.read("session.create");
                wire.reply(&create, json!({
                    "sessionId":"session", "sequence":8,
                    "model":{"providerId":provider,"modelId":"model"}
                }));
                let attach = wire.read("session.attach");
                let mut record = attach_record(&attach);
                record["model"]["providerId"] = json!(provider);
                wire.reply(&attach, record);
                let history = wire.read("session.history");
                wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
                let mut line = String::new();
                while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    line.clear();
                    match request["method"].as_str().unwrap() {
                        "account.providers" => wire.reply(&request, json!([{"providerId":"dimcode-api-oauth"}])),
                        "account.status" => wire.reply(&request, json!({
                            "account":{"issuer":"fixture","subject":"signed-in","grantedScopes":[]}
                        })),
                        "session.context.usage" => {
                            if scenario == "unlimited" {
                                failed(wire, &request);
                            } else {
                                wire.reply(&request, usage(1849.0));
                            }
                        }
                        "account.usage" => {
                            assert_eq!(provider, "dimcode-api-oauth");
                            assert_eq!(request["params"], json!({"providerId":"dimcode-api-oauth"}));
                            match scenario {
                                "quota" => wire.reply(&request, json!({
                                    "plan":{"name":"\u{1b}]2;injected-title\u{7}Pro","status":"active","expiresAt":"2026-11-30T00:00:00Z"},
                                    "credits":{"usedUnits":25000,"totalUnits":300000,"resetsAt":"2026-11-30T00:00:00Z"},
                                    "windows":[{"durationHours":5,"tokenUsed":9500,"tokenCap":10000}],
                                    "features":[{"key":"web_\u{1b}[2Jsearch","used":211,"allowance":1500}]
                                })),
                                "unlimited" => wire.reply(&request, json!({"credits":{"unlimited":true}})),
                                "account_error" => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                                    "code":-32000,"message":"Account usage temporarily \u{1b}]2;injected-title\u{7}unavailable"
                                }})),
                                _ => panic!("unexpected account usage for {scenario}"),
                            }
                        }
                        method => panic!("unexpected request {method}"),
                    }
                }
            });
            let mut command = home.command();
            command.arg(if fullscreen {
                "--fullscreen"
            } else {
                "--scrollback"
            });
            let mut terminal = Terminal::launch_tui(command);
            terminal.until(&format!("{provider} / model"));
            terminal.write(b"/usage\r");
            terminal.until(if scenario == "unlimited" {
                "This daemon does not track context usage"
            } else {
                "Context: 1,849 / 256,000 tokens"
            });
            if scenario == "quota" {
                for expected in [
                    "DimAgent OAuth",
                    "Pro · active · expires 2026-11-30",
                    "25,000 / 300,000  8%",
                    "9,500 / 10,000  95%",
                    "211 / 1,500  14%",
                ] {
                    terminal.until(expected);
                }
            } else if scenario == "unlimited" {
                terminal.until("DimAgent OAuth");
                terminal.until("unlimited");
            } else if scenario == "account_error" {
                terminal.until("Account usage temporarily unavailable");
            }
            terminal.write(b"/exit\r");
            terminal.until("Bye!");
            assert!(terminal.wait().success());
            daemon.join().unwrap();
            assert!(
                !terminal.transcript.contains("injected-title"),
                "{scenario}"
            );
            if scenario == "unlimited" {
                assert!(!terminal.transcript.contains("window"));
                assert!(!terminal.transcript.contains("Web search"));
            }
            if provider == "test" {
                assert!(!terminal.transcript.contains("DimAgent OAuth"));
                assert!(!terminal.transcript.contains("Credits"));
            }
        }
    }
}
