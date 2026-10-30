use super::*;

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn native_tui_stop_all_settles_the_member_and_reports_cancellation_to_the_leader() {
    fn rpc(stream: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
        writeln!(
            stream.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        loop {
            let mut line = String::new();
            assert!(stream.read_line(&mut line).unwrap() > 0);
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == 1 {
                assert!(reply.get("error").is_none(), "{method}: {reply}");
                return reply["result"].clone();
            }
        }
    }

    fn history(stream: &mut BufReader<UnixStream>, session: &str) -> Vec<Value> {
        let page = rpc(
            stream,
            "session.history",
            json!({"sessionId":session,"branchId":"main","after":0,"limit":256}),
        );
        assert_eq!(
            page["done"], true,
            "the fixture must fit in one public history page"
        );
        serde_json::from_value(page["events"].clone()).unwrap()
    }

    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let _daemon = OwnedDaemon(socket.clone());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
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
    let stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stream = BufReader::new(stream);
    rpc(
        &mut stream,
        "hello",
        json!({"protocolVersion":3,"clientKind":"cli"}),
    );
    let agents = rpc(&mut stream, "agent.list", json!({}));
    let holder = agents[0]["agentId"].as_str().unwrap().to_owned();
    let sessions = rpc(&mut stream, "session.summaries", json!({}));
    let leader = sessions[0]["sessionId"].as_str().unwrap().to_owned();
    let model = thread::spawn(move || {
        for phase in 0..4 {
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
            let messages = request["messages"].as_array().unwrap();
            let continuing = messages
                .iter()
                .any(|message| message["role"] == "assistant");
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"stop-fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = if continuing {
                frame(
                    json!({"content":"native delegation settled for this turn"}),
                    json!("stop"),
                )
            } else {
                let (name, input) = if messages.iter().any(|message| {
                    message["content"]
                        .to_string()
                        .contains("delegate-native-task")
                }) {
                    (
                        "agent",
                        json!({"action":"request","agentId":holder,"prompt":"native-member-command"}),
                    )
                } else {
                    assert!(
                        messages.iter().any(|message| message["content"]
                            .to_string()
                            .contains("native-member-command")),
                        "{request}"
                    );
                    (
                        "exec",
                        json!({"command":"printf started > foreground-started; sleep 60","runInBackground":true,"timeoutMs":-1}),
                    )
                };
                frame(
                    json!({"tool_calls":[{"index":0,"id":format!("native-{name}"),"type":"function","function":{"name":name,"arguments":input.to_string()}}]}),
                    Value::Null,
                ) + &frame(json!({}), json!("tool_calls"))
            } + "data: [DONE]\n\n";
            write!(stream.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    terminal.write(b"delegate-native-task\r");
    terminal.until("native delegation settled for this turn");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !home.0.join("foreground-started").exists() {
        assert!(
            Instant::now() < deadline,
            "the real member command did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let before = history(&mut stream, &leader);
    let operation = before
        .iter()
        .find(|event| event["type"] == "collaboration" && event["name"] == "request")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let member = before
        .iter()
        .find(|event| {
            event["type"] == "collaboration"
                && event["name"] == "delivered"
                && event["data"]["op_event_id"] == operation
        })
        .unwrap()["data"]["session_id"]
        .as_str()
        .unwrap();
    let leader_turns = before
        .iter()
        .filter(|event| event["type"] == "turn" && event["name"] == "start")
        .count();
    assert!(
        !history(&mut stream, member)
            .iter()
            .any(|event| event["type"] == "exec")
    );

    terminal.write(b"/tasks\r");
    terminal.until("Background tasks");
    terminal.until("native-member-command");
    terminal.write(b"\x18");
    terminal.until("attempted 1,");
    terminal.until("unavailable 0.");
    terminal.until("Cancelled");
    rpc(
        &mut stream,
        "session.idle",
        json!({"sessionId":member,"branchId":"main"}),
    );
    let stopped = history(&mut stream, member);
    assert!(stopped.iter().any(|event| {
        event["type"] == "step"
            && event["name"] == "tool-result"
            && event["data"]["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|result| {
                    result["call_id"] == "native-exec"
                        && result["result"]["isError"] == true
                        && result["result"]["content"]
                            .as_str()
                            .unwrap()
                            .to_lowercase()
                            .contains("cancel")
                })
    }));
    assert!(stopped.iter().any(|event| event["type"] == "turn"
        && event["name"] == "end"
        && event["data"]["status"] == "cancelled"));
    assert!(!stopped.iter().any(|event| event["type"] == "exec"));
    model.join().unwrap();
    rpc(
        &mut stream,
        "session.idle",
        json!({"sessionId":leader,"branchId":"main"}),
    );
    let reported = history(&mut stream, &leader);
    let results: Vec<_> = reported
        .iter()
        .filter(|event| {
            event["type"] == "collaboration"
                && event["name"] == "result"
                && event["data"]["request_event_id"] == operation
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "one real terminal result closes the delegation"
    );
    assert_eq!(results[0]["data"]["status"], "cancelled");
    assert!(reported.iter().any(|event| {
        event["type"] == "input"
            && event["name"] == "message"
            && event["data"]["mode"] == "steer"
            && event["data"]["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| {
                    block["text"].as_str().unwrap().contains(&format!(
                        "resultId \"{}\"",
                        results[0]["id"].as_str().unwrap()
                    ))
                })
    }));
    assert!(
        reported
            .iter()
            .filter(|event| event["type"] == "turn" && event["name"] == "start")
            .count()
            > leader_turns,
        "the cancellation report remains able to start a new leader turn after stop-all"
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

fn fact(id: &str, sequence: u64, at: u64, kind: &str, name: &str, data: Value) -> Value {
    json!({"id":id,"sequence":sequence,"create_at":at,"type":kind,"name":name,"data":data})
}

fn command(id: &str, sequence: u64, at: u64, label: &str) -> Value {
    fact(
        id,
        sequence,
        at,
        "exec",
        "started",
        json!({
            "turn_id":"turn","request_id":"request","call_id":id,"task_id":id,
            "description":label,"output_path":"/server/private/output",
        }),
    )
}

fn delegation(id: &str, sequence: u64, label: &str) -> Value {
    fact(
        id,
        sequence,
        sequence * 1000,
        "collaboration",
        "request",
        json!({
            "turn_id":"turn","request_id":"same-model-request","call_id":"same-model-call",
            "target":{"kind":"agent","agent_id":"reviewer"},"prompt":label,
        }),
    )
}

fn delivery(id: &str, sequence: u64, request: &str) -> Value {
    fact(
        id,
        sequence,
        sequence * 1000,
        "collaboration",
        "delivered",
        json!({
            "op_event_id":request,"status":"ok","agent_id":"reviewer","session_id":"member",
        }),
    )
}

/// The delegation's terminal fact: its card no longer keeps the member observed.
fn ended(id: &str, sequence: u64, request: &str) -> Value {
    fact(
        id,
        sequence,
        sequence * 1000,
        "collaboration",
        "result",
        json!({
            "request_event_id":request,"agent_id":"reviewer","session_id":"member",
            "status":"completed","text":"ended before viewing",
        }),
    )
}

fn state(wire: &mut Wire, branch: &str) {
    wire.notice(
        "session.state",
        json!({"sessionId":"session","branchId":branch,"running":false}),
    );
}

fn event(wire: &mut Wire, branch: &str, event: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":"session","branchId":branch,"event":event}),
    );
}

#[test]
fn tui_task_history_resume_windows_complete_main_pages_and_preserves_the_draft() {
    for resume_on_launch in [true, false] {
        let home = Home::new();
        let (ready, readiness) = std::sync::mpsc::channel();
        let (proceed, control) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            if !resume_on_launch {
                wire.created();
                let attach = wire.read("session.attach");
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                wire.reply(&history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
                wire.no_context();
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
            }
            if !resume_on_launch {
                let summaries = wire.read("session.summaries");
                wire.reply(&summaries, json!([{"sessionId":"paged","cwd":"/workspace",
                    "preview":"Paged history","model":{"providerId":"test","modelId":"restored-model"},"updatedAt":1}]));
            }
            if !resume_on_launch {
                let detach = wire.read("session.detach");
                assert_eq!(detach["params"], json!({"sessionId":"session"}));
                wire.reply(&detach, json!({}));
            }
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["sessionId"], "paged");
            wire.reply(&attach, json!({"sessionId":"paged","cwd":"/workspace",
                "model":{"providerId":"test","modelId":"restored-model"},"createdAt":1,
                "running":false,"sequence":0,"attachedClients":1}));
            let first = wire.read("session.history");
            wire.no_context();
            if resume_on_launch {
                let providers = wire.read("account.providers");
                wire.reply(&providers, json!([]));
            }
            ready.send(()).unwrap();
            control.recv_timeout(Duration::from_secs(10)).unwrap();
            let mut request = first;
            for page in 0..2 {
                let after = page * 75;
                assert_eq!(request["params"]["after"], after);
                let events: Vec<_> = (after + 1..=after + 75).map(|index| {
                    fact(&format!("message-{index}"), index, index, "input", "message",
                        json!({"mode":"append","user":{"content":[{"type":"text","text":format!("paged-message-{index:03}")}]}}))
                }).collect();
                wire.reply(&request, json!({"events":events,"through":150,"nextAfter":after+75,"done":page==1}));
                if page == 0 {
                    request = wire.read("session.history");
                    assert_eq!(request["params"]["through"], 150);
                }
            }
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["sessionId"], "paged");
            assert_eq!(submit["params"]["text"], "draft during history");
            wire.reply(&submit, json!({"event":{}}));
            let mut line = String::new();
            assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "earlier history is already loaded: {line}");
        });
        let mut command = home.command();
        if resume_on_launch {
            command.args(["--resume", "paged"]);
        }
        let mut terminal = Terminal::launch_tui(command);
        if !resume_on_launch {
            terminal.until("test / model");
            terminal.write(b"/resume\r");
            terminal.until("Paged history");
            terminal.write(b"paged\r");
        }
        readiness.recv_timeout(Duration::from_secs(10)).unwrap();
        terminal.until("test / restored-model");
        terminal.write(b"draft during history");
        terminal.until("draft during history");
        proceed.send(()).unwrap();
        terminal.until("paged-message-150");
        terminal.until("resumed paged");
        terminal.until("100 messages");
        terminal.until("earlier history: /");
        // The window title names the session's first message; only the
        // screen is windowed.
        let screen = terminal
            .transcript
            .replace("\x1b]0;DIM | paged-message-001\x07", "");
        assert!(screen.contains("paged-message-051"));
        assert!(!screen.contains("paged-message-050"));
        assert!(!screen.contains("paged-message-001"));
        assert!(!terminal.transcript.contains("· 0 messages"));
        // A `/timeline` jump to the first message draws the earlier history.
        terminal.write(b"\r/timeline\r");
        terminal.until("#150");
        terminal.write(b"\x1b[H\r");
        terminal.until("paged-message-001");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_task_inventory_restores_branch_pages_and_retries_only_on_reopen() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let (proceed, control) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let main = wire.read("session.history");
        assert_eq!(main["params"]["branchId"], "main");
        wire.reply(&main, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        state(wire, "child");
        let child = wire.read("session.history");
        assert_eq!(child["params"]["branchId"], "child");
        assert_eq!(child["params"]["after"], 0);
        ready.send(()).unwrap();
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        state(wire, "nested");
        state(wire, "child");
        event(wire, "child", fact("child-end", 5, 9000, "exec", "ended", json!({
            "task_id":"child-command","status":"failed","result":{"content":"real child terminal","structuredContent":{"exitCode":7}},
        })));
        wire.quiet(Duration::from_millis(150));
        wire.reply(&child, json!({"events":[command("child-command",1,1000,"restored child command")],"through":4,"nextAfter":2,"done":false}));
        for _ in 0..2 {
            let read = wire.read("session.history");
            match read["params"]["branchId"].as_str().unwrap() {
                "nested" => wire.reply(&read, json!({"events":[command("nested-command",1,2000,"restored nested command")],"through":1,"nextAfter":1,"done":true})),
                "child" => {
                    assert_eq!(read["params"]["after"], 2);
                    assert_eq!(read["params"]["through"], 4);
                    wire.reply(&read, json!({"events":[
                        command("partial-page",3,3000,"must not adopt a partial page"),
                        command("unordered",2,4000,"unordered event rejected"),
                    ],"through":4,"nextAfter":4,"done":true}));
                }
                other => panic!("unexpected branch {other}"),
            }
        }
        state(wire, "child");
        state(wire, "nested");
        wire.quiet(Duration::from_millis(200));
        event(wire, "child", command("failed-period-command",6,10000,"created while history failed"));
        ready.send(()).unwrap();
        let retry = wire.read("session.history");
        assert_eq!(retry["params"], json!({"sessionId":"session","branchId":"child","after":2,"through":4,"limit":256}));
        ready.send(()).unwrap();
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.quiet(Duration::from_millis(150));
        wire.reply(&retry, json!({"events":[],"through":4,"nextAfter":4,"done":true}));
        let catch_up = wire.read("session.history");
        assert_eq!(catch_up["params"], json!({"sessionId":"session","branchId":"child","after":4,"limit":256}));
        wire.reply(&catch_up, json!({"events":[
            fact("child-end",5,9000,"exec","ended",json!({"task_id":"child-command","status":"failed","result":{"content":"real child terminal","structuredContent":{"exitCode":7}}})),
            command("failed-period-command",6,10000,"created while history failed"),
        ],"through":6,"nextAfter":6,"done":true}));
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.quiet(Duration::from_millis(200));
        ready.send(()).unwrap();
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected request after finished detail: {line}");
    });
    let mut launch = home.command();
    launch.args(["--resume", "session"]);
    let mut terminal = Terminal::launch_tui(launch);
    terminal.until("test / model");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"/tasks\r");
    terminal.until("Loading branch history");
    terminal.write(b"\x18");
    terminal.until("Branch history is incomplete");
    proceed.send(()).unwrap();
    terminal.until("Branch history failed; reopen to retry");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"\x1b[27u/tasks\r");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"\x1b[27u/tasks\r");
    proceed.send(()).unwrap();
    terminal.until("Failed");
    terminal.until("created while history failed");
    terminal.write(b"restored child\r");
    terminal.until("real child terminal");
    assert!(
        !terminal
            .transcript
            .contains("must not adopt a partial page")
    );
    terminal.write(b"x");
    proceed.send(()).unwrap();
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!terminal.transcript.contains("No output yet"));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_task_inventory_discards_old_pages_and_queued_retries_after_resuming_the_same_session() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let main = wire.read("session.history");
        wire.reply(
            &main,
            json!({"events":[command("old-main",1,1000,"old main command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        state(wire, "child");
        let failed = wire.read("session.history");
        assert_eq!(failed["params"]["branchId"], "child");
        wire.send(json!({"jsonrpc":"2.0","id":failed["id"],
            "error":{"code":-32000,"message":"old child read unavailable"}}));
        state(wire, "nested");
        let held = wire.read("session.history");
        assert_eq!(held["params"]["branchId"], "nested");
        ready.send(()).unwrap();

        // `/new` only goes home: no Session until a message.
        wire.quiet(Duration::from_millis(150));
        ready.send(()).unwrap();
        let summaries = json!([
            {"sessionId":"other","cwd":"/workspace","preview":"Other workspace",
                "model":{"providerId":"test","modelId":"model"},"updatedAt":1},
            {"sessionId":"session","cwd":"/workspace","preview":"Back to original",
                "model":{"providerId":"test","modelId":"model"},"updatedAt":2},
        ]);
        let picker = wire.read("session.summaries");
        wire.reply(&picker, summaries);
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["sessionId"], "session");
        wire.reply(&attach, attach_record(&attach));
        wire.no_context();
        wire.quiet(Duration::from_millis(150));
        wire.reply(
            &held,
            json!({"events":[command("obsolete",1,1000,"obsolete page from old attachment")],
            "through":1,"nextAfter":1,"done":true}),
        );
        let main = wire.read("session.history");
        assert_eq!(
            main["params"],
            json!({"sessionId":"session","branchId":"main","after":0,"limit":256})
        );
        wire.reply(
            &main,
            json!({"events":[command("fresh-main",1,1000,"fresh main command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        state(wire, "child");
        let child = wire.read("session.history");
        assert_eq!(
            child["params"],
            json!({"sessionId":"session","branchId":"child","after":0,"limit":256})
        );
        wire.reply(
            &child,
            json!({"events":[command("fresh-child",1,2000,"fresh child command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        let mut stops = Vec::new();
        for _ in 0..2 {
            let stop = wire.read("session.exec.stop");
            assert_eq!(stop["params"]["sessionId"], "session");
            stops.push((
                stop["params"]["branchId"].as_str().unwrap().to_owned(),
                stop["params"]["taskId"].as_str().unwrap().to_owned(),
            ));
            wire.reply(&stop, json!({}));
        }
        stops.sort();
        assert_eq!(
            stops,
            [
                ("child".into(), "fresh-child".into()),
                ("main".into(), "fresh-main".into())
            ]
        );
        let mut line = String::new();
        assert_eq!(
            wire.0.read_line(&mut line).unwrap(),
            0,
            "obsolete retry or stop: {line}"
        );
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"/tasks\r");
    terminal.until("Loading branch history");
    terminal.write(b"\x1b[27u/new\r");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"/resume\r");
    terminal.until("Back to original");
    terminal.until("Other workspace");
    let newest = terminal
        .transcript
        .find("Back to original")
        .expect("newest first");
    let older = terminal
        .transcript
        .find("Other workspace")
        .expect("older second");
    assert!(
        newest < older,
        "the resume picker must show the most recently updated session first"
    );
    terminal.write(b"session\r");
    terminal.until("resumed session");
    terminal.write(b"/tasks\r");
    terminal.until("fresh child command");
    assert!(
        !terminal
            .transcript
            .contains("obsolete page from old attachment")
    );
    terminal.write(b"\x18");
    terminal.until("attempted 2, errors 0, unavailable 0");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_task_stop_all_keeps_its_finite_targets_and_waits_only_for_replies() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[
            command("main-command",1,1000,"main running command"),
            delegation("request-a",2,"first member delegation"),delivery("delivered-a",3,"request-a"),
            delegation("request-b",4,"second member delegation"),delivery("delivered-b",5,"request-b"),
            delegation("request-starting",6,"still starting member"),
            command("finished-command",7,7000,"finished retained command"),
            fact("finished-end",8,8000,"exec","ended",json!({"task_id":"finished-command","status":"completed","result":{"content":"already complete"}})),
        ],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let observe = wire.read("session.attach");
        assert_eq!(observe["params"], json!({"sessionId":"member"}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.send(json!({"jsonrpc":"2.0","id":observe["id"],"error":{"code":-32000,"message":"observation refused"}}));
        state(wire, "child");
        let child = wire.read("session.history");
        wire.reply(&child, json!({"events":[command("child-command",1,10000,"child running command")],"through":1,"nextAfter":1,"done":true}));
        let first = wire.read("session.exec.stop");
        assert_eq!(first["params"], json!({"sessionId":"session","branchId":"main","taskId":"main-command"}));
        event(wire, "main", fact("leader-notice",9,9000,"input","message",json!({"mode":"append","user":{"content":[{"type":"text","text":"leader notification while stop waits"}]}})));
        event(wire, "main", command("late-command",10,11000,"arrived after stop admission"));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["sessionId"], "session");
        assert_eq!(submit["params"]["text"], "leader stays usable");
        wire.reply(&submit, json!({"event":{}}));
        wire.send(json!({"jsonrpc":"2.0","id":first["id"],"error":{"code":-32000,"message":"first stop failed","data":{"appError":"service_reloading"}}}));
        let member = wire.read("session.stop");
        assert_eq!(member["params"], json!({"sessionId":"member","branchId":"main","reason":"stopped by the user"}));
        wire.quiet(Duration::from_millis(150));
        wire.reply(&member, json!({}));
        let child = wire.read("session.exec.stop");
        assert_eq!(child["params"], json!({"sessionId":"session","branchId":"child","taskId":"child-command"}));
        wire.reply(&child, json!({}));
        wire.quiet(Duration::from_millis(200));
        ready.send(()).unwrap();
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected extra stop or destructive request: {line}");
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/tasks\r");
    terminal.until("child running command");
    terminal.write(b"\x18\x18");
    terminal.until("Stop-all requests are already in progress");
    terminal.write(b"\x1b[27uleader stays usable\r");
    if !terminal
        .transcript
        .contains("leader notification while stop waits")
    {
        terminal.until("leader notification while stop waits");
    }
    terminal.until("command main/main-command:");
    terminal.until("first stop failed");
    terminal.until("attempted 3, errors 1, unavailable 1");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"/tasks\r");
    terminal.until("arrived after stop admission");
    terminal.until("Running");
    assert!(
        !terminal.transcript.contains("Cancelled"),
        "a stop reply fabricated a terminal fact"
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_task_stop_all_excludes_delayed_discovery_and_settles_failed_branch_reads() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let (proceed, control) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let main = wire.read("session.history");
        wire.reply(&main, json!({"events":[command("captured",1,1000,"captured command")],
            "through":1,"nextAfter":1,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let stop = wire.read("session.exec.stop");
        assert_eq!(stop["params"], json!({"sessionId":"session","branchId":"main","taskId":"captured"}));
        state(wire, "late-child");
        let late = wire.read("session.history");
        assert_eq!(late["params"], json!({"sessionId":"session","branchId":"late-child","after":0,"limit":256}));
        wire.reply(&late, json!({"events":[command("late",1,2000,"late restored command")],
            "through":1,"nextAfter":1,"done":true}));
        event(wire, "main", fact("create",2,3000,"branch","create",json!({
            "branch":"failed-child","at":{"kind":"create"},
            "source":{"kind":"host","operation_id":"create-child"},"prompt":"child prompt",
        })));
        let failed = wire.read("session.history");
        assert_eq!(failed["params"]["branchId"], "failed-child");
        event(wire, "main", fact("create-failed",3,4000,"branch","create-failed",json!({
            "status":"failed","branch":"failed-child","createEvent":2,"error":"creation refused",
        })));
        ready.send(()).unwrap();
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.send(json!({"jsonrpc":"2.0","id":failed["id"],
            "error":{"code":-32000,"message":"branch was never published"}}));
        wire.reply(&stop, json!({}));
        wire.quiet(Duration::from_millis(150));
        ready.send(()).unwrap();
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "late discovery extended the stop batch: {line}");
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/tasks\r");
    terminal.until("captured command");
    terminal.write(b"\x18");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.until("Branch failed-child: creation refused");
    terminal.until("late restored command");
    proceed.send(()).unwrap();
    terminal.until("attempted 1, errors 0, unavailable 0");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!terminal.transcript.contains("Branch history failed"));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_task_controls_queued_before_a_replacement_never_retarget_it() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let (proceed, control) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let main = wire.read("session.history");
        wire.reply(
            &main,
            json!({"events":[command("old",1,1000,"old attachment command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let usage = wire.read("session.context.usage");
        ready.send(()).unwrap();
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.reply(
            &usage,
            json!({"contextWindow":2000,"outputTokens":100,"thresholdTokens":1500,
            "estimatedInputTokens":50,"baselineSource":"fixture"}),
        );
        // `/new` goes home without a Session; the queued stop reaches nothing.
        let mut line = String::new();
        assert_eq!(
            wire.0.read_line(&mut line).unwrap(),
            0,
            "stale control reached the server: {line}"
        );
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/usage\r");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"/new\r/tasks\r\x18queued-proof");
    terminal.until("No matching tasks.");
    proceed.send(()).unwrap();
    terminal.until("Task attachment changed; reopen the panel before stopping work");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_task_stop_all_keeps_admitted_targets_after_a_session_switch() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let (proceed, control) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let main = wire.read("session.history");
        wire.reply(
            &main,
            json!({"events":[command("main",1,1000,"old main command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        state(wire, "child");
        let child = wire.read("session.history");
        wire.reply(
            &child,
            json!({"events":[command("child",1,2000,"old child command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        let first = wire.read("session.exec.stop");
        assert_eq!(
            first["params"],
            json!({"sessionId":"session","branchId":"main","taskId":"main"})
        );
        ready.send(()).unwrap();
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        wire.reply(&first, json!({}));
        let second = wire.read("session.exec.stop");
        assert_eq!(
            second["params"],
            json!({"sessionId":"session","branchId":"child","taskId":"child"})
        );
        wire.reply(&second, json!({}));
        let mut line = String::new();
        assert_eq!(
            wire.0.read_line(&mut line).unwrap(),
            0,
            "admitted batch targeted new work: {line}"
        );
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/tasks\r");
    terminal.until("old child command");
    terminal.write(b"\x18");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"\x1b[27u/new\r");
    terminal.until("new session");
    proceed.send(()).unwrap();
    terminal.until("attempted 2, errors 0, unavailable 0");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_task_inventory_keeps_fifty_terminal_facts_and_their_original_results() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        let mut events = Vec::new();
        for index in 0..52 {
            let id = format!("row-{index:02}");
            let label = if index == 40 { "equal timestamp command".to_owned() } else { id.clone() };
            events.push(command(&id, index * 2 + 1, 500, &label));
            let result = if index == 3 {
                json!({"content":"original result without an exit code"})
            } else {
                json!({"content":format!("original result {index}"),"structuredContent":{"exitCode":index}})
            };
            events.push(fact(&format!("end-{index}"), index * 2 + 2, 1000 + index, "exec", "ended", json!({"task_id":id,"status":"completed","result":result})));
        }
        events.push(delegation("aaa-request",105,"equal timestamp delivery"));
        events.push(fact("failed-delivery",106,1040,"collaboration","delivered",json!({"op_event_id":"aaa-request","status":"error","error":"agent_not_found"})));
        wire.reply(&history, json!({"events":events,"through":106,"nextAfter":106,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "finished details must not send live reads or stops: {line}");
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    let size = libc::winsize {
        ws_row: 18,
        ws_col: 48,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.signal(libc::SIGWINCH);
    terminal.write(b"/tasks\r");
    terminal.until("row-51");
    terminal.write(b"row-02");
    terminal.until("No matching tasks.");
    terminal.write(b"\x1b[27urow-03\r");
    terminal.until("original result without an exit code");
    terminal.write(b"x\x1b[27u\x1b[27uequal timestamp\r");
    terminal.until("agent_not_found");
    assert!(!terminal.transcript.contains("\"agent_not_found\""));
    assert!(!terminal.transcript.contains(" in background"));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_member_conversation_is_read_only_and_preserves_the_leader_draft() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        let created = wire.created();
        assert_eq!(created["params"]["permissionPolicy"], json!({"preset":"full-access"}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["sessionId"], "session");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[delegation("leader-member",1,"Inspect member code"),delivery("leader-delivery",2,"leader-member")],"through":2,"nextAfter":2,"done":true}));
        wire.no_context();
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"member"}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.reply(&attach, attach_record(&attach));
        let first = wire.read("session.history");
        assert_eq!(first["params"], json!({"sessionId":"member","branchId":"main","after":0,"limit":256}));
        wire.reply(&first, json!({"events":[fact("member-input",1,1000,"input","message",json!({"mode":"queue","user":{"content":[{"type":"text","text":"member user\u{1b}]52;c;never-copy\u{7}"}]}}))],"through":2,"nextAfter":1,"done":false}));
        let second = wire.read("session.history");
        assert_eq!(second["params"], json!({"sessionId":"member","branchId":"main","after":1,"through":2,"limit":256}));
        event(wire, "main", fact("leader-observation",3,3000,"input","message",json!({"mode":"append","user":{"content":[{"type":"text","text":"leader notice while reading"}]}})));
        let text = fact("member-live",3,3000,"step","text",json!({"turn_id":"member-turn","request_id":"member-model","delta":" + live"}));
        wire.notice("session.event", json!({"sessionId":"member","branchId":"main","event":text}));
        wire.notice("session.event", json!({"sessionId":"member","branchId":"main","event":text}));
        wire.notice("session.event", json!({"sessionId":"member","branchId":"other","event":fact("other-member-branch",99,3000,"step","text",json!({"turn_id":"t","request_id":"wrong","delta":"wrong member branch"}))}));
        wire.notice("session.state", json!({"sessionId":"member","branchId":"main","running":true}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"child","requestId":"wrong-root","text":"","textOffset":0,"thinking":"wrong leader branch thought","thinkingOffset":0}));
        wire.reply(&second, json!({"events":[fact("member-text",2,2000,"step","text",json!({"turn_id":"member-turn","request_id":"member-model","delta":"member original"}))],"through":2,"nextAfter":2,"done":true}));
        wire.notice("session.model.progress", json!({"sessionId":"member","branchId":"main","requestId":"member-model","text":"","textOffset":0,"thinking":"live member thought","thinkingOffset":0}));
        let stop = wire.read("session.stop");
        assert_eq!(stop["params"], json!({"sessionId":"member","branchId":"main","reason":"stopped by the user"}));
        wire.reply(&stop, json!({}));
        event(wire, "main", fact("leader-result",4,4000,"collaboration","result",json!({"request_event_id":"leader-member","agent_id":"reviewer","session_id":"member","status":"cancelled","reason":"stopped by the user","text":"real cancelled delegation result"})));
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"member"}));
        wire.reply(&detach, json!({}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["sessionId"], "session");
        assert_eq!(submit["params"]["text"], "kept leader draft and continue");
        wire.reply(&submit, json!({"event":{}}));
        wire.event(5,"text",json!({"turn_id":"leader-turn","request_id":"leader-model","delta":"leader resumed after view"}));
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected member input/configuration or leader cleanup: {line}");
    });
    let mut launch = home.command();
    launch.args(["--permission", "full-access"]);
    let mut terminal = Terminal::launch_tui(launch);
    terminal.until("test / model");
    terminal.write(b"kept leader draft\x0ftasks\r");
    terminal.until("Inspect member code");
    terminal.write(b"\r");
    terminal.until("Member Session: member");
    terminal.until("member original + live");
    terminal.until("live member thought");
    let size = libc::winsize {
        ws_row: 18,
        ws_col: 48,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.until("Member Session: member");
    terminal.write(b"\x1b[H");
    terminal.until("member user");
    terminal.write(b"\x1b[F");
    terminal.until("live member thought");
    terminal.write(b"readonly typing\r");
    terminal.write(b"\x1b[200~hidden detail paste\x1b[201~");
    terminal.write(b"x");
    terminal.until("Delegation result: Cancelled");
    terminal.until("real cancelled delegation result");
    assert!(!terminal.transcript.contains("never-copy"));
    assert!(!terminal.transcript.contains("wrong member branch"));
    assert!(!terminal.transcript.contains("wrong leader branch thought"));
    assert!(
        !terminal
            .transcript
            .contains("member original + live + live")
    );
    terminal.write(b"\x1b[27u");
    terminal.until("Background tasks");
    terminal.until("Inspect member");
    terminal.write(b"\x1b[27u and continue\r");
    terminal.until("leader resumed after view");
    assert!(terminal.transcript.contains("leader notice while reading"));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_member_conversation_settles_old_attach_or_page_before_observing_a_replacement() {
    for close_during_attach in [true, false] {
        let home = Home::new();
        let (ready, readiness) = std::sync::mpsc::channel();
        let (proceed, control) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[
                delegation("request-a",1,"old member detail"),delivery("delivery-a",2,"request-a"),
                delegation("request-b",3,"new member detail"),
                fact("delivery-b",4,4000,"collaboration","delivered",json!({"op_event_id":"request-b","status":"ok","agent_id":"reviewer-b","session_id":"member-b"})),
                ended("result-b",5,"request-b"),ended("result-a",6,"request-a"),
            ],"through":6,"nextAfter":6,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let first_attach = wire.read("session.attach");
            assert_eq!(first_attach["params"]["sessionId"], "member");
            let held = if close_during_attach {
                first_attach
            } else {
                wire.reply(&first_attach, json!({}));
                wire.read("session.history")
            };
            ready.send(()).unwrap();
            control.recv_timeout(Duration::from_secs(10)).unwrap();
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"], json!({"sessionId":"member-b"}));
            ready.send(()).unwrap();
            wire.reply(&held, if close_during_attach { json!({}) } else { json!({"events":[fact("obsolete-text",1,1000,"step","text",json!({"turn_id":"old","request_id":"old","delta":"obsolete member history"}))],"through":1,"nextAfter":1,"done":true}) });
            wire.reply(&attach, attach_record(&attach));
            let mut requests = [wire.read_any(), wire.read_any()];
            requests.sort_by_key(|request| request["method"].as_str().unwrap().to_owned());
            let [detach, history] = requests;
            assert_eq!(detach["method"], "session.detach");
            assert_eq!(detach["params"], json!({"sessionId":"member"}));
            wire.reply(&detach, json!({}));
            assert_eq!(history["params"], json!({"sessionId":"member-b","branchId":"main","after":0,"limit":256}));
            wire.reply(&history, json!({"events":[fact("new-text",1,1000,"step","text",json!({"turn_id":"new","request_id":"new","delta":"only the new member"}))],"through":1,"nextAfter":1,"done":true}));
            let detach = wire.read("session.detach");
            assert_eq!(detach["params"], json!({"sessionId":"member-b"}));
            wire.reply(&detach, json!({}));
            let mut line = String::new();
            assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected observation after cleanup: {line}");
        });
        let mut terminal = Terminal::launch_tui(home.command());
        terminal.until("test / model");
        terminal.write(b"/tasks\r");
        terminal.until("old member detail");
        terminal.write(b"\r");
        terminal.until("Member Session: member");
        readiness.recv_timeout(Duration::from_secs(10)).unwrap();
        terminal.write(b"\x1b[27u\x1b[B\r");
        terminal.until("Member Session: member-b");
        proceed.send(()).unwrap();
        readiness.recv_timeout(Duration::from_secs(10)).unwrap();
        terminal.until("only the new member");
        assert!(!terminal.transcript.contains("obsolete member history"));
        terminal.write(b"\x1b[27u");
        terminal.until("Background tasks");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_member_conversation_keeps_errors_local_and_reopens_from_a_fresh_history() {
    for outcome in 0..4 {
        let home = Home::new();
        let (ready, readiness) = std::sync::mpsc::channel();
        let (proceed, control) = std::sync::mpsc::channel();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(&history, json!({"events":[delegation("request",1,"member detail"),delivery("delivered",2,"request"),ended("result",3,"request")],"through":3,"nextAfter":3,"done":true}));
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"], json!({"sessionId":"member"}));
            if outcome == 3 {
                wire.send(json!({"jsonrpc":"2.0","id":attach["id"],"error":{"code":-32000,"message":"member attachment refused"}}));
            } else {
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            if outcome == 0 {
                wire.reply(&history, json!({"events":[],"through":0,"nextAfter":0,"done":true}));
                control.recv_timeout(Duration::from_secs(10)).unwrap();
                wire.notice("session.closed", json!({"sessionId":"member","reason":null}));
            } else {
                wire.reply(&history, json!({"events":[fact("kept-prefix",1,1000,"input","message",json!({"mode":"queue","user":{"content":[{"type":"text","text":"retained member prefix"}]}}))],"through":2,"nextAfter":1,"done":false}));
                let page = wire.read("session.history");
                if outcome == 1 {
                    wire.send(json!({"jsonrpc":"2.0","id":page["id"],"error":{"code":-32000,"message":"member page failed"}}));
                } else {
                    wire.reply(&page, json!({"events":[fact("invalid-tail",3,3000,"step","text",json!({"turn_id":"t","request_id":"r","delta":"must not adopt invalid tail"}))],"through":2,"nextAfter":2,"done":true}));
                }
            }
            let detach = wire.read("session.detach");
            assert_eq!(detach["params"], json!({"sessionId":"member"}));
            if outcome == 1 {
                wire.send(json!({"jsonrpc":"2.0","id":detach["id"],"error":{"code":-32000,"message":"member cleanup refused"}}));
            } else {
                wire.reply(&detach, json!({}));
            }
            }
            wire.quiet(Duration::from_millis(150));
            ready.send(()).unwrap();
            if outcome != 0 {
                let attach = wire.read("session.attach");
                assert_eq!(attach["params"], json!({"sessionId":"member"}));
                wire.reply(&attach, attach_record(&attach));
                let history = wire.read("session.history");
                assert_eq!(history["params"], json!({"sessionId":"member","branchId":"main","after":0,"limit":256}));
                wire.reply(&history, json!({"events":[fact("fresh-text",1,1000,"step","text",json!({"turn_id":"fresh","request_id":"fresh","delta":"fresh member after retry"}))],"through":1,"nextAfter":1,"done":true}));
                let detach = wire.read("session.detach");
                assert_eq!(detach["params"], json!({"sessionId":"member"}));
                wire.reply(&detach, json!({}));
            }
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["sessionId"], "session");
            assert_eq!(submit["params"]["text"], "leader survives member error");
            wire.reply(&submit, json!({"event":{}}));
            wire.event(4,"text",json!({"turn_id":"leader","request_id":"leader","delta":"leader continued after member detail"}));
            let mut line = String::new();
            assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected automatic reopen or member mutation: {line}");
        });
        let mut terminal = Terminal::launch_tui(home.command());
        terminal.until("test / model");
        terminal.write(b"/tasks\r");
        terminal.until("member detail");
        terminal.write(b"\r");
        if outcome == 0 {
            terminal.until("No conversation yet");
            proceed.send(()).unwrap();
            terminal.until("Session closed");
            readiness.recv_timeout(Duration::from_secs(10)).unwrap();
            terminal.write(b"x");
        } else {
            terminal.until("Conversation unavailable");
            if outcome != 3 {
                assert!(terminal.transcript.contains("retained member prefix"));
            }
            readiness.recv_timeout(Duration::from_secs(10)).unwrap();
            terminal.write(b"\x1b[27u\r");
            terminal.until("fresh member after retry");
        }
        assert!(!terminal.transcript.contains("must not adopt invalid tail"));
        terminal.write(b"\x1b[27u\x1b[27uleader survives member error\r");
        terminal.until("leader continued after member detail");
        if outcome == 1 {
            assert!(terminal.transcript.contains("member cleanup refused"));
        }
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_member_conversation_yields_to_a_leader_approval_and_cleans_up_its_held_page() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[delegation("request",1,"member approval detail"),delivery("delivered",2,"request"),ended("result",3,"request")],"through":3,"nextAfter":3,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"member"}));
        wire.reply(&attach, attach_record(&attach));
        let held = wire.read("session.history");
        wire.notice("approval.request", json!({"sessionId":"member","branchId":"main","requestId":"member-approval","call":{"name":"exec","input":{"command":"member must not be approved here"}},"reason":"member approval is read-only"}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"main","requestId":"leader-approval","call":{"name":"exec","input":{"command":"leader command"}},"reason":"leader approval takes priority"}));
        wire.notice("approval.cancelled", json!({"sessionId":"member","branchId":"main","requestId":"leader-approval","reason":"member request cancelled"}));
        wire.event(4,"text",json!({"turn_id":"leader","request_id":"leader","delta":"leader live after member cancellation"}));
        let approval = wire.read("approval.respond");
        assert_eq!(approval["params"]["sessionId"], "session");
        assert_eq!(approval["params"]["requestId"], "leader-approval");
        assert_eq!(approval["params"]["decision"], "allow");
        wire.reply(&approval, json!({}));
        wire.reply(&held, json!({"events":[fact("obsolete-after-prompt",1,1000,"step","text",json!({"turn_id":"member","request_id":"member","delta":"obsolete page after priority prompt"}))],"through":1,"nextAfter":1,"done":true}));
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"member"}));
        wire.reply(&detach, json!({}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["sessionId"], "session");
        assert_eq!(submit["params"]["text"], "leader after approval");
        wire.reply(&submit, json!({"event":{}}));
        wire.event(5,"text",json!({"turn_id":"leader","request_id":"leader","delta":"leader input after observation cleanup"}));
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected member action or reopen: {line}");
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/tasks\r");
    terminal.until("member approval detail");
    terminal.write(b"\r");
    terminal.until("leader approval takes priority");
    terminal.until("leader live after member cancellation");
    terminal.write(b"y");
    terminal.until("test / model");
    terminal.write(b"leader after approval\r");
    terminal.until("leader input after observation cleanup");
    assert!(
        !terminal
            .transcript
            .contains("member must not be approved here")
    );
    assert!(
        !terminal
            .transcript
            .contains("obsolete page after priority prompt")
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_main_history_failure_is_visible_and_disables_submission() {
    for malformed in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[]);
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            assert_eq!(history["params"]["branchId"],"main");
            if malformed {
                wire.reply(&history,json!({"events":[],"through":8,"nextAfter":7,"done":true}));
            } else {
                wire.send(json!({"jsonrpc":"2.0","id":history["id"],"error":{"code":-32000,"message":"main store unreadable","data":{"appError":"storage_failed"}}}));
            }
            wire.no_context();
            loop {
                let mut line=String::new();
                if wire.0.read_line(&mut line).unwrap()==0 { break; }
                let request:Value=serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"],"account.providers","failed main history must not accept a hidden turn: {line}");
                wire.reply(&request,json!([]));
            }
        });
        let mut launch = home.command();
        launch.args(["--resume", "session"]);
        let mut terminal = Terminal::launch_tui(launch);
        terminal.until("Main branch history:");
        terminal.write(b"do not submit an invisible turn\r");
        terminal.until("Disconnected; draft kept.");
        terminal.signal(libc::SIGKILL);
        terminal.wait();
        daemon.join().unwrap();
    }
}

fn workflow_run(id: &str, name: &str, state: &str, started_by: Value, report_to: Value) -> Value {
    let ended = !matches!(state, "running" | "stopping");
    let mut run = json!({
        "run_id":id,"name":name,"source":"user","definition":{"version":"dim.workflow/v1"},
        "params":{},"cwd":"/work","started_by":started_by,
        "created_at":"2026-10-26T10:00:00.000Z","state":state,
        "stages":[
            {"id":"analyze","step":0,"state":"completed","session_id":"stage-session","result_text":"analysis stored result"},
            {"id":"tests","step":1,"state":if ended {"stopped"} else {"running"}},
            {"id":"lint","step":1,"state":if ended {"skipped"} else {"waiting"}},
        ],
        "log":[{"at":"2026-10-26T10:00:01.000Z","message":"Run accepted"}],
        "report":{"state":"none"},
    });
    if ended {
        run["ended_at"] = json!("2026-10-26T10:02:00.000Z");
    }
    if !report_to.is_null() {
        run["report_to"] = report_to;
    }
    run
}

fn tool_started(id: &str) -> Value {
    workflow_run(
        id,
        "nightly-check",
        "running",
        json!({"kind":"tool","session_id":"session","branch_id":"main","call_id":"wf-call"}),
        json!({"session_id":"session","branch_id":"main"}),
    )
}

/// Answers per-run refreshes with `record` until another request arrives.
fn next_request(wire: &mut Wire, record: &dyn Fn(&str) -> Value) -> Value {
    loop {
        let mut line = String::new();
        wire.0.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        match request["params"]["run_id"].as_str() {
            Some(id) if request["method"] == "workflow.status" => {
                assert_eq!(id, "run-a", "a historic or unknown run was read");
                wire.reply(&request, record(id));
            }
            _ => return request,
        }
    }
}

fn workflow_facts(wire: &mut Wire) {
    event(
        wire,
        "main",
        fact(
            "wf-use",
            2,
            2000,
            "step",
            "tool-call",
            json!({
                "turn_id":"turn","request_id":"request",
                "calls":[{"id":"wf-call","name":"workflow","input":{"action":"start","name":"nightly-check"}}],
            }),
        ),
    );
    event(
        wire,
        "main",
        fact(
            "wf-result",
            3,
            3000,
            "step",
            "tool-result",
            json!({
                "turn_id":"turn","request_id":"request","results":[{"call_id":"wf-call","result":{
                    "content":"{\"run_id\":\"run-a\"}\nThe result arrives as a report input.",
                    "structuredContent":{"run_id":"run-a","status":"running","stages":[]},
                }}],
            }),
        ),
    );
}

#[test]
fn tui_workflow_runs_are_discovered_from_facts_and_stopped_through_their_owner() {
    let home = Home::new();
    let (stop_read, stop_seen) = std::sync::mpsc::channel();
    let (repeat_sent, repeat) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["workflow.status", "workflow.stop"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[fact("old-report",1,1000,"input","message",json!({
            "mode":"steer","blocks":[{"type":"raw","text":"<dim-workflow kind=\"report\" run=\"run-old\" status=\"completed\">Workflow run-old: completed.</dim-workflow>"}],
        }))],"through":1,"nextAfter":1,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        workflow_facts(wire);
        let discovery = next_request(wire, &|_| tool_started("run-a"));
        assert_eq!(discovery["method"], "workflow.status");
        assert_eq!(discovery["params"], json!({}));
        let management = json!("management");
        wire.reply(&discovery, json!([
            tool_started("run-a"),
            workflow_run("run-managed","managed run","completed",management.clone(),json!({"session_id":"session","branch_id":"main"})),
            workflow_run("run-other","unrelated run","running",management.clone(),json!({"session_id":"other","branch_id":"main"})),
            workflow_run("run-loose","unaddressed run","running",management,Value::Null),
            workflow_run("run-old","old nightly","completed",json!({"kind":"tool","session_id":"session","branch_id":"child","call_id":"c"}),json!({"session_id":"session","branch_id":"child"})),
        ]));
        let stop = next_request(wire, &|_| tool_started("run-a"));
        assert_eq!(stop["method"], "workflow.stop");
        assert_eq!(stop["params"], json!({"run_id":"run-a"}));
        stop_read.send(()).unwrap();
        repeat.recv_timeout(Duration::from_secs(10)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            wire.0.get_ref().set_read_timeout(Some(Duration::from_millis(100))).unwrap();
            let mut line = String::new();
            if wire.0.read_line(&mut line).is_ok() {
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "workflow.status", "unexpected {line}");
                assert_eq!(request["params"], json!({"run_id":"run-a"}));
                wire.reply(&request, tool_started("run-a"));
            }
        }
        wire.0.get_ref().set_read_timeout(None).unwrap();
        let stopped = workflow_run("run-a","nightly-check","stopped",
            json!({"kind":"tool","session_id":"session","branch_id":"main","call_id":"wf-call"}),
            json!({"session_id":"session","branch_id":"main"}));
        wire.reply(&stop, stopped);
        wire.quiet(Duration::from_millis(1500));
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("1 in background");
    terminal.write(b"/tasks\r");
    terminal.until("nightly-check");
    terminal.until("workflow");
    terminal.until("old nightly");
    for hidden in ["unrelated run", "unaddressed run"] {
        assert!(
            !terminal.transcript.contains(hidden),
            "{hidden} is not this Session's run"
        );
    }
    assert!(terminal.transcript.contains("managed run"));
    terminal.write(b"\r");
    terminal.until("Run: run-a");
    terminal.until("analysis stored result");
    terminal.until("Run accepted");
    terminal.until("Report: none");
    terminal.until("stop workflow");
    terminal.write(b"x");
    stop_seen.recv_timeout(Duration::from_secs(10)).unwrap();
    terminal.write(b"x");
    repeat_sent.send(()).unwrap();
    terminal.until("Stopped");
    terminal.write(b"\x1b[27u");
    terminal.until("Stopped");
    daemon.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

#[test]
fn tui_workflow_inventory_errors_block_stop_all_until_reopen_and_stops_follow_other_targets() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["workflow.status", "workflow.stop"]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[command("main-command",1,1000,"main running command")],"through":1,"nextAfter":1,"done":true}));
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        workflow_facts(wire);
        let discovery = next_request(wire, &|_| tool_started("run-a"));
        assert_eq!(discovery["params"], json!({}));
        wire.send(json!({"jsonrpc":"2.0","id":discovery["id"],"error":{"code":-32000,"message":"store busy","data":{"appError":"service_unavailable"}}}));
        let discovery = wire.read("workflow.status");
        assert_eq!(discovery["params"], json!({}));
        wire.reply(&discovery, json!([
            tool_started("run-a"),
            workflow_run("run-managed","managed run","completed",json!("management"),json!({"session_id":"session","branch_id":"main"})),
        ]));
        let command = next_request(wire, &|_| tool_started("run-a"));
        assert_eq!(command["method"], "session.exec.stop");
        assert_eq!(command["params"], json!({"sessionId":"session","branchId":"main","taskId":"main-command"}));
        wire.reply(&command, json!({}));
        let (mut stop, mut refresh, mut submitted) = (None, None, false);
        while stop.is_none() || refresh.is_none() || !submitted {
            let mut line = String::new();
            wire.0.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            match request["method"].as_str().unwrap() {
                "session.submit" => {
                    assert_eq!(request["params"]["text"], "leader stays usable");
                    wire.reply(&request, json!({"event":{}}));
                    submitted = true;
                }
                "workflow.stop" => {
                    assert_eq!(request["params"], json!({"run_id":"run-a"}));
                    assert!(stop.replace(request).is_none(), "a second stop went out");
                }
                "workflow.status" if stop.is_none() => wire.reply(&request, tool_started("run-a")),
                method => {
                    assert_eq!(method, "workflow.status");
                    assert_eq!(request["params"], json!({"run_id":"run-a"}));
                    assert!(refresh.replace(request).is_none(), "a second status read went out");
                }
            }
        }
        let (stop, refresh) = (stop.unwrap(), refresh.unwrap());
        let stopped = workflow_run("run-a","nightly-check","stopped",
            json!({"kind":"tool","session_id":"session","branch_id":"main","call_id":"wf-call"}),
            json!({"session_id":"session","branch_id":"main"}));
        wire.reply(&stop, stopped);
        wire.reply(&refresh, tool_started("run-a"));
        let discovery = wire.read("workflow.status");
        assert_eq!(discovery["params"], json!({}));
        wire.send(json!({"jsonrpc":"2.0","id":discovery["id"],"error":{"code":-32000,"message":"store busy","data":{"appError":"service_unavailable"}}}));
        wire.quiet(Duration::from_millis(1500));
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("2 in background");
    terminal.write(b"/tasks\r");
    terminal.until("Workflow: last observed; refresh failed");
    terminal.until("store busy");
    terminal.write(b"\x18");
    terminal.until("Workflow inventory is incomplete");
    std::thread::sleep(Duration::from_millis(1500));
    terminal.write(b"\x1b[27u/tasks\r");
    terminal.until("managed run");
    terminal.write(b"\x18");
    terminal.write(b"\x1b[27uleader stays usable\r");
    terminal.until("attempted 2, errors 0, unavailable 0");
    terminal.write(b"/tasks\r");
    terminal.until("refresh failed");
    terminal.until("nightly-check");
    terminal.until("Stopped");
    daemon.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

/// The leader's `agent` request call and the delegation it made.
fn agent_request(
    sequence: u64,
    request: &str,
    call: &str,
    target: Value,
    prompt: &str,
) -> [Value; 3] {
    [
        fact(
            &format!("use-{sequence}"),
            sequence,
            sequence * 1000,
            "step",
            "tool-use",
            json!({"turn_id":"turn","request_id":request,"call":{"id":call,"name":"agent","input":{"action":"request","prompt":prompt}}}),
        ),
        fact(
            &format!("req-{sequence}"),
            sequence + 1,
            (sequence + 1) * 1000,
            "collaboration",
            "request",
            json!({"turn_id":"turn","request_id":request,"call_id":call,"target":target,"prompt":prompt}),
        ),
        fact(
            &format!("result-{sequence}"),
            sequence + 2,
            (sequence + 2) * 1000,
            "step",
            "tool-result",
            json!({"turn_id":"turn","request_id":request,"results":[{"call_id":call,"result":{"content":"{\"status\":\"accepted\"}"}}]}),
        ),
    ]
}

fn delivered(sequence: u64, request: &str, agent: &str, session: &str) -> Value {
    fact(
        &format!("delivered-{sequence}"),
        sequence,
        sequence * 1000,
        "collaboration",
        "delivered",
        json!({"op_event_id":request,"status":"ok","agent_id":agent,"session_id":session}),
    )
}

/// The refusal an unaddressable Agent gets from the server.
const AGENT_NOT_FOUND: &str = "agent_not_found: no visible Agent has this agentId; call list and use an exact agents[].agentId, or give a profile to create a temporary Agent";

/// A request the server refused before a member Session existed: the call,
/// its request fact, the refusal the model read and the failed delivery the
/// card is finished by.
fn refused_request(
    sequence: u64,
    request: &str,
    call: &str,
    agent_id: &str,
    prompt: &str,
) -> [Value; 4] {
    let [use_event, request_event, _] = agent_request(
        sequence,
        request,
        call,
        json!({"kind":"agent","agent_id":agent_id}),
        prompt,
    );
    [
        use_event,
        request_event,
        fact(
            &format!("result-{sequence}"),
            sequence + 2,
            (sequence + 2) * 1000,
            "step",
            "tool-result",
            json!({"turn_id":"turn","request_id":request,"results":[{"call_id":call,
                "result":{"content":AGENT_NOT_FOUND,"isError":true}}]}),
        ),
        fact(
            &format!("delivered-{sequence}"),
            sequence + 3,
            (sequence + 3) * 1000,
            "collaboration",
            "delivered",
            json!({"op_event_id":format!("req-{sequence}"),"status":"error","error":"agent_not_found"}),
        ),
    ]
}

fn member_event(wire: &mut Wire, member: &str, event: Value) {
    wire.notice(
        "session.event",
        json!({"sessionId":member,"branchId":"main","event":event}),
    );
}

#[test]
fn tui_member_cards_share_observations_and_show_the_members_actual_model() {
    let home = Home::new();
    let (opened, wait_opened) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        let mut events = Vec::new();
        events.extend(agent_request(1, "r1", "call-1", json!({"kind":"agent","agent_id":"reviewer"}), "Check crate dependencies"));
        events.push(delivered(4, "req-1", "reviewer", "member-a"));
        events.extend(agent_request(5, "r2", "call-1", json!({"kind":"temporary","profile":{"name":"inspect tests","description":"","systemPrompt":""}}), "Review the integration coverage"));
        events.push(delivered(8, "req-5", "temporary-agent", "member-b"));
        events.extend(agent_request(9, "r3", "call-3", json!({"kind":"agent","agent_id":"external-agent:node:agent"}), "Inspect the repository"));
        events.push(delivered(12, "req-9", "external-agent:node:agent", "member-c"));
        wire.reply(&history, json!({"events":events,"through":12,"nextAfter":12,"done":true}));
        wire.no_context();
        let mut requests = [wire.read_any(), wire.read_any(), wire.read_any()];
        requests.sort_by_key(|request| format!("{}{}", request["method"], request["params"]));
        let [list, attach_a, attach_b] = requests;
        assert_eq!(list["method"], "agent.list");
        assert_eq!(attach_a["params"], json!({"sessionId":"member-a"}));
        assert_eq!(attach_b["params"], json!({"sessionId":"member-b"}));
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        wire.reply(&list, json!([{"agentId":"reviewer","default":false,"name":"Code Reviewer","description":"","sessions":1}]));
        wire.reply(&attach_a, attach_record(&attach_a));
        wire.reply(&attach_b, attach_record(&attach_b));
        for _ in 0..2 {
            let page = wire.read("session.history");
            wire.quiet(Duration::from_millis(150));
            let events = if page["params"]["sessionId"] == "member-a" {
                json!([
                    fact("configured-a",1,1000,"context","configured",json!({"timeline":"main","revision":"a","previous":null,"prompt":null,"provider_id":"member-provider","model_id":"member-model"})),
                    fact("read-a",2,2000,"step","tool-use",json!({"turn_id":"a","request_id":"a","call":{"id":"read","name":"read","input":{"path":"Cargo.toml"}}})),
                ])
            } else {
                assert_eq!(page["params"]["sessionId"], "member-b");
                json!([])
            };
            wire.reply(&page, json!({"events":events,"through":2,"nextAfter":2,"done":true}));
        }
        member_event(wire, "member-a", fact("configured-a2",3,3000,"context","configured",json!({"timeline":"main","revision":"a2","previous":"a","prompt":null,"provider_id":"member-provider","model_id":"member-model-2"})));
        wait_opened.recv_timeout(PATIENCE).unwrap();
        wire.quiet(Duration::from_millis(150));
        event(wire, "main", fact("leader-result",13,20000,"collaboration","result",json!({"request_event_id":"req-5","agent_id":"temporary-agent","session_id":"member-b","status":"completed","text":"Found two missing restart cases"})));
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"member-b"}));
        wire.reply(&detach, json!({}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("member-c"), "provider member addressed as a Session: {line}");
            assert!(!line.contains("session.attach"), "unexpected observation: {line}");
            line.clear();
        }
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Agent: Code Reviewer");
    terminal.until("member-provider / member-model-2");
    assert!(
        terminal
            .transcript
            .contains("Provider member: external-agent:node:agent")
    );
    terminal.write(b"\t\r");
    terminal.until("Member Session: member-b");
    terminal.until("Member model: No model observed");
    terminal.write(b"\x1b[27u");
    terminal.write(b"\x1b[A\r");
    terminal.until("Member Session: member-a");
    terminal.until("Member model: member-provider / member-model-2");
    terminal.write(b"\x1b[27u");
    opened.send(()).unwrap();
    terminal.until("Completed");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
    assert!(!terminal.transcript.contains("Member Session: member-c"));
}

#[test]
fn tui_member_cards_attach_only_unfinished_members_and_keep_a_failed_read_unavailable() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        let mut events = Vec::new();
        events.extend(agent_request(1, "r1", "call-1", json!({"kind":"agent","agent_id":"finished"}), "Restored ended delegation"));
        events.push(delivered(4, "req-1", "finished", "member-ended"));
        events.push(fact("ended",5,5000,"collaboration","result",json!({"request_event_id":"req-1","agent_id":"finished","session_id":"member-ended","status":"completed","text":"restored result text"})));
        events.extend(agent_request(6, "r2", "call-2", json!({"kind":"agent","agent_id":"running"}), "Still running delegation"));
        events.push(delivered(9, "req-6", "running", "member-running"));
        wire.reply(&history, json!({"events":events,"through":9,"nextAfter":9,"done":true}));
        wire.no_context();
        let mut requests = [wire.read_any(), wire.read_any(), wire.read_any()];
        requests.sort_by_key(|request| request["method"].to_string());
        let [providers, names, attach] = requests;
        assert_eq!(providers["method"], "account.providers");
        assert_eq!(names["method"], "agent.list");
        assert_eq!(attach["method"], "session.attach");
        assert_eq!(attach["params"], json!({"sessionId":"member-running"}));
        wire.reply(&providers, json!([]));
        wire.reply(&names, json!([]));
        wire.send(json!({"jsonrpc":"2.0","id":attach["id"],"error":{"code":-32000,"message":"member unavailable"}}));
        let mut line = String::new();
        assert_eq!(wire.0.read_line(&mut line).unwrap(), 0, "unexpected observation: {line}");
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("No model observed");
    terminal.until("(observation unavailable)");
    assert!(terminal.transcript.contains("restored result text"));
    assert!(!terminal.transcript.contains("test / model · Completed"));
    thread::sleep(Duration::from_millis(300));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn tui_member_cards_read_agent_names_once_for_ids_the_list_does_not_return() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        let mut events = Vec::new();
        events.extend(refused_request(
            1,
            "r1",
            "call-1",
            "deleted-agent",
            "Ask a deleted Agent",
        ));
        events.extend(agent_request(
            5,
            "r2",
            "call-2",
            json!({"kind":"agent","agent_id":"unlisted-agent"}),
            "Ask an unlisted Agent",
        ));
        wire.reply(
            &history,
            json!({"events":events,"through":7,"nextAfter":7,"done":true}),
        );
        wire.no_context();
        let mut requests = [wire.read_any(), wire.read_any()];
        requests.sort_by_key(|request| request["method"].to_string());
        let [providers, list] = requests;
        assert_eq!(providers["method"], "account.providers");
        assert_eq!(list["method"], "agent.list");
        wire.reply(&providers, json!([]));
        wire.reply(&list, json!([]));
        wire.quiet(Duration::from_millis(1000));
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Agent: unlisted-agent");
    daemon.join().unwrap();
    assert!(terminal.transcript.contains("agent_not_found"));
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

/// `agent.list` is a built-in method, so no `hello` advertises it: the card
/// still asks for the Agent's name.
#[test]
fn tui_member_cards_name_a_native_agent_the_hello_does_not_announce() {
    let home = Home::new();
    let (named, wait_named) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        let events = agent_request(
            1,
            "r1",
            "call-1",
            json!({"kind":"agent","agent_id":"reviewer"}),
            "Check crate dependencies",
        );
        wire.reply(
            &history,
            json!({"events":events,"through":3,"nextAfter":3,"done":true}),
        );
        wire.no_context();
        let mut requests = [wire.read_any(), wire.read_any()];
        requests.sort_by_key(|request| request["method"].to_string());
        let [providers, list] = requests;
        assert_eq!(providers["method"], "account.providers");
        assert_eq!(list["method"], "agent.list");
        wire.reply(&providers, json!([]));
        wire.reply(
            &list,
            json!([{"agentId":"reviewer","name":"Code Reviewer"}]),
        );
        wait_named.recv_timeout(PATIENCE).unwrap();
        wire.quiet(Duration::from_millis(150));
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Agent: Code Reviewer");
    named.send(()).unwrap();
    daemon.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

/// A failed shared read keeps the id on the card it covered, a later card for
/// the same Agent asks nothing again, and the transcript keeps drawing.
#[test]
fn tui_member_cards_keep_the_id_when_the_shared_agent_list_read_fails() {
    let home = Home::new();
    let (drawn, wait_drawn) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        let events = agent_request(
            1,
            "r1",
            "call-1",
            json!({"kind":"agent","agent_id":"reviewer"}),
            "First review",
        );
        wire.reply(
            &history,
            json!({"events":events,"through":3,"nextAfter":3,"done":true}),
        );
        wire.no_context();
        let mut requests = [wire.read_any(), wire.read_any()];
        requests.sort_by_key(|request| request["method"].to_string());
        let [providers, list] = requests;
        assert_eq!(providers["method"], "account.providers");
        assert_eq!(list["method"], "agent.list");
        wire.reply(&providers, json!([]));
        wire.send(json!({"jsonrpc":"2.0","id":list["id"],"error":{"code":-32000,"message":"agent list unavailable"}}));
        for value in agent_request(
            4,
            "r2",
            "call-2",
            json!({"kind":"agent","agent_id":"reviewer"}),
            "Second review",
        ) {
            event(wire, "main", value);
        }
        wire.quiet(Duration::from_millis(1000));
        event(
            wire,
            "main",
            fact(
                "result-7",
                7,
                7000,
                "collaboration",
                "result",
                json!({"request_event_id":"req-4","agent_id":"reviewer","session_id":"member",
                    "status":"completed","text":"second review is complete"}),
            ),
        );
        wait_drawn.recv_timeout(PATIENCE).unwrap();
        wire.quiet(Duration::from_millis(150));
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    terminal.until("Agent: reviewer");
    terminal.until("Second review");
    terminal.until("agent result: second review is complete");
    drawn.send(()).unwrap();
    daemon.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

/// A delegation the server refused before a member existed is a rejected
/// attempt: the group counts it apart from the Agents the leader worked with,
/// and its card names the target and the refusal instead of a failure and a
/// model.
#[test]
fn tui_rejected_delegations_are_not_agents_the_leader_worked_with() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        let mut events = Vec::new();
        events.extend(refused_request(
            1,
            "a",
            "a-1",
            "default",
            "Check the imports",
        ));
        events.extend(refused_request(
            5,
            "a",
            "a-2",
            "default",
            "Review the tests",
        ));
        events.extend(refused_request(
            9,
            "a",
            "a-3",
            "default",
            "Inspect the release",
        ));
        events.extend(refused_request(20, "b", "b-1", "default", "Check the docs"));
        events.extend(agent_request(
            24,
            "b",
            "b-2",
            json!({"kind":"agent","agent_id":"reviewer"}),
            "Review the coverage",
        ));
        events.push(delivered(27, "req-24", "reviewer", "member-b"));
        events.push(fact(
            "result-b",
            28,
            28000,
            "collaboration",
            "result",
            json!({"request_event_id":"req-24","agent_id":"reviewer","session_id":"member-b",
                "status":"completed","text":"the coverage is complete"}),
        ));
        events.extend(agent_request(
            30,
            "c",
            "c-1",
            json!({"kind":"agent","agent_id":"reviewer"}),
            "Run the suite",
        ));
        events.push(delivered(33, "req-30", "reviewer", "member-c"));
        events.push(fact(
            "result-c",
            34,
            40000,
            "collaboration",
            "result",
            json!({"request_event_id":"req-30","agent_id":"reviewer","session_id":"member-c",
                "status":"failed","reason":"the member stopped","text":"two tests failed"}),
        ));
        wire.reply(
            &history,
            json!({"events":events,"through":34,"nextAfter":34,"done":true}),
        );
        wire.no_context();
        let mut requests = [wire.read_any(), wire.read_any()];
        requests.sort_by_key(|request| request["method"].to_string());
        let [providers, names] = requests;
        assert_eq!(providers["method"], "account.providers");
        assert_eq!(names["method"], "agent.list");
        wire.reply(&providers, json!([]));
        wire.reply(&names, json!([]));
        wire.quiet(Duration::from_millis(1000));
    });
    let mut command = home.command();
    command.arg("--fullscreen");
    let mut terminal = Terminal::launch_tui(command);
    let size = libc::winsize {
        ws_row: 60,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::ioctl(
                terminal.master.as_ref().unwrap().as_raw_fd(),
                libc::TIOCSWINSZ,
                &size,
            )
        },
        0
    );
    terminal.signal(libc::SIGWINCH);
    terminal.until("3 delegations rejected");
    for _ in 0..3 {
        terminal.until("Not started");
    }
    terminal.until("Reason: agent_not_found:");
    terminal.until("no visible Agent has this agentId");
    assert!(
        !terminal.transcript.contains("· Not started"),
        "a rejected delegation has no model line"
    );
    terminal.until("Worked with 1 agent · 1 rejected");
    terminal.until("Not started");
    terminal.until("No model observed · Failed");
    assert!(
        terminal.transcript.contains("the member stopped"),
        "a member that ran and failed keeps its real result"
    );
    daemon.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}

/// `/new` goes home without a Session; the first message creates one on the
/// model shown, leaves the old Session and only then submits.
#[test]
fn tui_new_creates_the_session_on_the_first_message() {
    let home = Home::new();
    let (ready, readiness) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let main = wire.read("session.history");
        wire.reply(
            &main,
            json!({"events":[command("old",1,1000,"old session command")],
            "through":1,"nextAfter":1,"done":true}),
        );
        wire.no_context();
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([]));
        ready.send(()).unwrap();
        wire.quiet(Duration::from_millis(300));
        ready.send(()).unwrap();
        let create = wire.read("session.create");
        assert_eq!(
            create["params"]["model"],
            json!({"provider":"test","modelId":"model"})
        );
        wire.reply(
            &create,
            json!({"sessionId":"fresh","sequence":0,
            "model":{"providerId":"test","modelId":"model"}}),
        );
        let detach = wire.read("session.detach");
        assert_eq!(detach["params"], json!({"sessionId":"session"}));
        wire.reply(&detach, json!({}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["sessionId"], "fresh");
        wire.reply(&attach, attach_record(&attach));
        loop {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("session.history") => {
                    assert_eq!(request["params"]["sessionId"], "fresh");
                    wire.reply(
                        &request,
                        json!({"events":[],"through":0,"nextAfter":0,"done":true}),
                    );
                }
                Some("session.submit") => {
                    assert_eq!(request["params"]["sessionId"], "fresh");
                    assert_eq!(request["params"]["text"], "first words");
                    wire.reply(&request, json!({"event":{}}));
                    break;
                }
                method => panic!("unexpected request {method:?}"),
            }
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"/new\r");
    terminal.until("new session");
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"first words\r");
    daemon.join().unwrap();
    terminal.signal(libc::SIGKILL);
    terminal.wait();
}
