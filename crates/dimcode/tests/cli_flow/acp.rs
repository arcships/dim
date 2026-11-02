use super::*;
use agent_client_protocol_schema::v1 as acp;
use std::{net::Shutdown, os::fd::OwnedFd};

const METHODS: &[&str] = &[
    "session.attach",
    "session.detach",
    "session.submit",
    "session.input_lookup",
    "session.stop",
    "session.list",
];

fn event(sequence: u64, id: &str, kind: &str, name: &str, data: Value) -> Value {
    json!({"id":id,"sequence":sequence,"create_at":1,"type":kind,"name":name,"data":data})
}

fn detached_eof(wire: &mut Wire) {
    loop {
        let mut line = String::new();
        if wire.0.read_line(&mut line).unwrap() == 0 {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            request["method"], "session.detach",
            "EOF can only detach the native connection: {request}"
        );
        wire.reply(&request, json!({}));
    }
}

fn accept_native(listener: &UnixListener) -> Wire {
    let (stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    Wire(BufReader::new(stream))
}
fn recovery_hello(wire: &mut Wire, instance: &str) {
    let hello = wire.read("hello");
    assert_eq!(hello["params"]["clientName"], "AcpGateway");
    wire.reply(&hello,json!({"instanceId":instance,"serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.input_lookup"]}));
}

/// #557: `session/new` records the editor as its creation origin, on a daemon
/// that reports the marker, and creates as before on one that does not.
#[test]
fn acp_session_new_declares_its_creation_origin_when_the_daemon_reports_one() {
    for (declared, expected) in [(true, Some("acp")), (false, None)] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            if declared {
                let mut features = METHODS.to_vec();
                features.push("session.create.origin");
                wire.hello(&features);
            } else {
                wire.hello(METHODS);
            }
            let created = wire.created();
            assert_eq!(
                created["params"].get("origin").and_then(Value::as_str),
                expected,
                "{created}"
            );
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        let created = peer.response(2);
        assert!(created.get("error").is_none(), "{created}");
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_reports_the_version_the_cli_prints() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(METHODS);
        detached_eof(wire);
    });
    let printed = home.command().arg("--version").output().unwrap();
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stderr)
    );
    let version = String::from_utf8_lossy(&printed.stdout)
        .trim()
        .strip_prefix("dimcode ")
        .expect("--version prints `dimcode` and the build's version")
        .to_owned();
    let mut peer = Peer::start(&home);
    peer.send(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    );
    let initialized = peer.response(1);
    assert_eq!(
        initialized["result"]["agentInfo"]["version"],
        json!(version),
        "initialize must advertise the build identity --version prints"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_distinguishes_rejected_and_uncertain_submissions_on_a_live_connection() {
    let cases = [
        Some("invalid_params"),
        Some("session_invalid_input"),
        Some("session_closed"),
        Some("session_build_failed"),
        Some("service_reloading"),
        Some("unknown_method"),
        Some("submission_unknown"),
        None,
        Some("storage_failed"),
    ];
    let home = Home::new();
    let (submitted, received) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach, json!({}));
        let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        for (index, app) in cases.into_iter().enumerate() {
            let submit = wire.read("session.submit");
            submitted.send(submit["params"]["submissionId"].clone()).unwrap();
            if index >= 6 {
                wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(9+index as u64,&format!("accepted-{index}"),"input","message",json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}))}));
            }
            wire.send(json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":if app.is_some(){-32000}else{-32603},"message":"Submission failed","data":app.map(|app|json!({"appError":app}))}}));
        }
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    for (index, app) in cases.into_iter().enumerate() {
        let request = index as i64 + 3;
        peer.send(request, "session/prompt", json!({"sessionId":"session","prompt":[{"type":"text","text":format!("Prompt {index}")}]}));
        let response = peer.response(request);
        let submission = received.recv_timeout(PATIENCE).unwrap();
        let data = &response["error"]["data"];
        if index < 6 {
            assert_eq!(data["code"], "native_error", "{response}");
            assert_eq!(data["nativeAppError"], app.unwrap());
        } else {
            assert_eq!(data["code"], "submission_unknown", "{response}");
            assert_eq!(data["sessionId"], "session");
            assert_eq!(data["submissionId"], submission);
            assert!(
                response["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("before sending again")
            );
        }
    }
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_rejects_an_oversized_native_frame_without_interrupting_pending_calls() {
    let home = Home::new();
    let (pending, pending_read) = std::sync::mpsc::sync_channel(0);
    let (release, released) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach, json!({}));
        let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let list = wire.read("session.list");
        pending.send(()).unwrap();
        released.recv_timeout(PATIENCE).unwrap();
        wire.reply(&list, json!([]));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["user"]["content"][0]["text"], "Small prompt");
        wire.send(json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32602,"message":"Rejected","data":{"appError":"invalid_params"}}}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    peer.send(3, "session/list", json!({}));
    pending_read.recv_timeout(PATIENCE).unwrap();
    let mut frame = json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"session","prompt":[{"type":"text","text":""}]}});
    let overhead = serde_json::to_vec(&frame).unwrap().len();
    frame["params"]["prompt"][0]["text"] = json!("x".repeat(16 * 1024 * 1024 - overhead - 1));
    writeln!(peer.input, "{frame}").unwrap();
    peer.input.flush().unwrap();
    let rejected = peer.response(4);
    assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
    assert_eq!(rejected["error"]["data"]["code"], "request_too_large");
    release.send(()).unwrap();
    assert_eq!(peer.response(3)["result"]["sessions"], json!([]));
    peer.send(
        5,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Small prompt"}]}),
    );
    assert_eq!(
        peer.response(5)["error"]["data"]["nativeAppError"],
        "invalid_params"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_context_usage_deserializes_as_protocol_integers() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.push("session.methods");
        wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let methods = wire.read("session.methods");
        wire.reply(
            &methods,
            json!({"methods":[{"name":"session.context.usage","mutating":false}]}),
        );
        let usage = wire.read("session.context.usage");
        wire.reply(
            &usage,
            json!({"estimatedInputTokens":53000.125,"contextWindow":128000.9}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    let update = loop {
        if let Some(index) = peer
            .updates
            .iter()
            .position(|frame| frame["params"]["update"]["sessionUpdate"] == "usage_update")
        {
            break peer.updates.remove(index);
        }
        let frame = peer.frame();
        peer.updates.push(frame);
    };
    let notification: acp::SessionNotification =
        serde_json::from_value(update["params"].clone()).unwrap();
    let acp::SessionUpdate::UsageUpdate(usage) = notification.update else {
        panic!("Expected a usage update")
    };
    assert_eq!((usage.used, usage.size), (53001, 128000));
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_recovers_accepted_input_without_resubmitting_and_resets_subagent_observation() {
    for restarted in [false, true] {
        let home = Home::new();
        let cwd = fs::canonicalize(&home.0).unwrap();
        let endpoint = home.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = thread::spawn(move || {
            let mut wire = accept_native(&listener);
            recovery_hello(&mut wire, "first");
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":7,"nextAfter":7,"done":true}),
            );
            let submit = wire.read("session.submit");
            let delegation = event(
                8,
                "delegation",
                "collaboration",
                "request",
                json!({"turn_id":"previous","request_id":"previous","call_id":"member","target":{"kind":"member","agent_id":"agent","session_id":"member"},"prompt":"Work"}),
            );
            wire.notice(
                "session.event",
                json!({"sessionId":"session","branchId":"main","event":delegation}),
            );
            let input = event(
                9,
                "accepted-input",
                "input",
                "message",
                json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}),
            );
            drop(wire);
            let mut wire = accept_native(&listener);
            recovery_hello(&mut wire, if restarted { "second" } else { "first" });
            let lookup = wire.read("session.input_lookup");
            assert_eq!(
                lookup["params"]["submissionId"],
                submit["params"]["submissionId"]
            );
            wire.reply(&lookup, json!({"status":"accepted","event":input}));
            let list = wire.read("session.list");
            wire.reply(&list, json!([{"sessionId":"session","cwd":cwd}]));
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            let facts = vec![
                delegation,
                input,
                event(10, "start", "turn", "start", json!({"turn_id":"owned"})),
                event(
                    11,
                    "consumed",
                    "input",
                    "consumed",
                    json!({"input_id":"accepted-input"}),
                ),
                event(
                    12,
                    "answer",
                    "step",
                    "text",
                    json!({"turn_id":"owned","request_id":"model","delta":"Recovered once"}),
                ),
                event(
                    13,
                    "end",
                    "turn",
                    "end",
                    json!({"turn_id":"owned","status":if restarted {"cancelled"} else {"completed"},"reason":if restarted {"server_restarted"} else {"end_turn"}}),
                ),
            ];
            wire.reply(
                &history,
                json!({"events":facts,"through":13,"nextAfter":13,"done":true}),
            );
            detached_eof(&mut wire);
        });
        let mut peer = Peer::start(&home);
        peer.send(1,"initialize",json!({"protocolVersion":1,"clientCapabilities":{"_meta":{"lody":{"subagentEvents":{"version":1}}}}}));
        peer.response(1);
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        peer.response(2);
        peer.send(
            3,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"Once"}]}),
        );
        let completed = peer.response(3);
        assert_eq!(
            completed["result"]["stopReason"],
            if restarted { "cancelled" } else { "end_turn" },
            "{completed}"
        );
        let text: String = peer
            .updates
            .iter()
            .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
            .map(|frame| {
                frame["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(text, "Recovered once");
        let snapshots: Vec<_> = peer
            .updates
            .iter()
            .filter(|frame| frame["method"] == "_lody/subagents/event")
            .collect();
        assert_eq!(snapshots.len(), 3, "{snapshots:?}");
        assert_eq!(snapshots[1]["params"]["snapshot"]["state"], "unknown");
        assert_eq!(snapshots[1]["params"]["snapshot"]["outputIncomplete"], true);
        assert_ne!(
            snapshots[0]["params"]["runId"],
            snapshots[2]["params"]["runId"]
        );
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_does_not_repeat_creation_when_the_native_reply_is_lost() {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let endpoint = home.0.join("rust/daemon.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
    let (restored, ready) = std::sync::mpsc::sync_channel(0);
    let daemon = thread::spawn(move || {
        let mut wire = accept_native(&listener);
        recovery_hello(&mut wire, "first");
        wire.read("session.create");
        drop(wire);
        let mut wire = accept_native(&listener);
        recovery_hello(&mut wire, "first");
        restored.send(()).unwrap();
        let list = wire.read("session.list");
        wire.reply(
            &list,
            json!([{"sessionId":"created-without-reply","cwd":cwd}]),
        );
        detached_eof(&mut wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert_eq!(
        peer.response(2)["error"]["data"]["code"],
        "daemon_unavailable"
    );
    ready.recv_timeout(PATIENCE).unwrap();
    // Wait for the native restoration fact to enter the same foreground actor.
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(
            Instant::now() < deadline,
            "Native restoration did not become ready"
        );
        peer.send(3, "session/list", json!({}));
        let list = peer.response(3);
        if list.get("error").is_none() {
            assert_eq!(
                list["result"]["sessions"][0]["sessionId"],
                "created-without-reply"
            );
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_temporary_mcp_rebinds_expired_resources_before_submission() {
    let home = Home::new();
    let (submitted, accepted) = std::sync::mpsc::sync_channel(0);
    let (release, released) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(&["session.input_lookup", "session.methods", "mcp.temporary.capabilities"]);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history"); wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let methods = wire.read("session.methods"); wire.reply(&methods,json!({"methods":[]}));
        let bound = wire.read("session.mcp_temporary.attach");
        assert_eq!(bound["params"]["servers"]["client"]["headers"]["x-fixture"],"temporary");
        assert!(bound["params"]["bindingId"].as_str().is_some());
        wire.reply(&bound,json!({}));
        let renew = wire.read("session.mcp_temporary.renew");
        assert_eq!(renew["params"]["bindingId"],bound["params"]["bindingId"]);
        writeln!(wire.0.get_mut(),"{}",json!({"jsonrpc":"2.0","id":renew["id"],"error":{"code":-32000,"message":"Binding expired","data":{"appError":"mcp_binding_missing"}}})).unwrap();
        wire.0.get_mut().flush().unwrap();
        let rebound = wire.read("session.mcp_temporary.attach");
        assert_eq!(rebound["params"],bound["params"],"resource reattachment must retain the binding identity and configuration");
        wire.reply(&rebound,json!({}));
        let submit = wire.read("session.submit");
        wire.reply(&submit,json!({"event":event(9,"input","input","message",json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}))}));
        submitted.send(()).unwrap();
        released.recv_timeout(PATIENCE).unwrap();
        for fact in [event(10,"turn","turn","start",json!({"turn_id":"turn"})),event(11,"consumed","input","consumed",json!({"input_id":"input"})),event(12,"end","turn","end",json!({"turn_id":"turn","status":"completed","reason":"end_turn"}))] { wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":fact})); }
        let detached = wire.read("session.mcp_temporary.detach");
        assert_eq!(detached["params"]["bindingId"],bound["params"]["bindingId"]);
        wire.reply(&detached,json!({}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.send(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    );
    assert_eq!(
        peer.response(1)["result"]["agentCapabilities"]["mcpCapabilities"],
        json!({"http":true,"sse":true})
    );
    peer.send(2,"session/new",json!({"cwd":home.0,"mcpServers":[{"name":"client","type":"http","url":"http://127.0.0.1:9/mcp","headers":[{"name":"X-Fixture","value":"temporary"}]}]}));
    let created = peer.response(2);
    assert!(created.get("error").is_none(), "{created}");
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Use the bound tools"}]}),
    );
    accepted.recv_timeout(PATIENCE).unwrap();
    peer.send(4,"session/load",json!({"sessionId":"session","cwd":home.0,"mcpServers":[{"name":"other","type":"http","url":"http://127.0.0.1:9/other","headers":[]}]}));
    assert_eq!(peer.response(4)["error"]["data"]["code"], "session_busy");
    release.send(()).unwrap();
    assert_eq!(peer.response(3)["result"]["stopReason"], "end_turn");
    peer.close();
    daemon.join().unwrap();
}

/// The prompt reply only arrives once the shared recovery deadline expires.
/// Waiting up to this long only bounds a settlement that never happens; it is
/// not the test's budget, so a loaded machine does not shorten it.
const RECOVERY_SETTLEMENT: Duration = Duration::from_secs(300);

#[test]
fn acp_stdio_recovery_deadline_settles_cancel_without_claiming_native_stop() {
    let home = Home::new();
    let endpoint = home.0.join("rust/daemon.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
    let (waiting, ready) = std::sync::mpsc::sync_channel(0);
    let daemon = thread::spawn(move || {
        let mut wire = accept_native(&listener);
        recovery_hello(&mut wire, "first");
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        wire.read("session.submit");
        drop(wire);
        let mut wire = accept_native(&listener);
        wire.0.get_ref().set_read_timeout(None).unwrap();
        recovery_hello(&mut wire, "first");
        wire.read("session.input_lookup");
        waiting.send(()).unwrap();
        // Keep the connection live while its lookup never replies.
        detached_eof(&mut wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Uncertain"}]}),
    );
    ready.recv_timeout(PATIENCE).unwrap();
    writeln!(
        peer.input,
        "{}",
        json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session"}})
    )
    .unwrap();
    peer.input.flush().unwrap();
    let result = peer.response_within(3, RECOVERY_SETTLEMENT);
    assert_eq!(result["result"]["stopReason"], "cancelled");
    assert_eq!(
        result["result"]["_meta"]["dim"]["nativeCancellation"],
        "unconfirmed"
    );
    let Peer {
        child,
        input,
        output,
        ..
    } = peer;
    let _ = input.shutdown(Shutdown::Write);
    drop(input);
    let failed = wait(child);
    drop(output);
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("App Server connection is unavailable")
    );
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_a_closed_preparation_cannot_publish_over_a_later_load() {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let list = wire.read("session.list"); wire.reply(&list,json!([{"sessionId":"saved","cwd":cwd}]));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let old = wire.read("session.history");
        wire.notice("session.closed",json!({"sessionId":"saved"}));
        let list = wire.read("session.list");
        wire.reply(&old,json!({"events":[event(8,"old","step","text",json!({"turn_id":"old","request_id":"old","delta":"must not leak"}))],"through":8,"nextAfter":8,"done":true}));
        wire.reply(&list,json!([{"sessionId":"saved","cwd":cwd}]));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let fresh = wire.read("session.history");
        wire.reply(&fresh,json!({"events":[event(9,"fresh","step","text",json!({"turn_id":"fresh","request_id":"fresh","delta":"fresh runtime"}))],"through":9,"nextAfter":9,"done":true}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    assert!(peer.response(2).get("error").is_some());
    peer.send(
        3,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    assert!(peer.response(3).get("error").is_none());
    assert_eq!(peer.updates.len(), 1);
    assert_eq!(
        peer.updates[0]["params"]["update"]["content"]["text"],
        "fresh runtime"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_cancelling_one_load_waiter_does_not_cancel_the_shared_preparation() {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let (waiting, ready) = std::sync::mpsc::sync_channel(0);
    let (release, delayed) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let list = wire.read("session.list"); wire.reply(&list,json!([{"sessionId":"saved","cwd":cwd}]));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history"); waiting.send(()).unwrap(); delayed.recv_timeout(PATIENCE).unwrap();
        wire.reply(&history,json!({"events":[event(8,"answer","step","text",json!({"turn_id":"turn","request_id":"model","delta":"one replay"}))],"through":8,"nextAfter":8,"done":true}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    ready.recv_timeout(PATIENCE).unwrap();
    peer.send(
        3,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    writeln!(
        peer.input,
        "{}",
        json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":2}})
    )
    .unwrap();
    peer.input.flush().unwrap();
    assert_eq!(peer.response(2)["error"]["code"], -32800);
    release.send(()).unwrap();
    assert!(peer.response(3).get("error").is_none());
    assert_eq!(peer.updates.len(), 1);
    assert_eq!(
        peer.updates[0]["params"]["update"]["content"]["text"],
        "one replay"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_bounds_in_flight_reads_and_releases_completed_requests() {
    let home = Home::new();
    let (waiting, ready) = std::sync::mpsc::sync_channel(0);
    let (release, delayed) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let calls: Vec<_> = (0..128).map(|_| wire.read("session.list")).collect();
        waiting.send(()).unwrap();
        delayed.recv_timeout(PATIENCE).unwrap();
        for call in calls {
            wire.reply(&call, json!([]));
        }
        let next = wire.read("session.list");
        wire.reply(&next, json!([]));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    for id in 2..130 {
        peer.send(id, "session/list", json!({}));
    }
    ready.recv_timeout(PATIENCE).unwrap();
    peer.send(130, "session/list", json!({}));
    assert_eq!(peer.response(130)["error"]["code"], -32001);
    release.send(()).unwrap();
    let mut ids = std::collections::HashSet::new();
    for _ in 0..128 {
        let frame = peer.frame();
        assert_eq!(frame["result"]["sessions"], json!([]));
        assert!(ids.insert(frame["id"].as_i64().unwrap()));
    }
    peer.send(131, "session/list", json!({}));
    assert_eq!(peer.response(131)["result"]["sessions"], json!([]));
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_slow_output_fails_explicitly_without_stopping_native_execution() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(METHODS); wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history"); wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":event(9,"input","input","message",json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}))}));
        wire.0.get_ref().set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let text = "x".repeat(1024*1024);
        for sequence in 10..30 {
            let frame = json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":event(sequence,&format!("text-{sequence}"),"step","text",json!({"turn_id":"turn","request_id":"model","delta":text}))}});
            if writeln!(wire.0.get_mut(),"{frame}").is_err() { break; }
        }
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|size|size > 0) {
            let frame: Value = serde_json::from_str(&line).unwrap(); assert_eq!(frame["method"],"session.detach");
            if writeln!(wire.0.get_mut(),"{}",json!({"jsonrpc":"2.0","id":frame["id"],"result":{}})).is_err() { break; }
            line.clear();
        }
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Backpressure"}]}),
    );
    let Peer {
        child,
        input,
        output,
        ..
    } = peer;
    let failed = wait(child);
    drop(input);
    drop(output);
    assert!(!failed.status.success());
    let diagnostic = String::from_utf8_lossy(&failed.stderr);
    assert!(
        diagnostic.contains("reliable output exceeds")
            || diagnostic.contains("writer did not flush a frame"),
        "{diagnostic}"
    );
    daemon.join().unwrap();
}

struct Peer {
    child: Child,
    input: UnixStream,
    output: BufReader<std::process::ChildStdout>,
    updates: Vec<Value>,
}

impl Peer {
    fn start(home: &Home) -> Self {
        Self::spawn(home.command().arg("acp"))
    }

    /// Starts an `acp` CLI whose stdin is one end of a socket pair, not a pipe.
    ///
    /// A pipe reaches EOF only once every descriptor for its write end is
    /// closed. On macOS, std creates a pipe and then marks it close-on-exec,
    /// so a child that another test thread spawns in that window can inherit
    /// the write end. Such a child outlives this peer, the CLI never reads
    /// EOF, and it neither detaches nor exits. Closing the write direction on
    /// the socket itself ends the stream regardless of who else holds a
    /// descriptor.
    fn spawn(command: &mut Command) -> Self {
        let (input, stdin) = UnixStream::pair().unwrap();
        let mut child = command
            .stdin(Stdio::from(OwnedFd::from(stdin)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
            updates: Vec::new(),
        }
    }

    fn send(&mut self, id: i64, method: &str, params: Value) {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
    }

    fn response(&mut self, id: i64) -> Value {
        self.response_within(id, PATIENCE)
    }

    /// Waits up to `patience` for the reply, for flows whose timing the CLI's
    /// own recovery deadline sets.
    fn response_within(&mut self, id: i64, patience: Duration) -> Value {
        loop {
            let frame = self.frame_within(patience);
            if frame.get("id") == Some(&json!(id)) {
                return frame;
            }
            if frame["method"] == "$/cancel_request" {
                continue;
            }
            assert!(
                matches!(
                    frame["method"].as_str(),
                    Some("session/update" | "_lody/subagents/event")
                ),
                "{frame}"
            );
            self.updates.push(frame);
        }
    }

    fn frame(&mut self) -> Value {
        self.frame_within(PATIENCE)
    }

    fn frame_within(&mut self, patience: Duration) -> Value {
        if self.output.buffer().is_empty() {
            let deadline = Instant::now() + patience;
            let mut fd = libc::pollfd {
                fd: self.output.get_ref().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match unsafe { libc::poll(&mut fd, 1, remaining.as_millis() as i32) } {
                    ready if ready > 0 => break,
                    0 => panic!("ACP frame timed out"),
                    _ => {
                        let error = std::io::Error::last_os_error();
                        assert_eq!(
                            error.kind(),
                            std::io::ErrorKind::Interrupted,
                            "ACP poll failed: {error}"
                        );
                    }
                }
            }
        }
        let mut line = String::new();
        if self.output.read_line(&mut line).unwrap() == 0 {
            let mut stderr = String::new();
            self.child
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("ACP closed before a frame: {stderr}");
        }
        serde_json::from_str(&line).expect("stdout must contain only ACP")
    }

    fn answer(&mut self, request: &Value, result: Value) {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        )
        .unwrap();
        self.input.flush().unwrap();
    }

    fn initialize(&mut self) {
        self.send(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        );
        let response = self.response(1);
        assert_eq!(response["result"]["protocolVersion"], 1, "{response}");
        assert!(
            response["result"]["agentCapabilities"]
                .get("providers")
                .is_none()
        );
    }

    fn close(self) {
        let _ = self.input.shutdown(Shutdown::Write);
        drop(self.input);
        let output = wait(self.child);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn acp_stdio_cancels_a_held_input_after_a_late_submit_reply() {
    let home = Home::new();
    let (submitted, ready) = std::sync::mpsc::sync_channel(0);
    let (release, delayed) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history"); wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let submit = wire.read("session.submit");
        submitted.send(()).unwrap(); delayed.recv_timeout(PATIENCE).unwrap();
        wire.reply(&submit,json!({"event":event(9,"held","input","message",json!({"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"held"}]},"mode":"queue"}))}));
        let withdraw = wire.read("session.input.withdraw");
        assert_eq!(withdraw["params"]["inputId"],"held");
        wire.reply(&withdraw,json!({"status":"deleted","sequence":10}));
        let stop = wire.read("session.stop");
        wire.reply(&stop,json!({}));
        let close = wire.read("session.close");
        wire.reply(&close,json!({}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"held"}]}),
    );
    ready.recv_timeout(PATIENCE).unwrap();
    writeln!(
        peer.input,
        "{}",
        json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session"}})
    )
    .unwrap();
    peer.input.flush().unwrap();
    release.send(()).unwrap();
    assert_eq!(peer.response(3)["result"]["stopReason"], "cancelled");
    writeln!(
        peer.input,
        "{}",
        json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session"}})
    )
    .unwrap();
    peer.input.flush().unwrap();
    peer.send(4, "session/close", json!({"sessionId":"session"}));
    assert!(peer.response(4).get("error").is_none());
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_routes_modes_permissions_and_question_forms_through_native_owners() {
    let home = Home::new();
    let replies = [
        json!({"action":"cancel"}),
        json!({"action":"accept","content":{}}),
        json!({"action":"unknown"}),
        json!({"action":"accept","content":{"custom0":"Custom only"}}),
        json!({"action":"accept","content":{"q0":["A","A"]}}),
        json!({"action":"decline"}),
        json!({"action":"accept","content":{"q0":[]}}),
        json!({"action":"accept","content":{"q0":"A","custom0":"C"}}),
        json!({"error":{"code":-32601,"message":"Unsupported form"}}),
    ];
    let additional_questions = replies.len();
    let daemon = home.daemon(move |wire| {
        let mut methods = METHODS.to_vec(); methods.push("session.methods"); wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history"); wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let methods = wire.read("session.methods");
        wire.reply(&methods,json!({"methods":[{"name":"session.permission.get","mutating":false},{"name":"session.permission.update","mutating":true},{"name":"session.question.respond","mutating":true}]}));
        let get = wire.read("session.permission.get");
        wire.reply(&get,json!({"version":4,"policy":{"preset":"read-only","allowedTools":["fixture"],"disallowedTools":["danger"]}}));
        let get = wire.read("session.permission.get");
        wire.reply(&get,json!({"version":4,"policy":{"preset":"read-only","allowedTools":["fixture"],"disallowedTools":["danger"]}}));
        let update = wire.read("session.permission.update");
        assert_eq!(update["params"]["expectedVersion"],4);
        assert_eq!(update["params"]["policy"],json!({"preset":"workspace-write","allowedTools":["fixture"],"disallowedTools":["danger"]}));
        wire.reply(&update,json!({"version":5,"policy":update["params"]["policy"]}));
        let get = wire.read("session.permission.get");
        wire.reply(&get,json!({"version":5,"policy":update["params"]["policy"]}));
        let get = wire.read("session.permission.get"); wire.reply(&get,json!({"version":5,"policy":update["params"]["policy"]}));
        let conflict = wire.read("session.permission.update"); assert_eq!(conflict["params"]["expectedVersion"],5);
        wire.send(json!({"jsonrpc":"2.0","id":conflict["id"],"error":{"code":-32000,"message":"Concurrent policy change","data":{"appError":"permission_conflict"}}}));
        let refresh = wire.read("session.permission.get"); wire.reply(&refresh,json!({"version":6,"policy":{"preset":"read-only","allowedTools":["fixture"],"disallowedTools":["danger"]}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["user"]["content"][0]["source"]["mediaType"],"image/png");
        let input = event(9,"input","input","message",json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}));
        wire.reply(&submit,json!({"event":input}));
        let fact = |wire: &mut Wire,value: Value|wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":value}));
        fact(wire,event(10,"start","turn","start",json!({"turn_id":"turn"})));
        fact(wire,event(11,"consumed","input","consumed",json!({"input_id":"input"})));
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"child","requestId":"approval","call":{"name":"exec","input":{"command":"echo test"}},"reason":"Run command?"}));
        let approval = wire.read("approval.respond");
        assert_eq!(approval["params"]["branchId"],"child");
        assert_eq!(approval["params"]["decision"],"deny");
        wire.reply(&approval,json!({}));
        wire.notice("approval.resolved",json!({"sessionId":"session","branchId":"child","requestId":"approval","decision":"deny"}));
        fact(wire,event(12,"ask","step","tool-call",json!({"turn_id":"turn","request_id":"model","calls":[{"id":"question","name":"ask_user","input":{"questions":[{"id":"choice","question":"Choose","options":[{"label":"A"},{"label":"B"}],"multiSelect":true,"allowCustom":true}]}}]})));
        let answer = wire.read("session.question.respond");
        assert_eq!(answer["params"]["requestId"],"model");
        assert_eq!(answer["params"]["answers"],json!([{"questionId":"choice","selected":["A","B"],"custom":"C"}]));
        wire.reply(&answer,json!({}));
        fact(wire,event(13,"result","step","tool-result",json!({"turn_id":"turn","request_id":"model","call_event_id":"ask","results":[{"call_id":"question","result":{"content":"answered"}}]})));
        for index in 0..additional_questions {
            let sequence = 14 + index as u64 * 2;
            let request = format!("model-{index}");
            let ask = format!("ask-{index}");
            fact(wire,event(sequence,&ask,"step","tool-call",json!({"turn_id":"turn","request_id":request,"calls":[{"id":"question","name":"ask_user","input":{"questions":[{"id":"choice","question":"Choose","options":[{"label":"A"},{"label":"B"}],"multiSelect":index != 3 && index != 7,"allowCustom":index != 6}]}}]})));
            let answer = wire.read("session.question.respond");
            assert_eq!(answer["params"]["requestId"], request);
            if index == 3 {
                assert_eq!(answer["params"]["answers"], json!([{"questionId":"choice","selected":[],"custom":"Custom only"}]));
            } else {
                assert_eq!(answer["params"]["declined"], true);
                assert!(answer["params"].get("answers").is_none());
            }
            wire.reply(&answer, json!({}));
            fact(wire,event(sequence+1,&format!("result-{index}"),"step","tool-result",json!({"turn_id":"turn","request_id":request,"call_event_id":ask,"results":[{"call_id":"question","result":{"content":"settled"}}]})));
        }
        fact(wire,event(14+additional_questions as u64*2,"end","turn","end",json!({"turn_id":"turn","status":"completed","reason":"end_turn"})));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.send(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{"elicitation":{"form":{}}}}),
    );
    peer.response(1);
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert_eq!(
        peer.response(2)["result"]["modes"]["currentModeId"],
        "read-only"
    );
    peer.send(
        3,
        "session/set_mode",
        json!({"sessionId":"session","modeId":"workspace-write"}),
    );
    assert!(peer.response(3).get("error").is_none());
    peer.send(
        30,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"mode","value":"full-access"}),
    );
    assert_eq!(
        peer.response(30)["error"]["data"]["nativeAppError"],
        "permission_conflict"
    );
    peer.send(4,"session/prompt",json!({"sessionId":"session","prompt":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="},{"type":"resource","resource":{"uri":"test://text","text":"inline"}}]}));
    let mut elicitation = 0;
    loop {
        let frame = peer.frame();
        match frame["method"].as_str() {
            Some("session/request_permission") => peer.answer(
                &frame,
                json!({"outcome":{"outcome":"selected","optionId":"reject_once"}}),
            ),
            Some("elicitation/create") => {
                let _: acp::CreateElicitationRequest =
                    serde_json::from_value(frame["params"].clone()).unwrap();
                let schema = &frame["params"]["requestedSchema"];
                if schema["properties"]["q0"]["type"] == "array" {
                    assert_eq!(schema["properties"]["q0"]["minItems"], 1);
                }
                assert_eq!(
                    schema["required"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("q0")),
                    elicitation == 7
                );
                if elicitation == 0 {
                    peer.answer(
                        &frame,
                        json!({"action":"accept","content":{"q0":["A","B"],"custom0":"C"}}),
                    );
                } else if let Some(error) = replies[elicitation - 1].get("error") {
                    writeln!(
                        peer.input,
                        "{}",
                        json!({"jsonrpc":"2.0","id":frame["id"],"error":error})
                    )
                    .unwrap();
                    peer.input.flush().unwrap();
                } else {
                    peer.answer(&frame, replies[elicitation - 1].clone());
                }
                elicitation += 1;
            }
            Some("session/update") => peer.updates.push(frame),
            Some("$/cancel_request") => {}
            _ => {
                assert_eq!(frame["id"], 4, "{frame}");
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                break;
            }
        }
    }
    assert_eq!(elicitation, replies.len() + 1);
    assert!(
        peer.updates.iter().any(
            |frame| frame["params"]["update"]["configOptions"][0]["currentValue"] == "read-only"
        )
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_creates_a_session_with_the_native_provider_selection() {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let daemon = home.daemon(move |wire| {
        let mut methods = METHODS.to_vec();
        methods.extend(["account.providers", "provider.selection"]);
        wire.hello(&methods);
        let providers = wire.read("account.providers");
        wire.reply(&providers, json!([{"providerId":"selected-managed","displayName":"Selected","loginMethods":["browser"]}]));
        let status = wire.read("account.status");
        wire.reply(&status, json!({"account":null}));
        for authenticated in [false, true] {
            let selection = wire.read("provider.selection");
            assert_eq!(selection["params"], json!({"cwd":cwd}));
            wire.reply(&selection, json!({"selection":{"provider_id":"selected-managed","model_id":"selected-model","version":1}}));
            let status = wire.read("account.status");
            assert_eq!(status["params"]["providerId"], "selected-managed");
            wire.reply(&status, json!({"account":authenticated.then(|| json!({"displayName":"fixture"}))}));
        }
        let created = wire.created();
        assert_eq!(created["params"]["cwd"], json!(cwd));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    let rejected = peer.response(2);
    assert_eq!(rejected["error"]["code"], -32000, "{rejected}");
    assert_eq!(
        rejected["error"]["message"],
        "Authentication required for the selected provider"
    );
    peer.send(3, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert_eq!(peer.response(3)["result"]["sessionId"], "session");
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_discovers_managed_auth_without_starting_an_existing_login() {
    for (capabilities, terminal_available) in [
        (json!({"auth":{"terminal":true}}), true),
        (json!({"_meta":{"terminal-auth":true}}), true),
        (
            json!({"auth":{"terminal":false},"_meta":{"terminal-auth":true}}),
            true,
        ),
        (json!({}), false),
        (json!({"terminal":true}), false),
        (json!({"_meta":{"terminal-auth":"true"}}), false),
    ] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec(); methods.push("account.providers"); wire.hello(&methods);
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([{"providerId":"managed-fixture","displayName":"Fixture","loginMethods":["browser","device"]}]));
        let status = wire.read("account.status"); wire.reply(&status,json!({"account":{"displayName":"fixture"}}));
        let status = wire.read("account.status"); wire.reply(&status,json!({"account":{"displayName":"fixture"}}));
        detached_eof(wire);
    });
        let mut peer = Peer::start(&home);
        peer.send(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":capabilities}),
        );
        let initialized = peer.response(1);
        let methods = initialized["result"]["authMethods"].as_array().unwrap();
        let agent = methods
            .iter()
            .find(|method| method.get("type").is_none())
            .unwrap()["id"]
            .clone();
        let terminal = methods.iter().find(|method| method["type"] == "terminal");
        assert_eq!(terminal.is_some(), terminal_available, "{initialized}");
        peer.send(2, "authenticate", json!({"methodId":agent}));
        assert!(peer.response(2).get("error").is_none());
        if let Some(terminal) = terminal {
            peer.send(3, "authenticate", json!({"methodId":terminal["id"]}));
            assert_eq!(peer.response(3)["error"]["code"], -32602);
        }
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_terminal_auth_helper_rediscovers_the_method_and_uses_the_same_home() {
    for directory in [Some("https://fixture.invalid/directory"), None] {
        let expected_directory = directory.unwrap_or("https://dimagent.com");
        let home = Home::new();
        let app_server = home.0.join("dim-app-server");
        fs::write(&app_server, "").unwrap();
        let endpoint = home.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = thread::spawn(move || {
            let providers = |wire: &mut Wire| {
                let request = wire.read("account.providers");
                wire.reply(
                &request,
                json!([{"providerId":"fixture","displayName":"Fixture","loginMethods":["device"]}]),
            );
            };
            let mut wire = accept_native(&listener);
            let mut methods = METHODS.to_vec();
            methods.extend([
                "account.providers",
                "provider.management",
                "account.login.begin",
            ]);
            wire.hello(&methods);
            providers(&mut wire);
            let status = wire.read("account.status");
            wire.reply(&status, json!({"account":null}));
            detached_eof(&mut wire);
            drop(wire);
            let mut wire = accept_native(&listener);
            wire.hello(&methods);
            providers(&mut wire);
            let listed = wire.read("provider.management");
            wire.reply(
                &listed,
                json!({"providers":[{"providerId":"fixture","displayName":"Fixture","account":{"namespace":"account","loginMethods":["device"]}}]}),
            );
            let begin = wire.read("account.login.begin");
            assert_eq!(begin["params"]["providerId"], "fixture");
            assert_eq!(begin["params"]["directory"], expected_directory);
            wire.reply(&begin,json!({"attemptId":"terminal-attempt","verificationUri":"https://fixture.invalid/device","userCode":"FIXTURE-CODE"}));
            let status = wire.read("account.login.status");
            assert_eq!(status["params"]["attemptId"], "terminal-attempt");
            wire.reply(&status, json!({"status":"completed"}));
            detached_eof(&mut wire);
        });
        let mut command = home.command();
        command.env("DIMCODE_NPM_PACKAGE", "dimagent").args([
            "acp",
            "--redirect-uri",
            "http://localhost:54322/auth/callback",
            "--server",
            app_server.to_str().unwrap(),
        ]);
        if let Some(directory) = directory {
            command.args(["--directory", directory]);
        }
        let mut peer = Peer::spawn(&mut command);
        peer.send(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{"_meta":{"terminal-auth":true}}}),
        );
        let initialized = peer.response(1);
        let method = initialized["result"]["authMethods"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let args: Vec<String> =
            serde_json::from_value(initialized["result"]["authMethods"][0]["args"].clone())
                .unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--directory", expected_directory])
        );
        assert_eq!(&args[args.len() - 2..], ["--auth-setup", &method]);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--redirect-uri", "http://localhost:54322/auth/callback"])
        );
        let server = fs::canonicalize(&app_server).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--server", server.to_str().unwrap()])
        );
        peer.close();
        let result = Command::new(env!("CARGO_BIN_EXE_dimcode"))
            .env_remove("DIMCODE_HOME")
            .env("DIMCODE_NPM_PACKAGE", "dimcode")
            .current_dir(&home.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("Signed in."));
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_model_choices_keep_the_catalog_provider_and_adopted_result() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec(); methods.extend(["session.methods","provider.catalog"]); wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[event(8,"configured","context","configured",json!({"provider_id":"provider/with/slashes","model_id":"native/model"}))],"through":8,"nextAfter":8,"done":true}));
        let methods = wire.read("session.methods"); wire.reply(&methods,json!({"methods":[{"name":"session.model.update","mutating":true}]}));
        let catalog = |wire: &mut Wire| {
            let list = wire.read("provider.list"); wire.reply(&list,json!({"providers":[{"providerId":"provider/with/slashes","enabled":true}]}));
            let read = wire.read("provider.catalog");
            assert_eq!(read["params"]["providerId"],"provider/with/slashes");
            wire.reply(&read,json!({"catalog":{"models":[{"id":"native/model","name":"First"},{"id":"next/model","name":"Next"}],"disabledModelIds":[]}}));
        };
        catalog(wire);
        let update = wire.read("session.model.update");
        assert_eq!(update["params"]["providerId"],"provider/with/slashes");
        assert_eq!(update["params"]["modelId"],"next/model");
        wire.reply(&update,json!({"outcome":"adopted","model":{"providerId":"provider/with/slashes","modelId":"next/model"}}));
        catalog(wire);
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    let created = peer.response(2);
    assert_eq!(
        created["result"]["configOptions"][0]["currentValue"], "native/model",
        "{created}"
    );
    peer.send(
        3,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"model","value":"next/model"}),
    );
    assert_eq!(
        peer.response(3)["result"]["configOptions"][0]["currentValue"],
        "next/model"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_lody_snapshots_background_lifecycle_and_terminal_fork_use_real_facts() {
    for version in [0, 2, 1] {
        lody_projection(version);
    }
}

fn lody_projection(version: u32) {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let daemon = home.daemon(move |wire| {
        let mut methods = METHODS.to_vec(); methods.push("session.fork"); wire.hello(&methods); wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let read = wire.read("session.history"); wire.reply(&read,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let facts = vec![
            event(9,"start","turn","start",json!({"turn_id":"turn"})),
            event(10,"request-a","collaboration","request",json!({"turn_id":"turn","request_id":"model","call_id":"a","target":{"kind":"member","agent_id":"member-agent","session_id":"member-session"},"prompt":"A"})),
            event(11,"delivery-a","collaboration","delivered",json!({"op_event_id":"request-a","status":"ok","agent_id":"member-agent","session_id":"member-session","input_event_id":"a-input"})),
            event(12,"result-a","collaboration","result",json!({"request_event_id":"request-a","agent_id":"member-agent","session_id":"member-session","text":"A done","status":"completed"})),
            event(13,"request-b","collaboration","request",json!({"turn_id":"turn","request_id":"model","call_id":"b","target":{"kind":"member","agent_id":"member-agent","session_id":"member-session"},"prompt":"B"})),
            event(14,"delivery-b","collaboration","delivered",json!({"op_event_id":"request-b","status":"ok","agent_id":"member-agent","session_id":"member-session","input_event_id":"b-input"})),
            event(15,"end","turn","end",json!({"turn_id":"turn","status":"completed","reason":"end_turn"})),
            event(16,"background","exec","started",json!({"turn_id":"turn","request_id":"model","call_id":"exec","task_id":"background","description":"Detached command","output_path":"fixture-output"})),
            event(17,"compacting","context","compaction-start",json!({"through":16,"trigger":"manual","strategy":"fixture"})),
            event(18,"compact-cancelled","context","compaction",json!({"through":16,"start":17,"status":"cancelled","reason":"Stopped by owner"})),
            event(19,"background-ended","exec","ended",json!({"task_id":"background","status":"cancelled","result":{"content":"Native command was cancelled","isError":true}})),
        ];
        for fact in &facts { wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":fact})); }
        for _ in 0..2 {
            let list = wire.read("session.list"); wire.reply(&list,json!([{"sessionId":"session","cwd":cwd}]));
            let history = wire.read("session.history"); wire.reply(&history,json!({"events":facts,"through":19,"nextAfter":19,"done":true}));
        }
        let fork = wire.read("session.fork"); assert_eq!(fork["params"]["at"],15); wire.reply(&fork,json!({"sessionId":"forked","sequence":15}));
        let attach = wire.read("session.attach"); assert_eq!(attach["params"]["sessionId"],"forked"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history"); wire.reply(&history,json!({"events":[],"through":15,"nextAfter":15,"done":true}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    let capabilities = if version == 0 {
        json!({})
    } else {
        json!({"_meta":{"lody":{"subagentEvents":{"version":version}}}})
    };
    peer.send(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":capabilities}),
    );
    let initialize = peer.response(1);
    assert_eq!(
        initialize["result"]["agentCapabilities"]["_meta"]["lody"]["tasks"]["background"],
        true
    );
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "_lody/session/history/read",
        json!({"sessionId":"session"}),
    );
    assert_eq!(peer.response(3)["result"], json!({}));
    let snapshots: Vec<_> = peer
        .updates
        .iter()
        .filter(|frame| frame["method"] == "_lody/subagents/event")
        .collect();
    assert_eq!(
        snapshots.len(),
        if version == 1 { 5 } else { 0 },
        "replay cannot duplicate observed delegation transitions: {snapshots:?}"
    );
    if version == 1 {
        assert_ne!(
            snapshots[0]["params"]["runId"],
            snapshots[3]["params"]["runId"]
        );
        assert_eq!(
            snapshots[0]["params"]["snapshot"]["support"],
            json!({"stream":[],"progress":false,"outputRead":"none","cancel":false})
        );
        assert_eq!(snapshots[2]["params"]["snapshot"]["state"], "completed");
    }
    let task = peer
        .updates
        .iter()
        .find(|frame| frame["params"]["update"]["_meta"]["lody"]["task"]["status"] == "in_progress")
        .unwrap();
    assert_ne!(
        task["params"]["update"]["toolCallId"],
        task["params"]["update"]["_meta"]["lody"]["task"]["parentToolCallId"]
    );
    assert!(peer.updates.iter().any(
        |frame| frame["params"]["update"]["_meta"]["lody"]["activity"]["failureReason"]
            == "Cancelled: Stopped by owner"
    ));
    peer.send(4,"session/fork",json!({"sessionId":"session","cwd":home.0,"mcpServers":[{"type":"http","name":"client","url":"http://127.0.0.1:9/mcp","headers":[]}],"_meta":{"lody":{"forkAtTurn":{"version":1,"turnId":"turn"}}}}));
    assert_eq!(peer.response(4)["result"]["sessionId"], "forked");
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_lody_startup_model_is_selected_before_creation_without_shared_writes() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.push("provider.catalog");
        wire.hello(&methods);
        let list = wire.read("provider.list");
        wire.reply(
            &list,
            json!({"providers":[{"providerId":"native-provider","enabled":true}]}),
        );
        let catalog = wire.read("provider.catalog");
        wire.reply(
            &catalog,
            json!({"catalog":{"models":[{"id":"chosen","name":"Chosen"}],"disabledModelIds":[]}}),
        );
        let create = wire.created();
        assert_eq!(
            create["params"]["model"],
            json!({"provider":"native-provider","modelId":"chosen"})
        );
        assert_eq!(
            create["params"]["permissionPolicy"],
            json!({"preset":"workspace-write"})
        );
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2,"session/new",json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"modelId":"chosen","configOptionValues":{"model":"ignored","mode":"workspace-write"}}}}}));
    assert_eq!(peer.response(2)["result"]["sessionId"], "session");
    peer.send(3,"session/resume",json!({"sessionId":"session","cwd":home.0,"_meta":{"lody":{"sessionConfig":{"version":1,"configOptionValues":{"model":"chosen"}}}}}));
    assert_eq!(
        peer.response(3)["error"]["data"]["code"],
        "unsupported_control"
    );
    peer.send(4,"session/new",json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":2,"configOptionValues":{}}}}}));
    assert!(peer.response(4).get("error").is_some());
    for (index, mode) in [json!(true), json!("unknown"), Value::Null]
        .into_iter()
        .enumerate()
    {
        let id = index as i64 + 5;
        peer.send(id,"session/new",json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"configOptionValues":{"mode":mode}}}}}));
        assert_eq!(peer.response(id)["error"]["code"], -32602);
    }
    for (index, method) in ["session/load", "session/resume", "session/fork"]
        .into_iter()
        .enumerate()
    {
        let id = index as i64 + 8;
        peer.send(id,method,json!({"sessionId":"session","cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"configOptionValues":{"mode":"workspace-write"}}}}}));
        let response = peer.response(id);
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["data"]["code"], "unsupported_control");
    }
    peer.send(11,"session/new",json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"configOptionValues":{"reasoning_effort":"high"}}}}}));
    let response = peer.response(11);
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(
        response["error"]["data"]["field"],
        "sessionConfig.configOptionValues.reasoning_effort"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reasoning_effort")
    );
    for (index, field) in ["steer", "goalControl", "worktreeProject"]
        .into_iter()
        .enumerate()
    {
        let id = index as i64 + 12;
        peer.send(
            id,
            "session/new",
            json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{field:{"version":1}}}}),
        );
        let response = peer.response(id);
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["data"]["field"], field);
    }
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_lody_startup_mode_is_applied_before_creation_without_shared_writes() {
    for mode in ["read-only", "workspace-write", "full-access"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let mut methods = METHODS.to_vec();
            methods.extend(["session.permission.get", "session.permission.update"]);
            wire.hello(&methods);
            let create = wire.created();
            assert_eq!(create["params"]["permissionPolicy"], json!({"preset":mode}));
            assert!(create["params"].get("model").is_none());
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let policy = wire.read("session.permission.get");
            wire.reply(
                &policy,
                json!({"version":1,"policy":create["params"]["permissionPolicy"]}),
            );
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2,"session/new",json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"configOptionValues":{"mode":mode}}}}}));
        let response = peer.response(2);
        assert_eq!(response["result"]["sessionId"], "session", "{response}");
        assert_eq!(response["result"]["modes"]["currentModeId"], mode);
        let option = response["result"]["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["id"] == "mode")
            .unwrap();
        assert_eq!(option["currentValue"], mode);
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
#[ignore = "requires Node and the workspace MCP SDK"]
fn acp_stdio_temporary_mcp_calls_tools_and_settles_the_child_on_eof() {
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = String::from_utf8(node.stdout).unwrap().trim().to_owned();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../dim-app-server/tests/fixtures/mcp-stdio.cjs")
        .canonicalize()
        .unwrap();
    let pid_file = home.0.join("mcp.pid");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let model = thread::spawn(move || {
        for index in 0..3 {
            let deadline = Instant::now() + PATIENCE;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "The model request did not arrive"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut wire = BufReader::new(stream);
            let mut line = String::new();
            wire.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /v1/chat/completions "));
            let mut length = 0;
            loop {
                line.clear();
                wire.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            wire.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            let (delta, finish) = if index == 1 {
                let messages = request["messages"].to_string();
                assert!(
                    messages.contains("1709") && messages.contains("fixture-env"),
                    "The actual MCP result must reach the model"
                );
                (json!({"role":"assistant","content":"ACP_MCP_OK"}), "stop")
            } else {
                let tool = request["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|tool| {
                        let function = &tool["function"];
                        function["name"]
                            .as_str()
                            .is_some_and(|name| name.starts_with("mcp_temporary_"))
                            && function["parameters"]["properties"].get("a").is_some()
                                == (index == 0)
                    })
                    .expect("Temporary MCP tools must be present in the first model request");
                (
                    json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("call-{index}"),"type":"function","function":{"name":tool["function"]["name"],"arguments":if index==0 {"{\"a\":17,\"b\":9}"}else{"{}"}}}]}),
                    "tool_calls",
                )
            };
            let chunk = |delta: Value, finish: Value| {
                json!({"id":"mcp-model","object":"chat.completion.chunk","model":"fixture-model","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}).to_string()
            };
            let data = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                chunk(delta, Value::Null),
                chunk(json!({}), json!(finish))
            );
            write!(wire.get_mut(),"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len()).unwrap();
            wire.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config,json!({"defaultModel":{"providerId":"local","modelId":"fixture-model"},"providers":[{"id":"local","kind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":256}}]}]}).to_string()).unwrap();
    let mut command = home.command();
    command.args(["acp", "--model-config", config.to_str().unwrap()]);
    let mut peer = Peer::spawn(&mut command);
    peer.initialize();
    peer.send(2,"session/new",json!({"cwd":home.0,"mcpServers":[{"name":"client","command":node,"args":[fixture,"normal","fixture-argument"],"env":[{"name":"MCP_PID_FILE","value":pid_file},{"name":"MCP_TEST_VALUE","value":"fixture-env"}]}]}));
    let created = peer.response(2);
    assert!(created.get("error").is_none(), "{created}");
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    peer.send(
        3,
        "session/set_mode",
        json!({"sessionId":id,"modeId":"full-access"}),
    );
    let mode = peer.response(3);
    assert!(mode.get("error").is_none(), "{mode}");
    peer.send(
        4,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"Call inspect"}]}),
    );
    let response = peer.response(4);
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    assert!(
        peer.updates
            .iter()
            .any(|update| update["params"]["update"]["content"]["text"] == "ACP_MCP_OK")
    );
    peer.send(
        5,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"Call slow"}]}),
    );
    let deadline = Instant::now() + PATIENCE;
    while !pid_file.with_extension("pid.called").exists() {
        assert!(Instant::now() < deadline, "The slow MCP call did not start");
        thread::sleep(Duration::from_millis(10));
    }
    peer.close();
    model.join().unwrap();
    assert!(
        pid_file.with_extension("pid.cancelled").exists(),
        "EOF must cancel the accepted MCP call before closing its server"
    );
    let pid: i32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    let deadline = Instant::now() + PATIENCE;
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "Temporary MCP child outlived Gateway EOF"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!home.0.join("mcp.json").exists());
    assert!(!home.0.join(".mcp.json").exists());
}

#[test]
fn acp_stdio_runs_against_the_production_daemon_and_a_local_model() {
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let model = thread::spawn(move || {
        for (prompt, response) in [
            ("ACP_REAL_LIMIT", "ACP_PARTIAL"),
            ("ACP_REAL_REQUEST", "ACP_REAL_OK"),
            ("ACP_REAL_FOLLOWUP", "ACP_REAL_FOLLOWUP_OK"),
            ("ACP_NEW_REQUEST", "ACP_NEW_OK"),
            ("CLI_REAL_LIMIT", "CLI_PARTIAL"),
        ] {
            let deadline = Instant::now() + PATIENCE;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "The production daemon did not request the model"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /v1/chat/completions "), "{line}");
            let mut length = 0;
            loop {
                line.clear();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["model"], "fixture-model");
            assert!(
                request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "user"
                        && message["content"].to_string().contains(prompt))
            );
            if prompt == "ACP_REAL_FOLLOWUP" {
                assert!(
                    request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| message["role"] == "assistant"
                            && message["content"].to_string().contains("ACP_REAL_OK")),
                    "A follow-up must retain the Session's earlier reply"
                );
                assert!(request["messages"].to_string().contains("ACP_REAL_REQUEST"));
            } else if prompt == "ACP_NEW_REQUEST" {
                let messages = request["messages"].to_string();
                assert!(
                    !messages.contains("ACP_REAL_REQUEST")
                        && !messages.contains("ACP_REAL_FOLLOWUP")
                        && !messages.contains("ACP_REAL_OK"),
                    "A new Session must not inherit the other Session's context"
                );
            }
            let chunk = |delta: Value, finish: Value| {
                json!({"id":"fixture-response","object":"chat.completion.chunk","model":"fixture-model","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}).to_string()
            };
            let data = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                chunk(json!({"role":"assistant","content":response}), Value::Null),
                chunk(
                    json!({}),
                    json!(if prompt.ends_with("_LIMIT") {
                        "length"
                    } else {
                        "stop"
                    })
                )
            );
            write!(stream.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config, json!({"defaultModel":{"providerId":"local","modelId":"fixture-model"},"providers":[{"id":"local","kind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":256}}]}]}).to_string()).unwrap();
    let mut command = home.command();
    command
        .args(["--model-config", config.to_str().unwrap(), "acp"])
        .env("ACP_STICKY_SESSION", "true")
        .env_remove("RUST_LOG");
    let mut peer = Peer::spawn(&mut command);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    let created = peer.response(2);
    assert!(created.get("error").is_none(), "{created}");
    let default_mode = created["result"]["modes"]["currentModeId"].clone();
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    peer.send(
        30,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"ACP_REAL_LIMIT"}]}),
    );
    let limited = peer.response(30);
    assert_eq!(limited["result"]["stopReason"], "max_tokens", "{limited}");
    peer.updates.clear();
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"ACP_REAL_REQUEST"}]}),
    );
    let response = peer.response(3);
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let text: String = peer
        .updates
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .map(|frame| {
            frame["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(text, "ACP_REAL_OK");
    peer.updates.clear();
    peer.send(
        4,
        "session/prompt",
        json!({"sessionId":id,"prompt":[{"type":"text","text":"ACP_REAL_FOLLOWUP"}]}),
    );
    assert_eq!(peer.response(4)["result"]["stopReason"], "end_turn");
    assert!(
        peer.updates
            .iter()
            .any(|frame| frame["params"]["update"]["content"]["text"] == "ACP_REAL_FOLLOWUP_OK")
    );
    let mut options: serde_json::Map<String, Value> = created["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| {
            (
                option["id"].as_str().unwrap().to_owned(),
                option["currentValue"].clone(),
            )
        })
        .collect();
    let startup_mode = created["result"]["modes"]["availableModes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mode| mode["id"] != default_mode)
        .unwrap()["id"]
        .clone();
    options.insert("mode".into(), startup_mode.clone());
    peer.send(5,"session/new",json!({"cwd":home.0,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"configOptionValues":options}}}}));
    let other = peer.response(5);
    assert!(other.get("error").is_none(), "{other}");
    assert_eq!(other["result"]["modes"]["currentModeId"], startup_mode);
    let other_id = other["result"]["sessionId"].as_str().unwrap();
    assert_ne!(other_id, id);
    peer.send(
        6,
        "session/prompt",
        json!({"sessionId":other_id,"prompt":[{"type":"text","text":"ACP_NEW_REQUEST"}]}),
    );
    assert_eq!(peer.response(6)["result"]["stopReason"], "end_turn");
    assert!(
        peer.updates
            .iter()
            .any(|frame| frame["params"]["update"]["content"]["text"] == "ACP_NEW_OK")
    );
    peer.updates.clear();
    peer.send(7, "session/resume", json!({"sessionId":id,"cwd":home.0}));
    assert_eq!(
        peer.response(7)["result"]["modes"]["currentModeId"],
        default_mode
    );
    assert!(
        !peer.updates.iter().any(|frame| matches!(
            frame["params"]["update"]["sessionUpdate"].as_str(),
            Some("agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk")
        )),
        "resume may refresh metadata but cannot replay old messages"
    );
    peer.send(8, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert_eq!(
        peer.response(8)["result"]["modes"]["currentModeId"],
        default_mode,
        "Startup mode must not change the shared default"
    );
    peer.close();
    let limited = home
        .command()
        .env_remove("RUST_LOG")
        .args([
            "--model-config",
            config.to_str().unwrap(),
            "exec",
            "-p",
            "CLI_REAL_LIMIT",
        ])
        .output()
        .unwrap();
    assert_eq!(limited.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&limited.stderr);
    assert!(
        stderr.contains("local/fixture-model stopped at max_tokens"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Increase the output token limit and retry"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("[failed]").count(), 1, "{stderr}");
    let log = fs::read_to_string(home.0.join("rust/daemon.log")).unwrap();
    assert!(
        log.contains("Model request failed")
            && log.contains("max_tokens")
            && log.contains("fixture-model"),
        "{log}"
    );
    model.join().unwrap();
    let shutdown = home.command().args(["daemon", "stop"]).output().unwrap();
    assert!(
        shutdown.status.success(),
        "{}",
        String::from_utf8_lossy(&shutdown.stderr)
    );
}

#[test]
fn acp_stdio_recognizes_max_tokens_before_a_late_submit_reply() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let submit = wire.read("session.submit");
        let input = event(9, "limited-input", "input", "message", json!({"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"Hello"}]},"mode":"queue"}));
        let notice = |wire: &mut Wire, value: Value| wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":value}));
        notice(wire, event(10,"start","turn","start",json!({"turn_id":"limited"})));
        notice(wire, event(11,"consumed","input","consumed",json!({"input_id":"limited-input"})));
        notice(wire, event(12,"model-end","step","model-output-end",json!({"turn_id":"limited","request_id":"model","status":"failed","error":"opaque internal detail","presentation":{"source":"fixture","reason":"max_tokens","action":"Increase the output token limit and retry."}})));
        notice(wire, event(13,"end","turn","end",json!({"turn_id":"limited","status":"failed","error":"opaque internal detail"})));
        wire.reply(&submit, json!({"event":input}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert_eq!(peer.response(2)["result"]["sessionId"], "session");
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Hello"}]}),
    );
    let limited = peer.response(3);
    assert_eq!(limited["result"]["stopReason"], "max_tokens", "{limited}");
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_correlates_the_consumed_input_and_deduplicates_unicode_progress() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        assert_eq!(hello["params"]["clientKind"], "other");
        assert_eq!(hello["params"]["clientName"], "AcpGateway");
        let mut methods = vec!["session.create", "session.history"];
        methods.extend_from_slice(METHODS);
        wire.reply(&hello, json!({"instanceId":"fixture","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":methods}));
        let create = wire.read("session.create");
        wire.reply(&create, json!({"sessionId":"session","sequence":7,"model":{"providerId":"fixture","modelId":"fixture"}}));
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":7,"nextAfter":7,"done":true}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["mode"], "queue");
        let input = event(9, "owned-input", "input", "message", json!({"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"Hello"}]},"blocks":[],"mode":"queue"}));
        let notice = |wire: &mut Wire, value: Value| wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":value}));
        notice(wire, event(8,"other-start","turn","start",json!({"turn_id":"other"})));
        notice(wire, input.clone());
        notice(wire, event(10,"other-end","turn","end",json!({"turn_id":"other","status":"completed","reason":"end_turn"})));
        wire.reply(&submit, json!({"session_id":"session","event":input}));
        notice(wire, event(11,"owned-start","turn","start",json!({"turn_id":"owned"})));
        notice(wire, event(12,"consumed","input","consumed",json!({"input_id":"owned-input"})));
        notice(wire, event(13,"model-start","step","model-start",json!({"turn_id":"owned","request_id":"model"})));
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","text":"A🦀","textOffset":0,"thinking":"","thinkingOffset":0}));
        notice(wire, event(14,"text-a","step","text",json!({"turn_id":"owned","request_id":"model","delta":"A"})));
        wire.notice("session.model.progress", json!({"sessionId":"session","branchId":"main","requestId":"model","text":"🦀B","textOffset":1,"thinking":"","thinkingOffset":0}));
        notice(wire, event(15,"text-b","step","text",json!({"turn_id":"owned","request_id":"model","delta":"🦀B"})));
        notice(wire, event(16,"owned-end","turn","end",json!({"turn_id":"owned","status":"completed","reason":"end_turn"})));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert_eq!(peer.response(2)["result"]["sessionId"], "session");
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Hello"}]}),
    );
    let response = peer.response(3);
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let text: String = peer
        .updates
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .map(|frame| {
            frame["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(text, "A🦀B");
    assert!(
        !peer
            .updates
            .iter()
            .any(|frame| frame["params"]["update"]["sessionUpdate"] == "user_message_chunk")
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_load_coalesces_fixed_head_replay_and_rejects_unsupported_inputs() {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let list = wire.read("session.list");
        wire.reply(
            &list,
            json!([{"sessionId":"saved","cwd":cwd,"title":"Saved title","titleRevision":2}]),
        );
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let first = wire.read("session.history");
        assert!(first["params"].get("through").is_none());
        wire.reply(
            &first,
            json!({"events":[],"through":8,"nextAfter":3,"done":false}),
        );
        let second = wire.read("session.history");
        assert_eq!(second["params"]["through"], 8);
        assert_eq!(second["params"]["after"], 3);
        let text = event(
            8,
            "answer",
            "step",
            "text",
            json!({"turn_id":"turn","request_id":"request","delta":"saved answer"}),
        );
        wire.notice(
            "session.event",
            json!({"sessionId":"saved","branchId":"main","event":text}),
        );
        wire.reply(
            &second,
            json!({"events":[text],"through":8,"nextAfter":8,"done":true}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/new",
        json!({"cwd":home.0,"mcpServers":[],"additionalDirectories":[home.0]}),
    );
    assert_eq!(
        peer.response(2)["error"]["data"]["code"],
        "unsupported_additional_directories"
    );
    peer.send(
        3,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[{"type":"http","name":"client","url":"http://127.0.0.1:9/mcp","headers":[]}]}),
    );
    peer.send(
        4,
        "session/resume",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[{"name":"client","command":"unused-client-mcp","args":[],"env":[]}]}),
    );
    assert!(peer.response(3).get("error").is_none());
    assert!(peer.response(4).get("error").is_none());
    assert_eq!(
        peer.updates.len(),
        2,
        "live copy of replayed fact must be suppressed"
    );
    assert_eq!(
        peer.updates[0]["params"]["update"]["content"]["text"],
        "saved answer"
    );
    assert_eq!(peer.updates[1]["params"]["update"]["title"], "Saved title");
    peer.send(5, "session/prompt", json!({"sessionId":"saved","prompt":[{"type":"audio","data":"AA==","mimeType":"audio/wav"}]}));
    assert!(peer.response(5).get("error").is_some());
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn tui_reports_max_tokens_action_and_accepts_the_next_prompt() {
    for same_failure in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[
            {"id":"limited-model","sequence":9,"create_at":1,"type":"step","name":"model-output-end","data":{"turn_id":"limited","request_id":"limited-request","status":"failed","error":"private model detail","presentation":{"source":"local","reason":"max_tokens","serverMessage":"local/model stopped at max_tokens","action":"Increase the output token limit and retry"}}},
            {"id":"limited-turn","sequence":10,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"limited","status":"failed","error":if same_failure { "private model detail" } else { "independent turn failure" }}}
        ],"through":10,"nextAfter":10,"done":true}));
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.reply(&request, Value::Null),
                method => panic!("unexpected request {method:?}"),
            }
        }
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "try again");
        wire.reply(&submit, json!({"event":{}}));
        wire.event(11, "text", json!({"turn_id":"next","request_id":"next-request","delta":"the next turn answers"}));
        wire.end(json!({"turn_id":"next","status":"completed","reason":"fixture"}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
        let mut terminal = Terminal::launch_tui(home.command());
        terminal.until("Increase the output token");
        terminal.until("limit and retry");
        assert!(!terminal.transcript.contains("private model detail"));
        if !same_failure {
            terminal.until("independent turn failure");
        }
        terminal.write(b"try again\r");
        terminal.until("the next turn answers");
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
        if same_failure {
            assert!(!terminal.transcript.contains("turn failed:"));
        }
        assert!(!terminal.transcript.contains("private model detail"));
    }
}

#[test]
fn acp_stdio_member_approval_shows_the_original_action_and_rechecks_before_allowing() {
    for (failure, recheck) in [
        (None, false),
        (Some(-32601), false),
        (Some(-32000), false),
        (Some(-32000), true),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        let mut methods = METHODS.to_vec(); methods.push("session.collaboration.approval.get"); wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach, json!({}));
        let history = wire.read("session.history"); wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let submit = wire.read("session.submit");
        let input = event(9,"input","input","message",json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}));
        wire.reply(&submit,json!({"event":input}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(10,"start","turn","start",json!({"turn_id":"turn"}))}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(11,"consumed","input","consumed",json!({"input_id":"input"}))}));
        wire.notice("approval.request", json!({"sessionId":"session","branchId":"review","requestId":"outer","call":{"name":"member_approval_allow","input":{"approvalId":"route"}},"reason":"Leader admission"}));
        let detail = wire.read("session.collaboration.approval.get");
        assert_eq!(detail["params"], json!({"sessionId":"session","branchId":"review","requestId":"outer"}));
        if let Some(code) = failure.filter(|_| !recheck) {
            wire.send(json!({"jsonrpc":"2.0","id":detail["id"],"error":{"code":code,"message":"Details unavailable","data":{"appError":"service_reloading"}}}));
        } else {
        wire.reply(&detail, json!({"state":"pending","approvalId":"route","descriptor":{"delegationId":"delegation","member":{"agentId":"reviewer","sessionId":"member","branchId":"main"},"requestId":"original","call":{"name":"exec","input":{"command":"cargo test original action"}},"reason":"Member process request"}}));
        let detail = wire.read("session.collaboration.approval.get");
        if let Some(code) = failure {
            wire.send(json!({"jsonrpc":"2.0","id":detail["id"],"error":{"code":code,"message":"Details unavailable","data":{"appError":"service_reloading"}}}));
        } else {
            wire.reply(&detail, json!({"state":"ended"}));
        }
        }
        let response = wire.read("approval.respond");
        assert_eq!(response["params"], json!({"sessionId":"session","branchId":"review","requestId":"outer","decision":"deny"}));
        wire.reply(&response, json!({}));
        wire.notice("approval.resolved", json!({"sessionId":"session","branchId":"review","requestId":"outer","decision":"deny"}));
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(12,"end","turn","end",json!({"turn_id":"turn","status":"completed","reason":"end_turn"}))}));
        detached_eof(wire);
    });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        assert!(peer.response(2).get("error").is_none());
        peer.send(
            3,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"work"}]}),
        );
        loop {
            let frame = peer.frame();
            if frame["method"] != "session/request_permission" {
                continue;
            }
            let _: acp::RequestPermissionRequest =
                serde_json::from_value(frame["params"].clone()).unwrap();
            if failure.is_some() && frame["params"]["options"].as_array().unwrap().len() == 1 {
                assert!(
                    frame["params"]["toolCall"]["title"]
                        .as_str()
                        .unwrap()
                        .contains("Cannot review")
                );
                assert_eq!(
                    frame["params"]["options"],
                    json!([{"optionId":"reject_once","name":"Reject once","kind":"reject_once"}])
                );
                peer.answer(
                    &frame,
                    json!({"outcome":{"outcome":"selected","optionId":"reject_once"}}),
                );
                break;
            }
            assert_eq!(
                frame["params"]["toolCall"]["rawInput"]["member"]["sessionId"],
                "member"
            );
            assert!(
                frame["params"]["toolCall"]["content"][0]["content"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("cargo test original action")
            );
            assert_eq!(frame["params"]["options"].as_array().unwrap().len(), 2);
            peer.answer(
                &frame,
                json!({"outcome":{"outcome":"selected","optionId":"allow_once"}}),
            );
            if !recheck {
                break;
            }
        }
        assert_eq!(peer.response(3)["result"]["stopReason"], "end_turn");
        peer.close();
        daemon.join().unwrap();
    }
}

/// Appendix D 7: a blocked prompt ends the ACP turn with `refusal`, and the check's
/// message is the text the user sees.
#[test]
fn acp_stdio_reports_a_blocked_prompt_as_refusal_with_the_reason() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let submit = wire.read("session.submit");
        wire.send(
            json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,
            "message":"Hook user: no prompts today","data":{"appError":"input_blocked"}}}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Prompt"}]}),
    );
    let response = peer.response(3);
    assert_eq!(
        response["result"]["stopReason"],
        "refusal",
        "{response}: {updates:?}",
        updates = peer.updates
    );
    assert!(
        peer.updates.iter().any(|frame| {
            frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                && frame["params"]["update"]["content"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("Hook user: no prompts today"))
        }),
        "the block's message is text: {:?}",
        peer.updates
    );
    peer.close();
    daemon.join().unwrap();
}
