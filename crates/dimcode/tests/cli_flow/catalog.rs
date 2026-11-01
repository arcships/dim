use super::*;

fn rows(start: usize, end: usize) -> Value {
    (start..end).map(|index|json!({"sessionId":format!("session-{index}"),"cwd":"/workspace","preview":format!("topic{index:04}"),"model":{"providerId":"test","modelId":"model"},"updatedAt":10000-index})).collect()
}

#[test]
fn tui_resume_catalog_pages_searches_and_closes_without_waiting_for_a_read() {
    let home = Home::new();
    let (ready, waiting) = std::sync::mpsc::channel();
    let daemon=home.daemon(move |wire| {
        wire.hello(&["session.catalog", "session.catalog.cancel"]);
        wire.created();
        let attach=wire.read("session.attach");
        wire.reply(&attach,attach_record(&attach));
        let history=wire.read("session.history");
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_context();
        let providers=wire.read("account.providers");
        wire.reply(&providers,json!([]));
        let first=wire.read("session.catalog");
        assert_eq!(first["params"],json!({"limit":50,"query":"","withPreview":true}));
        wire.reply(&first,json!({"sessions":rows(0,50),"nextCursor":"second-page"}));
        let second=wire.read("session.catalog");
        assert_eq!(second["params"],json!({"limit":50,"query":"","withPreview":true,"cursor":"second-page"}));
        wire.reply(&second,json!({"sessions":rows(50,100)}));
        let query=wire.read("session.catalog");
        assert_eq!(query["params"],json!({"limit":50,"query":"ä title","withPreview":true}));
        wire.reply(&query,json!({"sessions":[{"sessionId":"off-page","cwd":"/workspace","title":"off-page result","preview":"different preview","model":{"providerId":"test","modelId":"model"},"updatedAt":2}]}));
        let cleared=wire.read("session.catalog");
        assert_eq!(cleared["params"]["query"],"");
        wire.reply(&cleared,json!({"sessions":rows(0,50)}));
        let held=wire.read("session.catalog");
        ready.send(()).unwrap();
        let cancelled=wire.read("session.catalog.cancel");
        assert_eq!(cancelled["params"], json!({"requestId":held["id"]}));
        wire.reply(&cancelled,json!({}));
        let usage=wire.read("session.context.usage");
        wire.send(json!({"jsonrpc":"2.0","id":usage["id"],"error":{"code":-32601,"message":"unsupported","data":{"appError":"unknown_method"}}}));
        wire.reply(&held,json!({"sessions":[{"sessionId":"obsolete","cwd":"/workspace","preview":"STALE CATALOG ROW","model":{"providerId":"test","modelId":"model"},"updatedAt":1}]}));
        let reopened=wire.read("session.catalog");
        wire.reply(&reopened,json!({"sessions":[{"sessionId":"fresh","cwd":"/workspace","preview":"fresh opening","model":{"providerId":"test","modelId":"model"},"updatedAt":3}]}));
        no_catalog(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("topic0000");
    terminal.write(b"\x1b[F");
    terminal.until("topic0049");
    terminal.write("ä title".as_bytes());
    terminal.until("off-page result");
    terminal.write(b"\x1b[27u");
    terminal.until("topic0000");
    terminal.write(b"\x12");
    waiting.recv_timeout(PATIENCE).unwrap();
    terminal.write(b"\x1b[27u");
    terminal.write(b"/usage\r");
    terminal.until("This daemon does not track context usage");
    terminal.write(b"/resume\r");
    terminal.until("fresh opening");
    assert!(!terminal.transcript.contains("STALE CATALOG ROW"));
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// #557: the picker labels every row by its creation origin and asks for ACP
/// Sessions only when the saved preference says so.
#[test]
fn tui_resume_labels_origins_and_shows_acp_sessions_when_asked() {
    let home = Home::new();
    fs::write(
        home.0.join("config.json"),
        r#"{"locale":"en","showAcpSessions":true}"#,
    )
    .unwrap();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[
            "session.catalog",
            "session.catalog.cancel",
            "session.catalog.origins",
        ]);
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
        let first = wire.read("session.catalog");
        assert_eq!(
            first["params"],
            json!({"limit":50,"query":"","withPreview":true,"withAcpSessions":true})
        );
        wire.reply(
            &first,
            json!({"sessions":[
                {"sessionId":"chat","cwd":"/workspace","title":"Update cache","preview":"chat preview",
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":5,"origin":"client"},
                {"sessionId":"editor","cwd":"/workspace","title":"Editor conversation","preview":"acp preview",
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":4,"origin":"acp"},
                {"sessionId":"helper","cwd":"/workspace","title":"Inspect cache","preview":"agent preview",
                 "model":{"providerId":"test","modelId":"model"},"updatedAt":3,"origin":"internal",
                 "parentSessionId":"chat"}
            ]}),
        );
        no_catalog(wire);
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    terminal.write(b"/resume\r");
    terminal.until("[Chat] Update cache");
    terminal.until("[ACP ] Editor conversation");
    terminal.until("[Agent] Inspect cache");
    terminal.write(b"\x1b[27u");
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn a_cold_ui_leaf_submits_its_first_message_with_a_large_native_catalog() {
    fn rpc(wire: &mut BufReader<UnixStream>, method: &str, params: Value) -> Value {
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        wire.get_mut().flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                wire.read_line(&mut line).unwrap() > 0,
                "{method}: connection closed"
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == 1 {
                return value;
            }
        }
    }
    let binary = std::env::var("DIM_TEST_APP_SERVER").expect("DIM_TEST_APP_SERVER");
    let home = Home::new();
    let source = Home::new();
    super::migration::legacy_home(
        &source.0,
        "dimcode.sqlite",
        &"large imported context ".repeat(5000),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let model = home.0.join("model.json");
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../dim-agent-aimux/examples/offline.json"
    ))
    .unwrap();
    config["providers"][0]["baseUrl"] = format!("http://{address}/v1").into();
    fs::write(&model, config.to_string()).unwrap();
    let command = || {
        let mut command = home.command();
        command.args([
            "--server",
            &binary,
            "--model-config",
            model.to_str().unwrap(),
            "--cwd",
            home.0.to_str().unwrap(),
        ]);
        command
    };
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let imported = command()
        .args([
            "migration",
            "run",
            "--source",
            source.0.to_str().unwrap(),
            "--source-stopped",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert!(
        command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut database = rusqlite::Connection::open(home.0.join("rust/dimcode.sqlite")).unwrap();
    let seed: String = database
        .query_row("SELECT session_id FROM agent_sessions LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let bytes: i64 = database
        .query_row(
            "SELECT max(length(body)) FROM timeline_events WHERE kind='context/compaction'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        bytes > 64 * 1024,
        "the imported fixture has a large compaction: {bytes}"
    );
    let tx = database.transaction().unwrap();
    for index in 0..1000 {
        let id = format!("cold-catalog-{index:04}");
        let params = rusqlite::params![id, seed];
        for sql in [
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id) SELECT ?1, config, created_at_ms, leader_session_id FROM agent_sessions WHERE session_id=?2",
            "INSERT INTO session_owners SELECT ?1,agent_id,protocol_version FROM session_owners WHERE session_id=?2",
            "INSERT INTO timeline_branches SELECT ?1,branch_id,info,sequence FROM timeline_branches WHERE session_id=?2",
            "INSERT INTO timeline_content SELECT ?1,hash,kind,body FROM timeline_content WHERE session_id=?2",
            "INSERT INTO timeline_events SELECT ?1,branch_id,sequence,last_sequence,kind,body,event_id,context_render,context_state FROM timeline_events WHERE session_id=?2",
            "INSERT INTO timeline_snapshots SELECT ?1,branch_id,sequence,interpreter,event_id,state FROM timeline_snapshots WHERE session_id=?2",
            "INSERT INTO session_metadata SELECT ?1,revision,title,title_source FROM session_metadata WHERE session_id=?2",
            "INSERT INTO session_catalog SELECT ?1,updated_at_ms,preview,cwd FROM session_catalog WHERE session_id=?2",
        ] {
            tx.execute(sql, params).unwrap();
        }
    }
    tx.commit().unwrap();
    drop(database);
    let http = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut wire = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            wire.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut body = vec![0; length];
        wire.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("cold first submission marker"), "{body}");
        assert!(
            !body.contains("large imported context"),
            "another Session's history entered the new request"
        );
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"id":"cold-flow","choices":[{"index":0,"delta":{"content":"native-cold-reply"},"finish_reason":null}]}),
            json!({"id":"cold-flow","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
        write!(wire.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        wire.get_mut().flush().unwrap();
    });
    let start = Instant::now();
    let mut terminal = Terminal::launch_tui(command());
    terminal.until("diagram-model");
    let ready = start.elapsed();
    let stream = UnixStream::connect(home.0.join("rust/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut wire = BufReader::new(stream);
    let hello = rpc(
        &mut wire,
        "hello",
        json!({"protocolVersion":3,"clientKind":"cli"}),
    );
    assert!(
        hello["result"]["features"]
            .as_array()
            .unwrap()
            .contains(&json!("ui.leaf.2"))
    );
    let status = rpc(&mut wire, "ui.status", json!({}));
    assert_eq!(
        status["result"]["instances"].as_array().unwrap().len(),
        1,
        "{status}"
    );
    assert!(
        status["result"]["instances"][0]["uiId"]
            .as_str()
            .unwrap()
            .starts_with("ui-"),
        "the terminal uses the ui.leaf.2 path: {status}"
    );
    let sent = Instant::now();
    terminal.write(b"cold first submission marker\r");
    terminal.until("native-cold-reply");
    eprintln!(
        "1001 dormant Sessions, compaction row {bytes} bytes: cold UI {ready:?}, first reply {:?}",
        sent.elapsed()
    );
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    http.join().unwrap();
}
