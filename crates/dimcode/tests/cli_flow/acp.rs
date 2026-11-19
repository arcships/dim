use super::*;
use agent_client_protocol_schema::v1 as acp;
use std::{net::Shutdown, os::fd::OwnedFd};

const METHODS: &[&str] = &[
    "session.attach",
    "session.attach.expectedCwd",
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
        let Some(line) = wire.trailing_request() else {
            return;
        };
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
    stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
    Wire::new(stream)
}

/// A title request carries no tools, only the system text and the input
/// (`docs/design/plugins/SessionTitle.md`); any other request is a prompt.
fn title_request_input(request: &Value) -> Option<String> {
    if request["tools"].as_array().map_or(0, Vec::len) != 0 {
        return None;
    }
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "a title request carries only its system text and the input: {request}"
    );
    assert_eq!(messages[0]["role"], "system", "{request}");
    assert_eq!(messages[1]["role"], "user", "{request}");
    Some(
        messages[1]["content"]
            .as_str()
            .expect("the title request's user message is the input text")
            .to_owned(),
    )
}

fn recovery_hello(wire: &mut Wire, instance: &str) {
    let hello = wire.read("hello");
    assert_eq!(hello["params"]["clientName"], "AcpGateway");
    wire.2 = true;
    wire.reply(&hello,json!({"instanceId":instance,"serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.input_lookup","session.methods","session.attach.expectedCwd","session.turn.get.inputId"]}));
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

fn command_methods(wire: &mut Wire, names: &[&str]) {
    let methods = wire.read("session.methods");
    wire.reply(&methods, json!({"methods":names.iter().map(|name| json!({"name":name,"mutating":!name.ends_with(".get") && !name.ends_with(".list")})).collect::<Vec<_>>()}));
}

fn command_session(wire: &mut Wire, names: &[&str]) {
    let attach = wire.read("session.attach");
    wire.reply(&attach, json!({}));
    let history = wire.read("session.history");
    wire.reply(
        &history,
        json!({"events":[],"through":8,"nextAfter":8,"done":true}),
    );
    command_methods(wire, names);
}

impl Peer {
    fn command(&mut self, id: i64, session: &str, text: &str) -> String {
        self.updates.clear();
        self.send(
            id,
            "session/prompt",
            json!({"sessionId":session,"prompt":[{"type":"text","text":text}]}),
        );
        let response = self.response(id);
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        self.updates
            .iter()
            .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
            .map(|frame| {
                frame["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap()
            })
            .collect()
    }

    fn commands(&self) -> Vec<String> {
        let update = self
            .updates
            .iter()
            .rev()
            .find(|frame| frame["params"]["update"]["sessionUpdate"] == "available_commands_update")
            .expect("commands are published on establishment and configuration changes");
        let _: acp::SessionNotification = serde_json::from_value(update["params"].clone()).unwrap();
        update["params"]["update"]["availableCommands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|command| command["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn acp_cancelling_a_command_read_prevents_its_following_mutation() {
    for (command, read_method, methods, result) in [
        (
            "/goal set Cancelled objective",
            "session.goal.get",
            vec![
                "session.goal.get",
                "session.goal.set",
                "session.goal.edit",
                "session.goal.pause",
                "session.goal.resume",
                "session.goal.clear",
            ],
            json!({"branchId":"main","through":8,"goal":null}),
        ),
        (
            "/loop stop target",
            "session.loop.list",
            vec![
                "session.loop.list",
                "session.loop.start",
                "session.loop.stop",
            ],
            json!({"loops":[{"loopId":"target","state":"running","round":1}]}),
        ),
    ] {
        let home = Home::new();
        let (accepted, waiting) = std::sync::mpsc::sync_channel(0);
        let (release, delayed) = std::sync::mpsc::sync_channel(0);
        let daemon = home.daemon(move |wire| {
            wire.hello(&[METHODS, &["session.methods"]].concat());
            wire.created();
            command_session(wire, &methods);
            let read = wire.read(read_method);
            accepted.send(()).unwrap();
            delayed.recv_timeout(PATIENCE).unwrap();
            wire.reply(&read, result.clone());
            let read = wire.read(read_method);
            wire.reply(&read, result);
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        assert!(peer.response(2).get("error").is_none());
        peer.send(
            3,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":command}]}),
        );
        waiting.recv_timeout(PATIENCE).unwrap();
        writeln!(
            peer.input,
            "{}",
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session"}})
        )
        .unwrap();
        peer.input.flush().unwrap();
        peer.send(
            4,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"busy probe"}]}),
        );
        assert_eq!(peer.response(4)["error"]["data"]["code"], "session_busy");
        release.send(()).unwrap();
        assert_eq!(peer.response(3)["result"]["stopReason"], "cancelled");
        let text = peer.command(5, "session", command.split_once(' ').unwrap().0);
        assert!(
            text.contains("No goal") || text.contains("target · running"),
            "{text}"
        );
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_closing_a_session_settles_its_command_before_the_account_reply() {
    for notice in [false, true] {
        let home = Home::new();
        let (accepted, waiting) = std::sync::mpsc::sync_channel(0);
        let (release, delayed) = std::sync::mpsc::sync_channel(0);
        let daemon = home.daemon(move |wire| {
            wire.hello(
                &[
                    METHODS,
                    &[
                        "session.methods",
                        "account.providers",
                        "account.status",
                        "account.usage",
                    ],
                ]
                .concat(),
            );
            let providers = json!([{"providerId":"test","displayName":"Test","loginMethods":[]}]);
            let discovery = wire.read("account.providers");
            wire.reply(&discovery, providers.clone());
            let status = wire.read("account.status");
            wire.reply(&status, json!({"account":{}}));
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(
                &attach,
                json!({"model":{"providerId":"test","modelId":"model"}}),
            );
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            command_methods(wire, &[]);
            let discovery = wire.read("account.providers");
            wire.reply(&discovery, providers);
            let status = wire.read("account.status");
            wire.reply(&status, json!({"account":{}}));
            let usage = wire.read("account.usage");
            accepted.send(()).unwrap();
            let close = wire.read("session.close");
            if notice {
                wire.notice("session.closed", json!({"sessionId":"session"}));
            }
            wire.reply(&close, json!({}));
            delayed.recv_timeout(PATIENCE).unwrap();
            wire.reply(
                &usage,
                json!({"plan":{"name":"late usage must not appear"}}),
            );
            let list = wire.read("session.list");
            wire.reply(&list, json!([]));
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        assert!(peer.response(2).get("error").is_none());
        peer.updates.clear();
        peer.send(
            3,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"/usage"}]}),
        );
        waiting.recv_timeout(PATIENCE).unwrap();
        peer.send(4, "session/close", json!({"sessionId":"session"}));
        assert_eq!(peer.response(3)["result"]["stopReason"], "cancelled");
        assert!(peer.response(4).get("error").is_none());
        release.send(()).unwrap();
        peer.send(5, "session/list", json!({}));
        assert!(peer.response(5).get("error").is_none());
        assert!(
            !peer
                .updates
                .iter()
                .any(|frame| frame.to_string().contains("late usage"))
        );
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_goal_commands_read_the_owner_and_preserve_paused_shorthand_across_load() {
    const GOAL_METHODS: &[&str] = &[
        "session.goal.get",
        "session.goal.set",
        "session.goal.edit",
        "session.goal.pause",
        "session.goal.resume",
        "session.goal.clear",
    ];
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[METHODS, &["session.methods"]].concat());
        wire.created();
        command_session(wire, GOAL_METHODS);
        let mut goal = Value::Null;
        for (method, objective, status) in [
            ("session.goal.set", Some("Ship safely"), Some("active")),
            ("session.goal.pause", None, Some("paused")),
            ("session.goal.edit", Some("Ship after review"), Some("paused")),
            ("session.goal.resume", None, Some("active")),
            ("session.goal.edit", Some("Explicit edit"), Some("active")),
            ("session.goal.clear", None, None),
        ] {
            let read = wire.read("session.goal.get");
            assert_eq!(read["params"], json!({"sessionId":"session","branchId":"main"}));
            wire.reply(&read, json!({"branchId":"main","through":8,"goal":goal}));
            let write = wire.read(method);
            assert_eq!(write["params"]["branchId"], "main");
            assert_eq!(write["params"]["sessionId"], "session");
            assert_eq!(write["params"].get("objective"), objective.map(|text| json!(text)).as_ref());
            goal = match status {
                None => Value::Null,
                Some(status) => json!({"objective":objective.map(str::to_owned).unwrap_or_else(|| goal["objective"].as_str().unwrap().to_owned()),"status":{"kind":status,"reason":"user"},"evaluator":false}),
            };
            if status != Some("paused") && !goal.is_null() {
                goal["status"].as_object_mut().unwrap().remove("reason");
            }
            wire.reply(&write, json!({"sequence":8}));
            let read = wire.read("session.goal.get");
            wire.reply(&read, json!({"branchId":"main","through":8,"goal":goal}));
            if method == "session.goal.resume" {
                command_session(wire, GOAL_METHODS);
                let read = wire.read("session.goal.get");
                wire.reply(&read, json!({"branchId":"main","through":8,"goal":goal}));
            }
        }
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    assert_eq!(peer.commands(), ["goal"]);
    assert!(peer.command(3, "session", "/goal set").contains("Usage:"));
    assert!(
        peer.command(4, "session", "/goal Ship safely")
            .contains("active\nShip safely")
    );
    assert!(
        peer.command(5, "session", "/goal pause")
            .contains("paused (user)")
    );
    assert!(
        peer.command(6, "session", "/goal Ship after review")
            .contains("paused (user)\nShip after review")
    );
    assert!(
        peer.command(7, "session", "/goal resume")
            .contains("active\nShip after review")
    );
    peer.send(
        8,
        "session/load",
        json!({"sessionId":"session","cwd":home.0,"mcpServers":[]}),
    );
    assert!(peer.response(8).get("error").is_none());
    assert_eq!(peer.commands(), ["goal"]);
    assert!(
        peer.command(9, "session", "/goal")
            .contains("Ship after review")
    );
    assert!(
        peer.command(10, "session", "/goal edit Explicit edit")
            .contains("Explicit edit")
    );
    assert!(
        peer.command(11, "session", "/goal clear")
            .contains("No goal on main")
    );
    assert!(peer.command(12, "session", "/loop").contains("unavailable"));
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_usage_commands_follow_the_selected_account_without_losing_units_or_zero() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[METHODS, &["session.methods", "account.providers", "account.status", "account.usage"]].concat());
        let providers = json!([
            {"providerId":"first","displayName":"First","loginMethods":[]},
            {"providerId":"second","displayName":"Second","loginMethods":[]}
        ]);
        let discovery = wire.read("account.providers");
        wire.reply(&discovery, providers.clone());
        for _ in 0..2 {
            let status = wire.read("account.status");
            wire.reply(&status, json!({"account":{}}));
        }
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({"model":{"providerId":"first","modelId":"model"}}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        command_methods(wire, &[]);
        let discovery = wire.read("account.providers");
        wire.reply(&discovery, providers);
        for (index, (provider, next, report)) in [
            ("first", "second", json!({
                "plan":{"name":"Pro","status":"active"},
                "credits":{"usedUnits":37,"totalUnits":100,"remainingUnits":63,"resetsAt":"2026-12-01T00:00:00Z"},
                "windows":[{"durationHours":5,"tokenUsed":0,"tokenCap":500,"expiresAt":"2026-11-16T02:00:00Z"},{"durationHours":168,"tokenUsed":200,"tokenCap":900,"expiresAt":"2026-11-20T00:00:00Z"}],
                "features":[{"key":"search","used":0,"allowance":10,"remaining":10,"unit":"requests"}],
                "subscriptionId":1234567
            })),
            ("second", "first", json!({"credits":{"usedUnits":0,"totalUnits":0,"remainingUnits":0,"unlimited":true},"features":[{"key":"images","used":3,"allowance":0,"unlimited":true,"unit":"images"}]})),
            ("first", "first", json!({"plan":{"status":"expired"},"credits":{"remainingUnits":0},"features":[{"key":"audio","used":7,"unit":"minutes","periodEnd":"2026-12-01T00:00:00Z"}]})),
            ("first", "second", json!({})),
        ].into_iter().enumerate() {
            let status = wire.read("account.status");
            assert_eq!(status["params"]["providerId"], provider);
            wire.reply(&status, json!({"account":{}}));
            let usage = wire.read("account.usage");
            assert_eq!(usage["params"], json!({"providerId":provider}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(9+index as u64,"configured","context","configured",json!({"provider_id":next,"model_id":"model"}))}));
            wire.reply(&usage, report);
        }
        let status = wire.read("account.status");
        assert_eq!(status["params"]["providerId"], "second");
        wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(13,"configured","context","configured",json!({"provider_id":"api-key","model_id":"model"}))}));
        wire.reply(&status, json!({}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    assert_eq!(peer.commands(), ["usage"]);
    assert!(
        peer.command(3, "session", "/usage other")
            .contains("Usage: /usage")
    );
    let report = peer.command(4, "session", "/usage");
    for expected in [
        "first",
        "Pro · active",
        "63 credits remaining",
        "37/100 credits used",
        "5 hour window: 0/500 tokens used",
        "168 hour window: 200/900 tokens used",
        "2026-11-16T02:00:00Z",
        "2026-11-20T00:00:00Z",
        "0/10 requests used",
    ] {
        assert!(report.contains(expected), "missing {expected}: {report}");
    }
    assert!(!report.contains("1234567"));
    let report = peer.command(5, "session", "/usage");
    assert!(
        report.contains("second")
            && report.contains("unlimited")
            && report.contains("3 images used"),
        "{report}"
    );
    assert!(!report.contains("0/0") && !report.contains("0 credits remaining"));
    let report = peer.command(6, "session", "/usage");
    assert!(
        report.contains("expired")
            && report.contains("0 credits remaining")
            && report.contains("7 minutes used"),
        "{report}"
    );
    assert!(
        peer.command(7, "session", "/usage")
            .contains("unavailable for first")
    );
    assert!(
        peer.command(8, "session", "/usage")
            .contains("Sign in to second")
    );
    assert!(peer.commands().is_empty());
    assert!(peer.command(9, "session", "/usage").contains("unavailable"));
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_loop_commands_keep_native_ids_and_require_an_unambiguous_branch_target() {
    let home = Home::new();
    let (accepted, waiting) = std::sync::mpsc::sync_channel(0);
    let (release, delayed) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(&[METHODS, &["session.methods"]].concat());
        wire.created();
        command_session(
            wire,
            &[
                "session.loop.start",
                "session.loop.list",
                "session.loop.stop",
            ],
        );
        let list = wire.read("session.loop.list");
        wire.reply(&list, json!({"loops":[]}));
        let mut records = Vec::new();
        for (id, prompt, interval) in [
            ("self-paced", "检查 deployment", None),
            ("fixed", "check again", Some(300)),
        ] {
            let start = wire.read("session.loop.start");
            assert_eq!(start["params"]["branchId"], "main");
            assert_eq!(start["params"]["prompt"], prompt);
            assert_eq!(
                start["params"].get("everySeconds"),
                interval.map(|value| json!(value)).as_ref()
            );
            let record = json!({"loopId":id,"branchId":"main","state":"running","round":1,"nextAt":null,"lastRoundAt":null,"expiresAt":"2026-11-18T00:00:00Z","stoppedReason":null,"settledAt":null});
            records.push(record.clone());
            if id == "fixed" {
                accepted.send(()).unwrap();
                delayed.recv_timeout(PATIENCE).unwrap();
            }
            wire.reply(&start, json!({"loop":record}));
        }
        for _ in 0..3 {
            let list = wire.read("session.loop.list");
            assert_eq!(
                list["params"],
                json!({"sessionId":"session","branchId":"main"})
            );
            wire.reply(&list, json!({"loops":records}));
        }
        let list = wire.read("session.loop.list");
        wire.reply(&list, json!({"loops":records}));
        let stop = wire.read("session.loop.stop");
        assert_eq!(
            stop["params"],
            json!({"sessionId":"session","loopId":"fixed"})
        );
        wire.reply(&stop, json!({"loop":records[1],"settled":false}));
        records[1]["state"] = json!("stopped");
        let list = wire.read("session.loop.list");
        wire.reply(&list, json!({"loops":records}));
        let stop = wire.read("session.loop.stop");
        assert_eq!(stop["params"]["loopId"], "self-paced");
        records[0]["state"] = json!("stopped");
        wire.reply(&stop, json!({"loop":records[0]}));
        let list = wire.read("session.loop.list");
        wire.reply(&list, json!({"loops":records}));
        let list = wire.read("session.loop.list");
        wire.reply(&list, json!({"loops":records}));
        let stop = wire.read("session.loop.stop");
        assert_eq!(stop["params"]["loopId"], "fixed");
        wire.reply(&stop, json!({"loop":records[1],"settled":false}));
        let start = wire.read("session.loop.start");
        let mut expiring = json!({"loopId":"expiring","branchId":"main","state":"running","round":1,"nextAt":null});
        wire.reply(&start, json!({"loop":expiring}));
        let list = wire.read("session.loop.list");
        records.push(expiring.clone());
        wire.reply(&list, json!({"loops":records}));
        let stop = wire.read("session.loop.stop");
        assert_eq!(stop["params"]["loopId"], "expiring");
        expiring["state"] = json!("expired");
        wire.reply(&stop, json!({"loop":expiring,"settled":false}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    assert_eq!(peer.commands(), ["loop"]);
    let empty = peer.command(30, "session", "/loop stop");
    assert!(
        peer.command(3, "session", "/loop 检查 deployment")
            .contains("self-paced started")
    );
    peer.send(
        4,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"/loop 5m check again"}]}),
    );
    waiting.recv_timeout(PATIENCE).unwrap();
    writeln!(
        peer.input,
        "{}",
        json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session"}})
    )
    .unwrap();
    peer.input.flush().unwrap();
    peer.send(
        24,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"/loop"}]}),
    );
    assert_eq!(peer.response(24)["error"]["data"]["code"], "session_busy");
    release.send(()).unwrap();
    assert_eq!(peer.response(4)["result"]["stopReason"], "cancelled");
    assert!(
        peer.updates
            .iter()
            .any(|frame| frame["params"]["update"]["content"]["text"]
                .as_str()
                .is_some_and(
                    |text| text.contains("fixed started") && text.contains("every 300 seconds")
                )),
        "cancellation drains the accepted command without pretending the loop was undone"
    );
    let ambiguous = peer.command(5, "session", "/loop stop");
    assert!(ambiguous.contains("choose /loop stop ID"));
    assert!(ambiguous.contains("self-paced") && ambiguous.contains("fixed"));
    assert!(
        peer.command(6, "session", "/loop stop other-branch-id")
            .contains("not on main")
    );
    assert!(peer.command(7, "session", "/loop").contains("round 1"));
    assert!(
        peer.command(8, "session", "/loop stop fixed")
            .contains("still settling")
    );
    assert!(
        peer.command(9, "session", "/loop stop")
            .contains("self-paced stopped")
    );
    let no_live = peer.command(10, "session", "/loop stop");
    assert!(
        peer.command(11, "session", "/loop stop two ids")
            .contains("Usage:")
    );
    let stopped = peer.command(12, "session", "/loop stop fixed");
    assert!(
        peer.command(13, "session", "/loop expire soon")
            .contains("expiring started")
    );
    let expired = peer.command(14, "session", "/loop stop expiring");
    peer.close();
    daemon.join().unwrap();
    assert!(
        empty.contains("No live loop")
            && !empty.contains("choose")
            && no_live.contains("No live loop")
            && !no_live.contains("choose")
            && stopped.contains("fixed stopped")
            && !stopped.contains("still settling")
            && expired.contains("expiring expired")
            && !expired.contains("still settling"),
        "empty: {empty}\nno live: {no_live}\nstopped: {stopped}\nexpired after listing: {expired}"
    );
}

#[test]
fn acp_compact_commands_report_each_native_outcome_once_and_cancel_without_submission() {
    for outcome in ["completed", "skipped", "failed", "cancelled"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[METHODS, &["session.methods"]].concat());
            wire.created();
            command_session(wire, &["session.context.compact"]);
            let compact = wire.read("session.context.compact");
            assert_eq!(compact["params"], json!({"sessionId":"session","branchId":"main"}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(9,"start","context","compaction-start",json!({"through":8,"trigger":"manual","strategy":"summary"}))}));
            if outcome == "cancelled" {
                let stop = wire.read("session.stop");
                assert_eq!(stop["params"], json!({"sessionId":"session","branchId":"main"}));
                wire.reply(&stop, json!({}));
            }
            let terminal = match outcome {
                "completed" => event(10,"finished","display","compaction",json!({"start":9,"strategy":"summary"})),
                "skipped" => event(10,"finished","context","compaction",json!({"through":8,"start":9,"status":"skipped","reason":"insufficient history","diagnostics":{"reason":"manual","trigger":"manual","estimatedInputTokens":0.0,"entriesReplaced":0,"modelCallCount":0,"durationMs":0,"strategy":{"kind":"summary"}}})),
                "failed" => event(10,"finished","context","compaction",json!({"through":8,"start":9,"status":"failed","error":"summary unavailable"})),
                _ => event(10,"finished","context","compaction",json!({"through":8,"start":9,"status":"cancelled","reason":"Stopped by user"})),
            };
            for _ in 0..2 {
                wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":terminal}));
            }
            if matches!(outcome, "completed" | "skipped") {
                wire.reply(&compact, json!({"status":outcome,"reason":"insufficient history","sequence":10}));
            } else {
                wire.send(json!({"jsonrpc":"2.0","id":compact["id"],"error":{"code":-32000,"message":"Native outcome","data":{"appError":format!("compaction_{outcome}")}}}));
            }
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["sessionId"], "session");
            assert_eq!(submit["params"]["user"]["content"][0]["text"], "继续工作");
            wire.reply(&submit, json!({"event":event(11,"next","input","message",json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}))}));
            wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":event(12,"deleted","input","deleted",json!({"input_id":"next"}))}));
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        assert!(peer.response(2).get("error").is_none());
        assert_eq!(peer.commands(), ["compact"]);
        assert!(
            peer.command(3, "session", "/compact please")
                .contains("Usage:")
        );
        peer.updates.clear();
        peer.send(
            4,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"/compact"}]}),
        );
        if outcome == "cancelled" {
            let started = peer.frame();
            assert_eq!(
                started["params"]["update"]["status"], "in_progress",
                "{started}"
            );
            peer.updates.push(started);
            writeln!(
                peer.input,
                "{}",
                json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"session"}})
            )
            .unwrap();
            peer.input.flush().unwrap();
        }
        let response = peer.response(4);
        assert_eq!(
            response["result"]["stopReason"],
            if outcome == "cancelled" {
                "cancelled"
            } else {
                "end_turn"
            },
            "{response}"
        );
        let terminal: Vec<_> = peer
            .updates
            .iter()
            .map(|frame| &frame["params"]["update"])
            .filter(|update| matches!(update["status"].as_str(), Some("completed" | "failed")))
            .collect();
        assert_eq!(
            terminal.len(),
            1,
            "one terminal activity: {:?}",
            peer.updates
        );
        assert_eq!(terminal[0]["toolCallId"], r#"["compaction",9]"#);
        if outcome == "skipped" {
            assert!(terminal[0].to_string().contains("insufficient history"));
        }
        assert!(
            !peer
                .updates
                .iter()
                .any(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        );
        peer.send(
            5,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"继续工作"}]}),
        );
        assert_eq!(peer.response(5)["result"]["stopReason"], "cancelled");
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_names_the_message_of_a_failed_command_after_its_operation() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[METHODS, &["session.methods"]].concat());
        wire.created();
        command_session(
            wire,
            &[
                "session.goal.get",
                "session.goal.set",
                "session.goal.edit",
                "session.goal.pause",
                "session.goal.resume",
                "session.goal.clear",
            ],
        );
        for message in ["Goal storage is unavailable", "Goal storage is full"] {
            let read = wire.read("session.goal.get");
            wire.send(
                json!({"jsonrpc":"2.0","id":read["id"],"error":{"code":-32000,
                "message":message,"data":{"appError":"storage_failed"}}}),
            );
        }
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    assert!(peer.response(2).get("error").is_none());
    let mut ids = Vec::new();
    for (request, message) in [
        (3, "Command failed: Goal storage is unavailable"),
        (4, "Command failed: Goal storage is full"),
    ] {
        peer.updates.clear();
        peer.send(
            request,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"/goal"}]}),
        );
        assert_eq!(
            peer.response(request)["result"]["stopReason"],
            "end_turn",
            "{updates:?}",
            updates = peer.updates
        );
        let chunk = peer
            .updates
            .iter()
            .map(|frame| frame["params"]["update"].clone())
            .find(|update| update["sessionUpdate"] == "agent_message_chunk")
            .expect("a failed command states why");
        assert_eq!(chunk["content"]["text"], message);
        let id = chunk["messageId"]
            .as_str()
            .unwrap_or_else(|| panic!("the failure names its message: {chunk}"));
        let parts: Vec<&str> = serde_json::from_str(id).unwrap();
        assert_eq!(&parts[..2], ["gateway", "command"]);
        assert!(uuid::Uuid::parse_str(parts[2]).is_ok(), "{id}");
        ids.push(id.to_owned());
    }
    assert_ne!(ids[0], ids[1], "one failure message per command");
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
    for (restarted, limited) in [(false, false), (true, false), (false, true)] {
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
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["expectedCwd"], json!(cwd));
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            let facts = [
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
            let pending = json!({"eventId":"delegation","sequence":8,"startedAt":1,"request":facts[0]["data"],"delivery":null});
            assert_eq!(history["params"]["latestTurn"], true);
            assert_eq!(history["params"]["limit"], 64);
            assert!(history["params"].get("after").is_none());
            wire.reply(
                &history,
                json!({"events":[facts[4]],"through":13,"nextBefore":12,"nextAfter":12,"done":true}),
            );
            let methods = wire.read("session.methods");
            wire.reply(
                &methods,
                json!({"methods":[{"name":"session.collaboration.pending","mutating":false}]}),
            );
            let read = wire.read("session.collaboration.pending");
            wire.reply(
                &read,
                json!({"branchId":"main","through":13,"requests":[pending],"next":null}),
            );
            let current = wire.read("session.turn.get");
            assert!(current["params"].get("inputId").is_none());
            wire.reply(
                &current,
                json!({"branchId":"main","through":13,"turn":null,"output":null}),
            );
            let turn = wire.read("session.turn.get");
            assert_eq!(turn["params"]["inputId"], "accepted-input");
            let mut boundary = json!({"turnId":"owned","sequence":13,"ended":true,"outcome":{"status":if restarted {"cancelled"} else {"completed"},"reason":if restarted {"server_restarted"} else {"end_turn"}}});
            if limited {
                boundary["outcome"] = json!({"status":"completed","reason":"output_limit"});
            }
            wire.reply(
                &turn,
                json!({"branchId":"main","through":13,"turn":boundary,"output":null}),
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
            if restarted {
                "cancelled"
            } else if limited {
                "max_tokens"
            } else {
                "end_turn"
            },
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
    let result = peer.response(3);
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
    let cwd = home.0.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let attach = wire.read("session.attach"); assert_eq!(attach["params"]["expectedCwd"],json!(cwd)); wire.reply(&attach,json!({}));
        let old = wire.read("session.history");
        wire.notice("session.closed",json!({"sessionId":"saved"}));
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["expectedCwd"],json!(cwd));
        wire.reply(&old,json!({"events":[event(8,"old","step","text",json!({"turn_id":"old","request_id":"old","delta":"must not leak"}))],"through":8,"nextAfter":8,"done":true}));
        wire.reply(&attach,json!({}));
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
    assert!(peer.commands().is_empty());
    assert_eq!(peer.updates.len(), 2);
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
    let cwd = home.0.clone();
    let (waiting, ready) = std::sync::mpsc::sync_channel(0);
    let (release, delayed) = std::sync::mpsc::sync_channel(0);
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let attach = wire.read("session.attach"); assert_eq!(attach["params"]["expectedCwd"],json!(cwd)); wire.reply(&attach,json!({}));
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
    assert!(peer.commands().is_empty());
    assert_eq!(peer.updates.len(), 2);
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
        let calls: Vec<_> = Vec::from_fn(128, |_| wire.read("session.list"));
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
    /// The line `frame` last read, for assertions about how it is spelled.
    received: String,
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
            received: String::new(),
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
        loop {
            let frame = self.frame();
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
        if self.output.buffer().is_empty() {
            let deadline = Instant::now() + HANG_GUARD;
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
        self.received = line;
        serde_json::from_str(&self.received).expect("stdout must contain only ACP")
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

/// One question the flow asks: `(multi_select, allow_custom)`.
type Shape = (bool, bool);

/// The questions one `ask_user` call asks, `choice0`, `choice1` and so on.
fn ask_user_input(shapes: &[Shape]) -> Value {
    let questions: Vec<Value> = shapes
        .iter()
        .enumerate()
        .map(|(index, &(multi_select, allow_custom))| {
            json!({
                "id":format!("choice{index}"),
                "question":"Choose",
                "options":[{"label":"A"},{"label":"B"}],
                "multiSelect":multi_select,
                "allowCustom":allow_custom
            })
        })
        .collect();
    json!({"questions":questions})
}

/// The question shapes the flow's forms ask, by reply index. Reply 9 answers a
/// form of two optional single-select questions that each allow a custom
/// answer, so its form lists `q0, custom0, q1, custom1`; every other reply
/// answers one question.
fn flow_shapes(index: usize) -> Vec<Shape> {
    match index {
        9 => vec![(false, true), (false, true)],
        _ => vec![(index != 3 && index != 7, index != 6)],
    }
}

/// Asserts the order the received form lists its fields in, reading the line as
/// it stands: each question, then the custom answer beside it, in question
/// order.
///
/// A `Value` compares members without their order, and a client renders the
/// fields in the schema's order, so the order has to be read from the line
/// itself. No field name this form writes appears as a value in it, so the
/// first `"q0":` in the line is the member.
fn assert_property_order(line: &str, shapes: &[Shape]) {
    let expected: Vec<String> = shapes
        .iter()
        .enumerate()
        .flat_map(|(index, &(_, allow_custom))| {
            let mut names = vec![format!("q{index}")];
            names.extend(allow_custom.then(|| format!("custom{index}")));
            names
        })
        .collect();
    let mut listed: Vec<(usize, &str)> = expected
        .iter()
        .map(|name| {
            let key = format!("\"{name}\":");
            let at = line
                .find(&key)
                .unwrap_or_else(|| panic!("the form asks {name}: {line}"));
            (at, name.as_str())
        })
        .collect();
    listed.sort();
    let listed: Vec<&str> = listed.into_iter().map(|(_, name)| name).collect();
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    assert_eq!(
        listed, expected,
        "the form lists its fields in this order: {line}"
    );
}

/// The `elicitation/create` params a question form of these shapes is expected
/// to carry, spelled out here rather than built from the gateway's own code.
fn question_form(request: &str, shapes: &[Shape]) -> Value {
    let choices = json!([
        {"const":"A","title":"A","description":null},
        {"const":"B","title":"B","description":null},
    ]);
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (index, &(multi_select, allow_custom)) in shapes.iter().enumerate() {
        let property = if multi_select {
            json!({"type":"array","title":"Choose","description":"Choose","minItems":1,"maxItems":2,"items":{"anyOf":choices}})
        } else {
            json!({"type":"string","title":"Choose","description":"Choose","oneOf":choices})
        };
        properties.insert(format!("q{index}"), property);
        if allow_custom {
            properties.insert(
                format!("custom{index}"),
                json!({"type":"string","title":"Custom answer: Choose"}),
            );
        } else {
            required.push(format!("q{index}"));
        }
    }
    json!({
        "mode":"form",
        "sessionId":"session",
        "toolCallId":format!("[\"main\",\"{request}\",\"question\"]"),
        "message":"Answer the agent's questions",
        "requestedSchema":{"type":"object","properties":properties,"required":required}
    })
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
        json!({"action":"accept","content":{"q0":"A","custom1":"second"}}),
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
        let import = wire.read("attachment.import");
        assert_eq!(std::fs::read(import["params"]["path"].as_str().unwrap()).unwrap(), b"image");
        assert_eq!(import["params"]["image"], true);
        wire.reply(&import, json!({"attachmentId":"image-id","path":"/files/image-id","bytes":5,"image":{"type":"file","mediaType":"image/png","fileId":"image-id"}}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["user"]["content"][0]["source"]["fileId"], "image-id");
        assert!(submit["params"]["user"]["content"][0]["source"].get("data").is_none());
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
        fact(wire,event(12,"ask","step","tool-call",json!({"turn_id":"turn","request_id":"model","calls":[{"id":"question","name":"ask_user","input":ask_user_input(&[(true,true)])}]})));
        let answer = wire.read("session.question.respond");
        assert_eq!(answer["params"]["requestId"],"model");
        assert_eq!(answer["params"]["answers"],json!([{"questionId":"choice0","selected":["A","B"],"custom":"C"}]));
        wire.reply(&answer,json!({}));
        fact(wire,event(13,"result","step","tool-result",json!({"turn_id":"turn","request_id":"model","call_event_id":"ask","results":[{"call_id":"question","result":{"content":"answered"}}]})));
        for index in 0..additional_questions {
            let sequence = 14 + index as u64 * 2;
            let request = format!("model-{index}");
            let ask = format!("ask-{index}");
            fact(wire,event(sequence,&ask,"step","tool-call",json!({"turn_id":"turn","request_id":request,"calls":[{"id":"question","name":"ask_user","input":ask_user_input(&flow_shapes(index))}]})));
            let answer = wire.read("session.question.respond");
            assert_eq!(answer["params"]["requestId"], request);
            if index == 3 {
                assert_eq!(answer["params"]["answers"], json!([{"questionId":"choice0","selected":[],"custom":"Custom only"}]));
            } else if index == 9 {
                assert_eq!(answer["params"]["answers"], json!([
                    {"questionId":"choice0","selected":["A"]},
                    {"questionId":"choice1","selected":[],"custom":"second"}
                ]));
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
            Some("session/request_permission") => {
                assert_eq!(
                    frame["params"],
                    json!({
                        "sessionId":"session",
                        "toolCall":{
                            "toolCallId":"[\"approval\",\"session\",\"child\",\"approval\",\"\"]",
                            "title":"Run command?",
                            "kind":"other",
                            "status":"pending",
                            "rawInput":{"command":"echo test"}
                        },
                        "options":[
                            {"optionId":"allow_once","name":"Allow once","kind":"allow_once"},
                            {"optionId":"reject_once","name":"Reject once","kind":"reject_once"}
                        ]
                    }),
                    "{frame}"
                );
                peer.answer(
                    &frame,
                    json!({"outcome":{"outcome":"selected","optionId":"reject_once"}}),
                )
            }
            Some("elicitation/create") => {
                let _: acp::CreateElicitationRequest =
                    serde_json::from_value(frame["params"].clone()).unwrap();
                let (request, shapes) = match elicitation {
                    0 => ("model".to_owned(), flow_shapes(0)),
                    index => (format!("model-{}", index - 1), flow_shapes(index - 1)),
                };
                assert_property_order(&peer.received, &shapes);
                assert_eq!(frame["params"], question_form(&request, &shapes), "{frame}");
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
        (json!({"auth":{"terminal":false}}), false),
        (json!({}), false),
        (json!({"terminal":true}), false),
        (json!({"_meta":{"terminal-auth":"true"}}), false),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        let mut methods = METHODS.to_vec(); methods.push("account.providers"); wire.hello(&methods);
        let providers = wire.read("account.providers");
        wire.reply(&providers,json!([{"providerId":"managed-fixture","displayName":"Fixture","loginMethods":["browser","device"]}]));
        let status = wire.read("account.status"); wire.reply(&status,json!({"account":{"displayName":"fixture"}}));
        if !terminal_available {
            let status = wire.read("account.status"); wire.reply(&status,json!({"account":{"displayName":"fixture"}}));
        }
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
        assert_eq!(methods.len(), 1, "{initialized}");
        let method = &methods[0];
        let (flow, handler) = if terminal_available {
            ("device", "terminal")
        } else {
            ("browser", "agent")
        };
        let id: Value = serde_json::from_str(method["id"].as_str().unwrap()).unwrap();
        assert_eq!(id, json!(["dim", "managed-fixture", flow, handler]));
        if terminal_available {
            assert_eq!(method["type"], "terminal");
            let args = method["args"].as_array().unwrap();
            assert_eq!(
                &args[args.len() - 2..],
                &[json!("--auth-setup"), method["id"].clone()]
            );
        } else {
            assert!(method.get("type").is_none());
            assert!(method.get("args").is_none());
        }
        peer.send(2, "authenticate", json!({"methodId":method["id"]}));
        let response = peer.response(2);
        if terminal_available {
            assert_eq!(response["error"]["code"], -32602, "{response}");
        } else {
            assert_eq!(response["result"], json!({}), "{response}");
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
                "provider.management.list",
                "account.login.begin",
                "provider.refresh",
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
            let listed = wire.read("provider.management.list");
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
            let refresh = wire.read("provider.refresh");
            assert_eq!(refresh["params"], json!({"providerId":"fixture"}));
            wire.reply(&refresh, json!({"version":1,"models":[]}));
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

/// The `effort` option of an ACP response.
fn effort(value: &Value) -> &Value {
    value["result"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == "effort")
        .unwrap_or_else(|| panic!("no effort option: {value}"))
}

/// A `context/configured` fact carrying this flow's model, declaration and
/// adopted preferences, as the public view gives them.
fn configured(id: &str, sequence: u64, preferences: Value) -> Value {
    let mut event = configured_event(sequence, "model", preferences);
    event["id"] = json!(id);
    event["data"]["capability_snapshot"] =
        declaration(json!({"supported":true,"effortOptions":["low","high"]}));
    event
}

/// An adopted capability snapshot whose reasoning declaration is `reasoning`,
/// or none when it is null.
fn declaration(reasoning: Value) -> Value {
    let mut capabilities = dim_agent_types::ModelCapabilities::with_limits(64_000, 2_048);
    capabilities.reasoning = !reasoning.is_null();
    let mut snapshot =
        json!({"target":{"providerId":"test","modelId":"model"},"capabilities":capabilities});
    if !reasoning.is_null() {
        snapshot["reasoning"] = reasoning;
    }
    snapshot
}

#[test]
fn acp_stdio_advertises_effort_from_the_adopted_configuration() {
    // A model whose declaration names a default effort adopts it, a model with
    // efforts but no adopted one says so, and a model without reasoning gets no
    // option at all.
    for (reasoning, preferences, expected) in [
        (
            json!({"supported":true,"effort":"medium","effortOptions":["low","medium","high"]}),
            json!({"reasoningEffort":"medium"}),
            Some(("medium", 4)),
        ),
        // A declaration that names an effort its own options do not list, as
        // 325 bundled entries do, so the adopted configuration carries none.
        (
            json!({"supported":true,"effort":"xhigh","effortOptions":["low","medium","high"]}),
            json!({}),
            Some(("provider-default", 4)),
        ),
        // The adopted configuration carries an effort its own declaration
        // does not list, so the option shows it rather than denying it.
        (
            json!({"supported":true,"effortOptions":["low","medium","high"]}),
            json!({"reasoningEffort":"minimal"}),
            Some(("minimal", 5)),
        ),
        (json!(null), json!({}), None),
    ] {
        let home = Home::new();
        let declaration = declaration(reasoning);
        let daemon = home.daemon(move |wire| {
            let mut methods = METHODS.to_vec();
            methods.extend([
                "session.methods",
                "provider.models.query",
                "session.model.update",
            ]);
            wire.hello(&methods);
            wire.created();
            let attach = wire.read("session.attach");
            let mut record = attach_record(&attach);
            record["capabilitySnapshot"] = declaration;
            if !preferences
                .as_object()
                .is_some_and(|value| value.is_empty())
            {
                record["modelPreferences"] = preferences;
            }
            wire.reply(&attach, record);
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let methods = wire.read("session.methods");
            wire.reply(
                &methods,
                json!({"methods":[{"name":"session.model.update","mutating":true}]}),
            );

            let catalog = wire.read("provider.models.query");
            wire.reply(
                &catalog,
                json!({"models":[{"providerId":"test","model":{"id":"model","name":"Model"}}],"next":null}),
            );
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.initialize();
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        let created = peer.response(2);
        let option = created["result"]["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["id"] == "effort");
        let Some((current, count)) = expected else {
            assert!(option.is_none(), "{created}");
            peer.close();
            daemon.join().unwrap();
            continue;
        };
        let option = option.unwrap_or_else(|| panic!("no effort option: {created}"));
        assert_eq!(option["category"], "thought_level", "{option}");
        assert_eq!(option["currentValue"], current, "{option}");
        let values: Vec<&str> = option["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|choice| choice["value"].as_str().unwrap())
            .collect();
        assert_eq!(values.len(), count, "{option}");
        assert_eq!(values[0], "provider-default", "{option}");
        if current == "minimal" {
            assert!(values.contains(&"minimal"), "{option}");
            peer.send(
                3,
                "session/set_config_option",
                json!({"sessionId":"session","configId":"effort","value":"minimal"}),
            );
            let refused = peer.response(3);
            assert_eq!(refused["error"]["code"], -32602, "{refused}");
        }
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_model_choices_keep_the_catalog_provider_and_adopted_result() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec(); methods.extend(["session.methods","provider.models.query"]); wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({"sequence":8,"model":{"providerId":"provider/with/slashes","modelId":"native/model"}}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[event(8,"configured","context","configured",json!({"provider_id":"provider/with/slashes","model_id":"native/model"}))],"through":8,"nextAfter":8,"done":true}));
        let methods = wire.read("session.methods"); wire.reply(&methods,json!({"methods":[{"name":"session.model.update","mutating":true}]}));
        let catalog = |wire: &mut Wire| {

            let read = wire.read("provider.models.query");
            assert_eq!(read["params"]["selectableOnly"],true);
            wire.reply(&read,json!({"models":[],"next":{"providerId":"provider/with/slashes","modelId":"hidden"}}));
            let read = wire.read("provider.models.query");
            assert_eq!(read["params"],json!({"selectableOnly":true,"limit":128,"after":{"providerId":"provider/with/slashes","modelId":"hidden"}}));
            wire.reply(&read,json!({"models":[{"providerId":"provider/with/slashes","model":{"id":"native/model","name":"First"}},{"providerId":"provider/with/slashes","model":{"id":"next/model","name":"Next"}}],"next":null}));
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
fn acp_stdio_sets_effort_through_the_adopted_model_without_saving_a_preference() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.extend([
            "session.methods",
            "provider.models.query",
            "session.model.update",
        ]);
        wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["capabilitySnapshot"] =
            declaration(json!({"supported":true,"effortOptions":["low","high"]}));
        wire.reply(&attach, record);
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let methods = wire.read("session.methods");
        wire.reply(
            &methods,
            json!({"methods":[{"name":"session.model.update","mutating":true}]}),
        );

        let catalog = wire.read("provider.models.query");
        wire.reply(
            &catalog,
            json!({"models":[{"providerId":"test","model":{"id":"model","name":"Model"}}],"next":null}),
        );
        // The App Server publishes the adopted fact before it replies, so the
        // reply can only report a value the owner has already committed.
        let update = wire.read("session.model.update");
        assert_eq!(
            update["params"],
            json!({"sessionId":"session","branchId":"main",
                "modelPreferences":{"reasoningEffort":"high"}})
        );
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main",
                "event":configured("configured-9", 9, json!({"reasoningEffort":"high"}))}),
        );
        wire.reply(
            &update,
            json!({"outcome":"adopted","model":{"providerId":"test","modelId":"model"}}),
        );
        let cleared = wire.read("session.model.update");
        assert_eq!(
            cleared["params"],
            json!({"sessionId":"session","branchId":"main",
                "modelPreferences":{"reasoningEffort":{"action":"clear"}}})
        );
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main",
                "event":configured("configured-10", 10, json!({}))}),
        );
        wire.reply(
            &cleared,
            json!({"outcome":"adopted","model":{"providerId":"test","modelId":"model"}}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"effort","value":"high"}),
    );
    let set = peer.response(3);
    assert_eq!(effort(&set)["currentValue"], "high", "{set}");
    peer.send(
        4,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"effort","value":"provider-default"}),
    );
    let cleared = peer.response(4);
    assert_eq!(
        effort(&cleared)["currentValue"],
        "provider-default",
        "{cleared}"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_keeps_the_adopted_effort_when_a_clear_changes_nothing() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.extend([
            "session.methods",
            "provider.models.query",
            "session.model.update",
        ]);
        wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["capabilitySnapshot"] = declaration(
            json!({"supported":true,"effort":"medium","effortOptions":["low","medium","high"]}),
        );
        record["modelPreferences"] = json!({"reasoningEffort":"medium"});
        wire.reply(&attach, record);
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let methods = wire.read("session.methods");
        wire.reply(
            &methods,
            json!({"methods":[{"name":"session.model.update","mutating":true}]}),
        );

        let catalog = wire.read("provider.models.query");
        wire.reply(
            &catalog,
            json!({"models":[{"providerId":"test","model":{"id":"model","name":"Model"}}],"next":null}),
        );
        // The clear restores the declaration the Session already sends, so the
        // owner reports `unchanged` and publishes no fact.
        let cleared = wire.read("session.model.update");
        assert_eq!(
            cleared["params"],
            json!({"sessionId":"session","branchId":"main",
                "modelPreferences":{"reasoningEffort":{"action":"clear"}}})
        );
        wire.reply(
            &cleared,
            json!({"outcome":"unchanged","model":{"providerId":"test","modelId":"model"}}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"effort","value":"provider-default"}),
    );
    let cleared = peer.response(3);
    assert_eq!(effort(&cleared)["currentValue"], "medium", "{cleared}");
    // The reply precedes the update this write publishes.
    let update = peer.frame();
    assert_eq!(update["method"], "session/update", "{update}");
    let option = update["params"]["update"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == "effort")
        .unwrap();
    assert_eq!(option["currentValue"], "medium", "{option}");
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_reports_an_adopted_clear_from_the_fact_that_precedes_the_reply() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.extend([
            "session.methods",
            "provider.models.query",
            "session.model.update",
        ]);
        wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["capabilitySnapshot"] =
            declaration(json!({"supported":true,"effortOptions":["low","medium","high"]}));
        wire.reply(&attach, record);
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let methods = wire.read("session.methods");
        wire.reply(
            &methods,
            json!({"methods":[{"name":"session.model.update","mutating":true}]}),
        );

        let catalog = wire.read("provider.models.query");
        wire.reply(
            &catalog,
            json!({"models":[{"providerId":"test","model":{"id":"model","name":"Model"}}],"next":null}),
        );
        // The clear changes the configuration, so the fact is published
        // before the reply.
        let cleared = wire.read("session.model.update");
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main",
                "event":configured("configured-9", 9, json!({"reasoningEffort":"medium"}))}),
        );
        wire.reply(
            &cleared,
            json!({"outcome":"adopted","model":{"providerId":"test","modelId":"model"}}),
        );
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"effort","value":"provider-default"}),
    );
    let cleared = peer.response(3);
    assert_eq!(effort(&cleared)["currentValue"], "medium", "{cleared}");
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_model_switch_replaces_effort_choices_and_refuses_a_missing_value() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.extend(["session.methods", "provider.models.query", "session.model.update"]);
        wire.hello(&methods);
        wire.created();
        let attach = wire.read("session.attach");
        let mut record = attach_record(&attach);
        record["capabilitySnapshot"] =
            declaration(json!({"supported":true,"effortOptions":["low","high"]}));
        record["modelPreferences"] = json!({"reasoningEffort":"high"});
        wire.reply(&attach, record);
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let methods = wire.read("session.methods");
        wire.reply(
            &methods,
            json!({"methods":[{"name":"session.model.update","mutating":true}]}),
        );

        let catalog = wire.read("provider.models.query");
        wire.reply(&catalog,json!({"models":[{"providerId":"test","model":{"id":"model","name":"Model"}},{"providerId":"test","model":{"id":"other","name":"Other"}}],"next":null}));
        let update = wire.read("session.model.update");
        assert_eq!(update["params"]["modelId"], "other");
        wire.reply(
            &update,
            json!({"outcome":"adopted","model":{"providerId":"test","modelId":"other"}}),
        );
        let mut event = configured_event(9, "other", json!({}));
        event["data"]["capability_snapshot"] =
            declaration(json!({"supported":true,"effortOptions":["minimal","max"]}));
        wire.notice(
            "session.event",
            json!({"sessionId":"session","branchId":"main","event":event}),
        );

        let catalog = wire.read("provider.models.query");
        wire.reply(&catalog,json!({"models":[{"providerId":"test","model":{"id":"model","name":"Model"}},{"providerId":"test","model":{"id":"other","name":"Other"}}],"next":null}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"model","value":"other"}),
    );
    peer.response(3);
    // The new model's declaration arrives as its own configuration event, and
    // that update is where a client learns the new choices.
    let update = peer
        .updates
        .iter()
        .find(|frame| {
            frame["params"]["update"]["configOptions"]
                .as_array()
                .is_some_and(|options| options.iter().any(|option| option["id"] == "effort"))
        })
        .unwrap_or_else(|| panic!("no effort option update: {}", peer.updates.len()));
    let option = update["params"]["update"]["configOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == "effort")
        .unwrap();
    assert_eq!(option["currentValue"], "provider-default", "{option}");
    let choices: Vec<&str> = option["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|choice| choice["value"].as_str().unwrap())
        .collect();
    assert_eq!(choices, ["provider-default", "minimal", "max"], "{option}");
    peer.send(
        4,
        "session/set_config_option",
        json!({"sessionId":"session","configId":"effort","value":"high"}),
    );
    let refused = peer.response(4);
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
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
        let mut methods = METHODS.to_vec(); methods.extend(["session.fork", "session.turn.get"]); wire.hello(&methods); wire.created();
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
        let history = wire.read("session.history");
        assert_eq!(history["params"]["latestTurn"],true);
        assert_eq!(history["params"]["limit"],64);
        assert!(history["params"].get("after").is_none());
        wire.reply(&history,json!({"events":[facts[9],facts[10]],"through":19,"nextBefore":18,"nextAfter":19,"done":true}));
        let earlier=wire.read("session.history");
        assert_eq!(earlier["params"],json!({"sessionId":"session","branchId":"main","through":19,"before":18,"limit":2}));
        wire.reply(&earlier,json!({"events":[facts[7],facts[8]],"through":19,"nextBefore":16,"nextAfter":17,"done":true}));
        let list = wire.read("session.list"); wire.reply(&list,json!([{"sessionId":"session","cwd":cwd}]));
        let turn = wire.read("session.turn.get");
        assert_eq!(turn["params"]["turnId"],"turn");
        wire.reply(&turn,json!({"branchId":"main","through":19,"turn":{"turnId":"turn","sequence":15,"ended":true},"output":null}));
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
        json!({"sessionId":"session","version":1}),
    );
    assert_eq!(
        peer.response(3)["error"]["data"]["code"],
        "unsupported_history_version"
    );
    peer.send(
        30,
        "_lody/session/history/read",
        json!({"sessionId":"session","version":2}),
    );
    assert_eq!(
        peer.response(30)["result"],
        json!({"through":19,"before":18,"complete":false})
    );
    peer.send(
        31,
        "_lody/session/history/read",
        json!({"sessionId":"session","version":2,"through":19,"before":18,"limit":2}),
    );
    assert_eq!(
        peer.response(31)["result"],
        json!({"through":19,"before":16,"complete":false})
    );
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

/// A client that declares `session.compaction` gets the protocol's compaction
/// entity; one that does not keeps the tool call Dim sent before.
#[test]
fn acp_stdio_sends_context_compaction_as_the_protocol_entity_only_when_declared() {
    for declared in [true, false] {
        let home = Home::new();
        let cwd = home.0.clone();
        let daemon = home.daemon(move |wire| {
            wire.hello(METHODS);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, json!({}));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            let fact = |wire: &mut Wire, value: Value| {
                wire.notice(
                    "session.event",
                    json!({"sessionId":"session","branchId":"main","event":value}),
                )
            };
            let submit = wire.read("session.submit");
            let input = event(
                9,
                "input",
                "input",
                "message",
                json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}),
            );
            wire.reply(&submit, json!({"event":input}));
            fact(wire, input);
            fact(wire, event(10, "start", "turn", "start", json!({"turn_id":"turn"})));
            fact(wire, event(11, "consumed", "input", "consumed", json!({"input_id":"input"})));
            fact(wire, event(12, "compacting", "context", "compaction-start", json!({"turn_id":"turn","through":11,"trigger":"manual","strategy":"summary"})));
            fact(wire, event(13, "compacted", "context", "compaction", json!({"turn_id":"turn","through":11,"start":12,"status":"failed","error":"summary unavailable"})));
            fact(wire, event(20, "cancel-start", "context", "compaction-start", json!({"turn_id":"turn","through":11,"trigger":"manual","strategy":"summary"})));
            fact(wire, event(21, "cancelled", "context", "compaction", json!({"turn_id":"turn","through":11,"start":20,"status":"cancelled","reason":"Stopped by user"})));
            fact(wire, event(30, "skip-start", "context", "compaction-start", json!({"turn_id":"turn","through":11,"trigger":"threshold","strategy":"summary"})));
            fact(wire, event(31, "skipped", "context", "compaction", json!({"turn_id":"turn","through":11,"start":30,"status":"skipped","reason":"insufficient history","diagnostics":{"reason":"manual","trigger":"manual","estimatedInputTokens":0.0,"entriesReplaced":0,"modelCallCount":0,"durationMs":0,"strategy":{"kind":"summary"}}})));
            fact(wire, event(40, "summary-start", "context", "compaction-start", json!({"turn_id":"turn","through":11,"trigger":"manual","strategy":"summary"})));
            fact(wire, event(41, "display", "display", "compaction", json!({"start":40,"strategy":"summary"})));
            fact(wire, event(50, "end", "turn", "end", json!({"turn_id":"turn","status":"completed","reason":"end_turn"})));
            if declared {
                let attach = wire.read("session.attach");
                assert_eq!(attach["params"]["expectedCwd"], json!(cwd));
                wire.reply(&attach, json!({}));
                let history = wire.read("session.history");
                wire.reply(
                    &history,
                    json!({"events":[
                        event(60,"replayed-start","context","compaction-start",json!({"through":11,"trigger":"manual","strategy":"summary"})),
                        event(70,"replayed-display","display","compaction",json!({"start":71,"strategy":"summary"}))
                    ],"through":71,"nextAfter":71,"done":true}),
                );
                fact(wire, event(72, "live-terminal", "context", "compaction", json!({"through":11,"start":60,"status":"cancelled","reason":"Stopped by user"})));
                fact(wire, event(73, "after", "step", "text", json!({"turn_id":"turn","request_id":"later","delta":"after"})));
            }
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        let capabilities = if declared {
            json!({"session":{"compaction":{}}})
        } else {
            json!({})
        };
        peer.send(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":capabilities}),
        );
        assert_eq!(peer.response(1)["result"]["protocolVersion"], 1);
        peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
        peer.response(2);
        peer.send(
            3,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":"Compact this"}]}),
        );
        assert_eq!(peer.response(3)["result"]["stopReason"], "end_turn");
        let compactions: Vec<_> = peer
            .updates
            .iter()
            .map(|frame| &frame["params"]["update"])
            .filter(|update| {
                update["sessionUpdate"] == "compaction_update"
                    || update["_meta"]["lody"]["activity"]["kind"] == "context_compaction"
            })
            .collect();
        let id = |start: u64| json!(["compaction", start]).to_string();
        if declared {
            let entity = |update: &&Value| {
                (
                    update["sessionUpdate"].clone(),
                    update["compactionId"].clone(),
                    update["status"].clone(),
                )
            };
            assert_eq!(
                compactions.iter().map(entity).collect::<Vec<_>>(),
                vec![
                    (
                        json!("compaction_update"),
                        json!(id(12)),
                        json!("in_progress")
                    ),
                    (json!("compaction_update"), json!(id(12)), json!("failed")),
                    (
                        json!("compaction_update"),
                        json!(id(20)),
                        json!("in_progress")
                    ),
                    (
                        json!("compaction_update"),
                        json!(id(20)),
                        json!("cancelled")
                    ),
                    (
                        json!("compaction_update"),
                        json!(id(30)),
                        json!("in_progress")
                    ),
                    (
                        json!("compaction_update"),
                        json!(id(30)),
                        json!("completed")
                    ),
                    (
                        json!("compaction_update"),
                        json!(id(40)),
                        json!("in_progress")
                    ),
                    (
                        json!("compaction_update"),
                        json!(id(40)),
                        json!("completed")
                    ),
                ],
                "each compaction is one entity under its own start: {compactions:?}"
            );
            assert_eq!(
                compactions[1]["error"], "summary unavailable",
                "the protocol's own error field carries the failure"
            );
            assert_eq!(
                compactions[3]["error"], "Cancelled: Stopped by user",
                "a cancellation keeps its own status and its real reason"
            );
            assert!(
                compactions[5].get("error").is_none(),
                "a skipped compaction ends as completed and states no error: {:?}",
                compactions[5]
            );
            assert_eq!(
                compactions[7]["status"], "completed",
                "the display summary closes the compaction its start opened"
            );
            assert!(
                !peer
                    .updates
                    .iter()
                    .any(|frame| frame["params"]["update"]["title"] == "Context compaction"),
                "a client that asked for the entity gets no compaction tool call"
            );

            // A loaded session replays the compaction it kept, and a terminal
            // fact that arrives after the replay patches that same entity.
            peer.updates.clear();
            peer.send(
                4,
                "session/load",
                json!({"sessionId":"session","cwd":home.0,"mcpServers":[]}),
            );
            let loaded = peer.response(4);
            assert!(loaded.get("error").is_none(), "{loaded}");
            let replayed: Vec<_> = peer
                .updates
                .iter()
                .map(|frame| &frame["params"]["update"])
                .filter(|update| update["sessionUpdate"] == "compaction_update")
                .collect();
            assert_eq!(
                replayed
                    .iter()
                    .map(|update| (update["compactionId"].clone(), update["status"].clone(),))
                    .collect::<Vec<_>>(),
                vec![
                    (json!(id(60)), json!("in_progress")),
                    (json!(id(71)), json!("completed")),
                ],
                "a loaded session replays the compaction it kept: {replayed:?}"
            );
            // Every frame between the reply and the marker that ends the live
            // window is checked, so a duplicate or extra terminal cannot pass.
            let mut after = Vec::new();
            loop {
                let update = peer.frame()["params"]["update"].clone();
                if update["sessionUpdate"] == "agent_message_chunk" {
                    break;
                }
                after.push(update);
            }
            assert_eq!(
                after
                    .iter()
                    .map(|update| (
                        update["sessionUpdate"].clone(),
                        update["compactionId"].clone(),
                        update["status"].clone(),
                    ))
                    .collect::<Vec<_>>(),
                vec![(
                    json!("compaction_update"),
                    json!(id(60)),
                    json!("cancelled"),
                )],
                "a terminal fact after the replay patches the replayed start's entity, once: {after:?}"
            );
            assert_eq!(
                after[0]["error"], "Cancelled: Stopped by user",
                "the live terminal keeps its reason"
            );
        } else {
            assert_eq!(
                compactions
                    .iter()
                    .map(|update| (update["toolCallId"].clone(), update["status"].clone(),))
                    .collect::<Vec<_>>(),
                vec![
                    (json!(id(12)), json!("in_progress")),
                    (json!(id(12)), json!("failed")),
                    (json!(id(20)), json!("in_progress")),
                    (json!(id(20)), json!("failed")),
                    (json!(id(30)), json!("in_progress")),
                    (json!(id(30)), json!("completed")),
                    (json!(id(40)), json!("in_progress")),
                    (json!(id(40)), json!("completed")),
                ],
                "without the capability every compaction stays the tool call it was: {compactions:?}"
            );
            assert!(
                compactions[1]["content"][0]["content"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("summary unavailable")
            );
            assert_eq!(
                compactions[3]["content"][0]["content"]["text"], "Cancelled: Stopped by user",
                "a cancelled compaction keeps its own reason"
            );
            assert_eq!(
                compactions[5]["content"][0]["content"]["text"], "Skipped: insufficient history",
                "the skipped reason is the text an undeclared client reads"
            );
            let automatic =
                |index: usize| compactions[index]["_meta"]["lody"]["activity"]["automatic"].clone();
            assert_eq!(
                (automatic(0), automatic(1)),
                (json!(false), json!(false)),
                "a manual compaction reports automatic false at its start and at its terminal"
            );
            assert_eq!(
                (automatic(4), automatic(5)),
                (json!(true), json!(true)),
                "a threshold compaction reports automatic true at its start and at its terminal"
            );
            assert!(
                !peer
                    .updates
                    .iter()
                    .any(|frame| frame["params"]["update"]["sessionUpdate"] == "compaction_update"),
                "a client that declared nothing must not receive the entity"
            );
        }
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_initialize_names_every_capability_it_reports() {
    for mcp in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let mut methods = METHODS.to_vec();
            if mcp {
                methods.push("mcp.temporary.capabilities");
            }
            wire.hello(&methods);
            detached_eof(wire);
        });
        let mut peer = Peer::start(&home);
        peer.send(
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{}}),
        );
        let result = peer.response(1)["result"].clone();
        let version = result["agentInfo"]["version"].as_str().unwrap().to_owned();
        assert!(
            version.starts_with(env!("CARGO_PKG_VERSION")),
            "the build reports its own version: {version}"
        );
        // Every field the protocol crate serializes into this result, including
        // the ones the old literals omitted because they hold a default value:
        // `promptCapabilities.audio`, `auth`, and the MCP pair when MCP is
        // unavailable.
        assert_eq!(
            result,
            json!({
                "protocolVersion":1,
                "agentInfo":{"name":"dimcode","version":version},
                "authMethods":[],
                "agentCapabilities":{
                    "loadSession":true,
                    "promptCapabilities":{"image":true,"audio":false,"embeddedContext":true},
                    "mcpCapabilities":{"http":mcp,"sse":mcp},
                    "sessionCapabilities":{"resume":{},"list":{},"close":{}},
                    "auth":{},
                    "_meta":{"lody":{
                        "sessionHistory":{"version":2},
                        "compaction":{"version":1},
                        "tasks":{"version":1,"background":true},
                        "subagentEvents":{"version":1}
                    }}
                }
            }),
            "mcp={mcp}: the whole initialize result, field presence included"
        );
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn acp_stdio_lody_startup_config_is_applied_before_creation_without_shared_writes() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.push("provider.models.query");
        wire.hello(&methods);

        let catalog = wire.read("provider.models.query");
        wire.reply(
            &catalog,
            json!({"models":[{"providerId":"native-provider","model":{"id":"chosen","name":"Chosen"}}],"next":null}),
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
        let first_inputs = ["Call inspect"];
        let mut next = 0;
        let mut titles = Vec::new();
        while next < 3 || titles.len() < first_inputs.len() {
            let deadline = Instant::now() + PATIENCE;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "{}",
                            if next == 3 {
                                "every new Session's first input must be titled"
                            } else {
                                "The model request did not arrive"
                            }
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
            let (delta, finish) = if let Some(input) = title_request_input(&request) {
                titles.push(input);
                (json!({"role":"assistant","content":"ACP_TITLE"}), "stop")
            } else {
                let index = next;
                next += 1;
                if index == 1 {
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
                }
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
        titles.sort();
        assert_eq!(
            titles, first_inputs,
            "each new Session's first input is titled with the configured model"
        );
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
    let deadline = Instant::now() + PATIENCE;
    while !pid_file.with_extension("pid.cancelled").exists() {
        assert!(
            Instant::now() < deadline,
            "EOF must cancel the accepted MCP call before closing its server"
        );
        thread::sleep(Duration::from_millis(10));
    }
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
        let expected = [
            ("ACP_REAL_LIMIT", "ACP_PARTIAL"),
            ("ACP_REAL_REQUEST", "ACP_REAL_OK"),
            ("ACP_REAL_FOLLOWUP", "ACP_REAL_FOLLOWUP_OK"),
            ("ACP_NEW_REQUEST", "ACP_NEW_OK"),
            ("CLI_REAL_LIMIT", "CLI_PARTIAL"),
        ];
        let first_inputs = ["ACP_NEW_REQUEST", "ACP_REAL_LIMIT", "CLI_REAL_LIMIT"];
        let mut next = 0;
        let mut titles = Vec::new();
        while next < expected.len() || titles.len() < first_inputs.len() {
            let deadline = Instant::now() + PATIENCE;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "{}",
                            if next == expected.len() {
                                "every new Session's first input must be titled"
                            } else {
                                "The production daemon did not request the model"
                            }
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
            let messages = request["messages"].as_array().unwrap();
            let (response, finish) = if let Some(input) = title_request_input(&request) {
                titles.push(input);
                ("ACP_TITLE", "stop")
            } else {
                let (prompt, response) = expected[next];
                assert!(messages.iter().any(|message| message["role"] == "user"
                    && message["content"].to_string().contains(prompt)));
                if prompt == "ACP_REAL_FOLLOWUP" {
                    assert!(
                        messages.iter().any(|message| message["role"] == "assistant"
                            && message["content"].to_string().contains("ACP_REAL_OK")),
                        "A follow-up must retain the Session's earlier reply"
                    );
                    assert!(request["messages"].to_string().contains("ACP_REAL_REQUEST"));
                } else if prompt == "ACP_NEW_REQUEST" {
                    let context = request["messages"].to_string();
                    assert!(
                        !context.contains("ACP_REAL_REQUEST")
                            && !context.contains("ACP_REAL_FOLLOWUP")
                            && !context.contains("ACP_REAL_OK"),
                        "A new Session must not inherit the other Session's context"
                    );
                }
                next += 1;
                (
                    response,
                    if prompt.ends_with("_LIMIT") {
                        "length"
                    } else {
                        "stop"
                    },
                )
            };
            let chunk = |delta: Value, finish: Value| {
                json!({"id":"fixture-response","object":"chat.completion.chunk","model":"fixture-model","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}).to_string()
            };
            let data = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                chunk(json!({"role":"assistant","content":response}), Value::Null),
                chunk(json!({}), json!(finish))
            );
            write!(stream.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).unwrap();
            stream.get_mut().flush().unwrap();
        }
        titles.sort();
        assert_eq!(
            titles, first_inputs,
            "each new Session's first input is titled with the configured model"
        );
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
    assert_eq!(peer.commands(), ["compact", "goal", "loop"]);
    assert!(peer.command(40, &id, "/goal").contains("No goal on main"));
    assert!(peer.command(41, &id, "/loop").contains("No loop on main"));
    assert!(peer.command(42, &id, "/compact").is_empty());
    assert_eq!(
        peer.updates
            .iter()
            .filter(|frame| {
                let update = &frame["params"]["update"];
                update["_meta"]["lody"]["activity"]["kind"] == "context_compaction"
                    && update["status"] == "completed"
                    && update.to_string().contains("Skipped:")
            })
            .count(),
        1,
        "an empty conversation reports one native skipped compaction"
    );
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
    let mut manager = Peer::spawn(&mut command);
    manager.initialize();
    manager.send(2, "session/delete", json!({"sessionId":id}));
    let active = manager.response(2);
    assert_eq!(
        active["error"]["data"]["nativeAppError"], "session_active",
        "another connection must not delete an attached Session: {active}"
    );
    manager.close();
    peer.close();
    let mut manager = Peer::spawn(&mut command);
    manager.send(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    );
    let initialized = manager.response(1);
    assert!(
        initialized["result"]["agentCapabilities"]["sessionCapabilities"]
            .get("delete")
            .is_some(),
        "{initialized}"
    );
    manager.send(2, "session/list", json!({}));
    let listed = manager.response(2);
    assert!(
        listed["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["sessionId"] == id),
        "{listed}"
    );
    manager.send(3, "session/delete", json!({"sessionId":id}));
    let deleted = manager.response(3);
    assert_eq!(
        deleted["result"],
        json!({}),
        "{deleted}; persisted main tail read after delete rejection: {:?}",
        (|| -> rusqlite::Result<Vec<(i64, i64, String, String)>> {
            let database = rusqlite::Connection::open_with_flags(
                home.0.join("rust/dimcode.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            database.busy_timeout(Duration::ZERO)?;
            let mut statement = database.prepare(
                "SELECT sequence, last_sequence, kind, body FROM timeline_events \
                 WHERE session_id=?1 AND branch_id='main' ORDER BY sequence DESC LIMIT 32",
            )?;
            statement
                .query_map([&id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect()
        })()
    );
    manager.send(4, "session/list", json!({}));
    let listed = manager.response(4);
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    assert!(sessions.iter().all(|session| session["sessionId"] != id));
    assert!(
        sessions
            .iter()
            .any(|session| session["sessionId"] == other_id)
    );
    manager.send(5, "session/delete", json!({"sessionId":id}));
    let missing = manager.response(5);
    assert_eq!(missing["error"]["code"], -32002, "{missing}");
    manager.close();
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
    // An output-limit stop ends its turn Completed, so the run succeeded even
    // though the failure line still reports the limit and what to do about it.
    assert_eq!(limited.status.code(), Some(0));
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
        notice(wire, event(13,"end","turn","end",json!({"turn_id":"limited","status":"completed","reason":"output_limit"})));
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
fn acp_stdio_answers_end_turn_after_a_recovered_output_limit() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let submit = wire.read("session.submit");
        let input = event(9, "recovered-input", "input", "message", json!({"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"Hello"}]},"mode":"queue"}));
        let notice = |wire: &mut Wire, value: Value| wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":value}));
        notice(wire, event(10,"start","turn","start",json!({"turn_id":"recovered"})));
        notice(wire, event(11,"consumed","input","consumed",json!({"input_id":"recovered-input"})));
        notice(wire, event(12,"limited","step","model-output-end",json!({"turn_id":"recovered","request_id":"first","status":"failed","error":"opaque internal detail","failure":{"kind":"output_limit","attempts":1,"waitedMs":0}})));
        notice(wire, event(13,"continued","step","model-output-end",json!({"turn_id":"recovered","request_id":"second","status":"completed","finish_reason":"end_turn"})));
        notice(wire, event(14,"end","turn","end",json!({"turn_id":"recovered","status":"completed","reason":"end_turn"})));
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
    let recovered = peer.response(3);
    assert_eq!(recovered["result"]["stopReason"], "end_turn", "{recovered}");
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
        wire.2 = true;
        let mut methods = vec!["session.create", "session.history", "session.history.latestTurn"];
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
    let chunks: Vec<&Value> = peer
        .updates
        .iter()
        .map(|frame| &frame["params"]["update"])
        .filter(|update| update["sessionUpdate"] == "agent_message_chunk")
        .collect();
    let text: String = chunks
        .iter()
        .map(|update| update["content"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, "A🦀B");
    assert_eq!(
        chunks
            .iter()
            .map(|update| update["messageId"].clone())
            .collect::<Vec<_>>(),
        vec![json!("model"); chunks.len()],
        "a committed delta and a replacing progress preview name the same message"
    );
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
    for display in [false, true] {
        let home = Home::new();
        let cwd = home.0.clone();
        let daemon = home.daemon(move |wire| {
        let mut methods = METHODS.to_vec();
        if display { methods.extend(["session.history.display", "session.history.detail", "session.history.display.tail", "session.history.display.transcript"]); }
        wire.hello(&methods);
        let method = if display { "session.history.display" } else { "session.history" };
        let field = if display { "items" } else { "events" };
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["expectedCwd"],json!(cwd));
        assert_eq!(attach["params"].get("display"), display.then_some(&json!(true)));
        wire.reply(&attach, json!({"title":"Saved title","titleRevision":2}));
        let first = wire.read(method);
        assert_eq!(first["params"].get("tail"),display.then_some(&json!(true)));
        assert_eq!(first["params"].get("transcript"), display.then_some(&json!(true)));
        assert_eq!(first["params"]["limit"], if display { 32 } else { 64 });
        assert!(first["params"].get("through").is_none());
        wire.reply(
            &first,
            json!({(field):[],"through":9,"nextBefore":9,"nextAfter":9,"done":false}),
        );
        let second = wire.read(method);
        assert_eq!(second["params"]["limit"], 64);
        assert_eq!(second["params"]["through"], 9);
        assert_eq!(second["params"]["before"], 9);
        let text = event(
            3,
            "answer",
            "step",
            "text",
            json!({"turn_id":"turn","request_id":"request","delta":"saved answer"}),
        );
        wire.notice(
            "session.event",
            json!({"sessionId":"saved","branchId":"main","event":text}),
        );
        if display {
            wire.notice("session.event", json!({"sessionId":"saved","branchId":"main","event":event(6,"compact-6","context","compaction-start",json!({"through":5,"trigger":"manual","strategy":"summary"}))}));
            wire.notice("session.display", json!({"sessionId":"saved","branchId":"main","item":event(7,"compact-7","display","compaction",json!({"start":6,"strategy":"summary"}))}));
            wire.notice("session.event", json!({"sessionId":"saved","branchId":"main","event":event(10,"compact-10","context","compaction-start",json!({"through":9,"trigger":"manual","strategy":"summary"}))}));
            for _ in 0..2 {
                wire.notice("session.display", json!({"sessionId":"saved","branchId":"main","item":event(11,"compact-11","display","compaction",json!({"start":10,"strategy":"summary"}))}));
            }
        }
        wire.reply(
            &second,
            json!({(field):if display { vec![text,
                event(6,"compact-6","context","compaction-start",json!({"through":5,"trigger":"manual","strategy":"summary"})),
                event(7,"compact-7","display","compaction",json!({"start":6,"strategy":"summary"})),
                event(8,"compact-8","display","compaction",json!({"strategy":"summary"}))
            ] } else { vec![text] },"through":9,"nextBefore":1,"nextAfter":8,"done":true}),
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
        peer.send(5, "session/prompt", json!({"sessionId":"saved","prompt":[{"type":"audio","data":"AA==","mimeType":"audio/wav"}]}));
        assert!(peer.response(5).get("error").is_some());
        assert!(peer.commands().is_empty());
        assert_eq!(
            peer.updates.len(),
            if display { 8 } else { 3 },
            "live copy of replayed fact must be suppressed"
        );
        assert_eq!(
            peer.updates[0]["params"]["update"]["content"]["text"],
            "saved answer"
        );
        assert!(
            peer.updates
                .iter()
                .any(|update| update["params"]["update"]["title"] == "Saved title")
        );
        if display {
            let activities: Vec<_> = peer
                .updates
                .iter()
                .map(|update| &update["params"]["update"])
                .filter(|update| {
                    update["_meta"]["lody"]["activity"]["kind"] == "context_compaction"
                })
                .collect();
            assert_eq!(
                activities.len(),
                5,
                "replay/live duplicates do not repeat activities: {activities:?}"
            );
            for start in [6, 10] {
                let id = json!(["compaction", start]).to_string();
                let pair: Vec<_> = activities
                    .iter()
                    .filter(|update| update["toolCallId"] == id)
                    .collect();
                assert_eq!(pair.len(), 2, "{pair:?}");
                assert_eq!(pair[0]["status"], "in_progress");
                assert_eq!(pair[1]["status"], "completed");
                assert_eq!(pair[1]["_meta"]["lody"]["activity"]["automatic"], false);
                assert!(
                    pair[1].get("rawOutput").is_none(),
                    "summary has no full diagnostics"
                );
            }
            assert!(
                activities
                    .iter()
                    .any(|update| update["toolCallId"] == r#"["compaction",8]"#
                        && update["status"] == "completed")
            );
        }
        peer.close();
        daemon.join().unwrap();
    }
}

#[test]
fn tui_reports_max_tokens_action_and_accepts_the_next_prompt() {
    for same_failure in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
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
        let mut terminal = Terminal::resume_tui(home.command());
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
/// message is the text the user sees. The refusal is a message of its own, named
/// by the submission of the prompt it refused, never by a model request or an
/// input.
#[test]
fn acp_stdio_reports_a_blocked_prompt_as_refusal_with_the_reason() {
    let home = Home::new();
    let (submitted, received) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        let fact = |wire: &mut Wire, value: Value| {
            wire.notice(
                "session.event",
                json!({"sessionId":"session","branchId":"main","event":value}),
            )
        };
        let submit = wire.read("session.submit");
        let input = event(9, "input-1", "input", "message", json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}));
        wire.reply(&submit, json!({"event":input}));
        fact(wire, input);
        fact(wire, event(10, "turn-start", "turn", "start", json!({"turn_id":"turn"})));
        fact(wire, event(11, "consumed", "input", "consumed", json!({"input_id":"input-1"})));
        fact(wire, event(12, "model-start", "step", "model-start", json!({"turn_id":"turn","request_id":"model"})));
        fact(wire, event(13, "text", "step", "text", json!({"turn_id":"turn","request_id":"model","delta":"Answer"})));
        fact(wire, event(14, "turn-end", "turn", "end", json!({"turn_id":"turn","status":"completed","reason":"end_turn"})));
        for message in ["Hook user: no prompts today", "Hook user: still no"] {
            let submit = wire.read("session.submit");
            submitted.send(submit["params"]["submissionId"].clone()).unwrap();
            wire.send(
                json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,
                "message":message,"data":{"appError":"input_blocked"}}}),
            );
        }
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
    assert_eq!(peer.response(3)["result"]["stopReason"], "end_turn");
    let answer = peer
        .updates
        .iter()
        .map(|frame| frame["params"]["update"].clone())
        .find(|update| update["sessionUpdate"] == "agent_message_chunk")
        .unwrap();
    assert_eq!(answer["messageId"], "model");
    let mut refusals = Vec::new();
    for (request, prompt) in [(4, "Prompt"), (5, "Another prompt")] {
        peer.updates.clear();
        peer.send(
            request,
            "session/prompt",
            json!({"sessionId":"session","prompt":[{"type":"text","text":prompt}]}),
        );
        let response = peer.response(request);
        assert_eq!(
            response["result"]["stopReason"],
            "refusal",
            "{response}: {updates:?}",
            updates = peer.updates
        );
        let submission = received.recv_timeout(PATIENCE).unwrap();
        let chunk = peer
            .updates
            .iter()
            .map(|frame| frame["params"]["update"].clone())
            .find(|update| update["sessionUpdate"] == "agent_message_chunk")
            .expect("the block's message is text");
        assert_eq!(
            chunk["messageId"],
            json!(["gateway", "refusal", submission]).to_string(),
            "the refusal is the message of the submission it refused"
        );
        assert_ne!(chunk["messageId"], answer["messageId"]);
        assert_ne!(chunk["messageId"], "input-1");
        assert!(
            chunk["content"]["text"]
                .as_str()
                .unwrap()
                .starts_with("Hook user:"),
            "the refusal carries the block's message: {chunk}"
        );
        refusals.push(chunk["messageId"].clone());
    }
    assert_ne!(
        refusals[0], refusals[1],
        "one refusal per refused operation"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_load_bounds_a_long_turn_and_marks_the_omitted_prefix() {
    let home = Home::new();
    let cwd = home.0.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["expectedCwd"],json!(cwd));
        wire.reply(&attach, json!({"sequence":1000}));
        for page in 0..1_u64 {
            let read = wire.read("session.history");
            assert_eq!(read["params"]["limit"], 64);
            if page == 0 {
                assert_eq!(read["params"]["latestTurn"], true);
                wire.notice("session.event", json!({"sessionId":"saved","branchId":"main","event":event(1001,"live","step","text",json!({"turn_id":"turn","request_id":"reply","delta":"live"}))}));
            } else {
                assert_eq!(read["params"]["before"], 1000-page*100);
                assert_eq!(read["params"]["through"], 1000);
                assert!(read["params"].get("latestTurn").is_none());
            }
            let sequence = 900-page*100;
            wire.reply(&read, json!({"events":[event(sequence,&format!("e{sequence}"),"step","text",json!({"turn_id":"turn","request_id":"reply","delta":format!("p{page}")}))],"through":1000,"nextBefore":sequence,"nextAfter":sequence,"done":true}));
        }
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    let loaded = peer.response(2);
    assert_eq!(
        loaded["result"]["_meta"]["dim/history"],
        json!({"through":1000,"before":900,"complete":false}),
        "{loaded}"
    );
    let text: String = peer
        .updates
        .iter()
        .filter_map(|frame| frame["params"]["update"]["content"]["text"].as_str())
        .collect();
    assert_eq!(text, "p0");
    let live = peer.frame();
    assert_eq!(live["params"]["update"]["content"]["text"], "live");
    peer.close();
    daemon.join().unwrap();
}

/// #1792: every chunk of one message carries that message's id, and a message
/// replayed from the store keeps the id it streamed with.
#[test]
fn acp_load_gives_a_message_the_same_id_on_replay_and_live_streaming() {
    let home = Home::new();
    let cwd = home.0.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let attach = wire.read("session.attach");
        assert_eq!(attach["params"]["expectedCwd"], json!(cwd));
        wire.reply(&attach, json!({"sequence":1000}));
        let read = wire.read("session.history");
        assert_eq!(read["params"]["latestTurn"], true);
        wire.notice("session.event", json!({"sessionId":"saved","branchId":"main","event":event(1001,"live","step","text",json!({"turn_id":"turn","request_id":"reply","delta":" live"}))}));
        wire.reply(&read,json!({"events":[
            event(1,"input","input","message",json!({"user":{"content":[{"type":"text","text":"question"}]},"mode":"queue"})),
            event(2,"start","turn","start",json!({"turn_id":"turn"})),
            event(3,"consumed","input","consumed",json!({"input_id":"input"})),
            event(4,"thought","step","thinking",json!({"turn_id":"turn","request_id":"reply","delta":"replayed thought"})),
            event(5,"text","step","text",json!({"turn_id":"turn","request_id":"reply","delta":"replayed answer"}))
        ],"through":1000,"nextAfter":5,"done":true}));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    let loaded = peer.response(2);
    assert!(loaded.get("error").is_none(), "{loaded}");
    let update = |kind: &str| {
        peer.updates
            .iter()
            .map(|frame| frame["params"]["update"].clone())
            .filter(|update| update["sessionUpdate"] == kind)
            .collect::<Vec<_>>()
    };
    let user = update("user_message_chunk");
    assert_eq!(user.len(), 1, "{user:?}");
    assert_eq!(
        user[0]["messageId"], "input",
        "the operator's message is the input it came from"
    );
    let text = update("agent_message_chunk");
    assert_eq!(text.len(), 1, "{text:?}");
    assert_eq!(text[0]["content"]["text"], "replayed answer");
    assert_eq!(
        text[0]["messageId"], "reply",
        "the answer is the request that produced it"
    );
    let thought = update("agent_thought_chunk");
    assert_eq!(thought.len(), 1, "{thought:?}");
    assert_eq!(
        thought[0]["messageId"], "reply",
        "reasoning belongs to the message it reasoned for"
    );
    let live = peer.frame();
    assert_eq!(live["params"]["update"]["content"]["text"], " live");
    assert_eq!(
        live["params"]["update"]["messageId"], text[0]["messageId"],
        "the live continuation of a replayed message keeps that message's id"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_load_keeps_an_orphan_result_without_reading_original_arguments() {
    let home = Home::new();
    let cwd = home.0.clone();
    let daemon=home.daemon(move |wire| {
        let mut methods=METHODS.to_vec(); methods.push("session.methods"); wire.hello(&methods);
        let attach=wire.read("session.attach"); assert_eq!(attach["params"]["expectedCwd"],json!(cwd)); wire.reply(&attach,json!({"sequence":1000}));
        let history=wire.read("session.history"); assert_eq!(history["params"]["latestTurn"],true);
        let mut events:Vec<_>=(900..999).map(|sequence|event(sequence,&format!("text-{sequence}"),"step","text",json!({"turn_id":"turn","request_id":format!("r-{sequence}"),"delta":"text"}))).collect();
        events.push(event(999,"result","step","tool-result",json!({"turn_id":"turn","request_id":"old-request","results":[{"call_id":"old-call","result":{"content":"saved result"}}]})));
        wire.reply(&history,json!({"events":events,"through":1000,"nextBefore":900,"nextAfter":999,"done":true}));
        let methods=wire.read("session.methods");
        wire.reply(&methods,json!({"methods":[{"name":"session.tool.get","mutating":false}]}));
        wire.2=true;
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    let loaded = peer.response(2);
    assert!(loaded.get("error").is_none(), "{loaded}");
    let updates: Vec<_> = peer
        .updates
        .iter()
        .map(|frame| &frame["params"]["update"])
        .filter(|update| {
            update["toolCallId"].as_str() == Some(r#"["main","old-request","old-call"]"#)
        })
        .collect();
    assert_eq!(updates.len(), 2, "{updates:?}");
    assert_eq!(updates[0]["sessionUpdate"], "tool_call");
    assert_eq!(updates[0]["rawInput"], Value::Null);
    assert_eq!(updates[0]["title"], "old-call");
    assert_eq!(updates[1]["status"], "completed");
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_does_not_offer_fork_without_the_native_turn_read() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let mut methods = METHODS.to_vec();
        methods.push("session.fork");
        wire.hello(&methods);
        wire.hold();
    });
    let mut peer = Peer::start(&home);
    peer.send(
        1,
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    );
    let initialized = peer.response(1);
    assert!(
        initialized["result"]["agentCapabilities"]["sessionCapabilities"]
            .get("fork")
            .is_none(),
        "{initialized}"
    );
    peer.send(
        2,
        "session/fork",
        json!({"sessionId":"source","cwd":home.0,"mcpServers":[]}),
    );
    let refused = peer.response(2);
    assert_eq!(refused["error"]["code"], -32001);
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session.turn.get"),
        "{refused}"
    );
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_load_and_resume_report_unavailable_sessions_without_listing_history() {
    let home = Home::new();
    let cwd = home.0.clone();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        for _ in 0..2 {
            let attach = wire.read("session.attach");
            assert_eq!(attach["params"]["expectedCwd"], json!(cwd));
            wire.send(json!({"jsonrpc":"2.0","id":attach["id"],"error":{"code":-32001,"message":"missing is closed","data":{"appError":"session_closed"}}}));
            let detach = wire.read("session.detach");
            wire.reply(&detach, json!({}));
        }
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    for (id, method) in [(2, "session/load"), (3, "session/resume")] {
        peer.send(
            id,
            method,
            json!({"sessionId":"missing","cwd":home.0,"mcpServers":[]}),
        );
        let response = peer.response(id);
        let expected =
            serde_json::to_value(acp::Error::resource_not_found(Some("missing".into()))).unwrap();
        assert_eq!(response["error"], expected);
    }
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_load_materializes_user_and_tool_file_images_in_order() {
    let home = Home::new();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        let source = |id| json!({"type":"file","mediaType":"image/png","fileId":id});
        wire.reply(&history, json!({"events":[
            event(1,"input","input","message",json!({"user":{"content":[{"type":"text","text":"before"},{"type":"image","source":source("user-image")},{"type":"text","text":"<dim-skill name=\"commit\">\n# Commit\n</dim-skill>"},{"type":"text","text":"after"}]},"mode":"queue"})),
            event(2,"start","turn","start",json!({"turn_id":"turn"})),
            event(3,"consumed","input","consumed",json!({"input_id":"input"})),
            event(4,"result","step","tool-result",json!({"turn_id":"turn","request_id":"request","results":[{"call_id":"call","result":{"content":[{"type":"text","text":"caption"},{"type":"image","source":source("tool-image")}]}}]}))
        ],"through":4,"nextAfter":4,"done":true}));
        for file in ["user-image", "tool-image"] {
            for (offset, data) in [(0, "aW1hZ2U="), (5, "")] {
                let read = wire.read("attachment.read");
                assert_eq!(read["params"], json!({"sessionId":"saved","fileId":file,"offset":offset}));
                wire.reply(&read, json!({"data":data,"eof":data.is_empty()}));
            }
        }
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(
        2,
        "session/load",
        json!({"sessionId":"saved","cwd":home.0,"mcpServers":[]}),
    );
    let loaded = peer.response(2);
    assert!(loaded.get("error").is_none(), "{loaded}");
    let updates: Vec<_> = peer
        .updates
        .iter()
        .map(|frame| &frame["params"]["update"])
        .collect();
    let user: Vec<_> = updates
        .iter()
        .filter(|update| update["sessionUpdate"] == "user_message_chunk")
        .collect();
    assert_eq!(user.len(), 4);
    assert_eq!(user[0]["content"]["text"], "before");
    assert_eq!(
        user[1]["content"],
        json!({"type":"image","mimeType":"image/png","data":"aW1hZ2U="})
    );
    assert_eq!(
        user[2]["content"]["text"], "\nSkill loaded: commit",
        "a loaded Skill replays as one line of the operator's message"
    );
    assert_eq!(user[3]["content"]["text"], "after");
    assert_eq!(
        user.iter()
            .map(|update| update["messageId"].clone())
            .collect::<Vec<_>>(),
        vec![json!("input"); 4],
        "every block of the message, its image and its Skill line included, \
         names the input it belongs to"
    );
    let tool = updates
        .iter()
        .find(|update| update["sessionUpdate"] == "tool_call_update")
        .unwrap();
    assert_eq!(tool["content"][0]["content"]["text"], "caption");
    assert_eq!(tool["content"][1]["content"], user[1]["content"]);
    peer.close();
    daemon.join().unwrap();
}

#[test]
fn acp_stdio_names_each_tool_call_with_its_object_kind_and_location() {
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let daemon = home.daemon(move |wire| {
        wire.hello(METHODS);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, json!({}));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let fact = |wire: &mut Wire, value: Value| {
            wire.notice(
                "session.event",
                json!({"sessionId":"session","branchId":"main","event":value}),
            )
        };
        let submit = wire.read("session.submit");
        let input = event(
            9,
            "input",
            "input",
            "message",
            json!({"submission_id":submit["params"]["submissionId"],"user":submit["params"]["user"],"mode":"queue"}),
        );
        wire.reply(&submit, json!({"event":input}));
        fact(wire, input);
        fact(wire, event(10, "start", "turn", "start", json!({"turn_id":"turn"})));
        fact(wire, event(11, "consumed", "input", "consumed", json!({"input_id":"input"})));
        // Dim announces a call as the model starts writing it, so the first
        // message names only the tool; the arguments arrive with the next one.
        fact(wire, event(12, "read-start", "step", "tool-use-start", json!({"turn_id":"turn","request_id":"model","call_id":"read","name":"read"})));
        fact(wire, event(13, "read-use", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"read","name":"read","input":{"path":"src/parser.rs","offset":42,"limit":20}}})));
        fact(wire, event(14, "edit", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"edit","name":"edit","input":{"path":"src/parser.rs","edits":[{"oldText":"let a = 1;","newText":"let a = 2;"},{"oldText":"let b = 3;","newText":"let b = 4;"}]}}})));
        fact(wire, event(15, "edit-done", "step", "tool-result", json!({"turn_id":"turn","request_id":"model","call_event_id":"edit","results":[{"call_id":"edit","result":{"content":"Edited src/parser.rs"}}]})));
        fact(wire, event(29, "write", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"write","name":"write","input":{"path":"notes.md","content":"new file\n"}}})));
        fact(wire, event(30, "write-done", "step", "tool-result", json!({"turn_id":"turn","request_id":"model","call_event_id":"write","results":[{"call_id":"write","result":{"content":"Wrote notes.md","structuredContent":{"path":"notes.md","bytesWritten":9,"revision":"r1","diff":"--- /dev/null\n+++ notes.md\n@@ -0,0 +1 @@\n+new file\n"}}}]})));
        fact(wire, event(31, "exec", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"exec","name":"exec","input":{"command":"cargo test --locked\n  -p dim-acp","title":"Run dim-acp tests"}}})));
        fact(wire, event(32, "grep", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"grep","name":"grep","input":{"pattern":"Projection","path":"crates/dim-acp"}}})));
        fact(wire, event(33, "fetch", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"fetch","name":"web_fetch","input":{"url":"https://example.com/doc"}}})));
        fact(wire, event(34, "unknown", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"unknown","name":"image_understand","input":{"image":"x"}}})));
        fact(wire, event(35, "rewrite", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"rewrite","name":"write","input":{"path":"notes.md","content":"newer text\n"}}})));
        fact(wire, event(36, "rewrite-done", "step", "tool-result", json!({"turn_id":"turn","request_id":"model","call_event_id":"rewrite","results":[{"call_id":"rewrite","result":{"content":"Wrote notes.md","structuredContent":{"path":"notes.md","bytesWritten":11,"revision":"r2","diff":"--- notes.md\n+++ notes.md\n@@ -1 +1 @@\n-new file\n+newer text\n"}}}]})));
        fact(wire, event(37, "huge", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"huge","name":"read","input":{"path":"src/parser.rs","offset":4294967296u64}}})));
        fact(wire, event(38, "overflow", "step", "tool-use", json!({"turn_id":"turn","request_id":"model","call":{"id":"overflow","name":"read","input":{"path":"src/parser.rs","offset":u64::MAX,"limit":2}}})));
        fact(wire, event(39, "after", "step", "text", json!({"turn_id":"turn","request_id":"model","delta":"still here"})));
        fact(wire, event(40, "end", "turn", "end", json!({"turn_id":"turn","status":"completed","reason":"end_turn"})));
        detached_eof(wire);
    });
    let mut peer = Peer::start(&home);
    peer.initialize();
    peer.send(2, "session/new", json!({"cwd":home.0,"mcpServers":[]}));
    peer.response(2);
    peer.send(
        3,
        "session/prompt",
        json!({"sessionId":"session","prompt":[{"type":"text","text":"Read the parser"}]}),
    );
    let response = peer.response(3);
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    let updates: Vec<_> = peer
        .updates
        .iter()
        .map(|frame| &frame["params"]["update"])
        .filter(|update| update["toolCallId"].is_string())
        .collect();
    let id = |name: &str| json!(["main", "model", name]).to_string();
    // The message that carries a call's input is the one that names it.
    let announced = |name: &str| {
        updates
            .iter()
            .find(|update| {
                update["toolCallId"] == json!(id(name)) && update["rawInput"].is_object()
            })
            .unwrap_or_else(|| panic!("no announced call for {name}: {updates:?}"))
    };
    let result = |name: &str| {
        updates
            .iter()
            .find(|update| {
                update["toolCallId"] == json!(id(name))
                    && matches!(update["status"].as_str(), Some("completed" | "failed"))
            })
            .unwrap_or_else(|| panic!("no result for {name}: {updates:?}"))
    };
    let path = cwd.join("src/parser.rs").to_str().unwrap().to_owned();

    let first = updates
        .iter()
        .find(|update| update["toolCallId"] == json!(id("read")))
        .unwrap();
    assert_eq!(
        first["title"], "read",
        "no call has its object yet: the arguments are still streaming"
    );
    let with_arguments = announced("read");
    assert_eq!(with_arguments["title"], "Read src/parser.rs:42-61");
    assert_eq!(with_arguments["kind"], "read");
    assert_eq!(
        with_arguments["locations"],
        json!([{"path":path,"line":42}]),
        "a line range names the file at its first line"
    );
    assert_eq!(with_arguments["rawInput"]["offset"], 42);

    assert_eq!(announced("edit")["title"], "Edit src/parser.rs");
    assert_eq!(announced("edit")["kind"], "edit");
    assert_eq!(announced("edit")["locations"], json!([{"path":path}]));
    let edited = result("edit");
    assert_eq!(
        edited["content"],
        json!([
            {"type":"diff","path":path,"oldText":"let a = 1;","newText":"let a = 2;"},
            {"type":"diff","path":path,"oldText":"let b = 3;","newText":"let b = 4;"},
            {"type":"content","content":{"type":"text","text":"Edited src/parser.rs"}}
        ]),
        "each replacement is its own diff, before the tool's own output"
    );
    assert_eq!(announced("write")["title"], "Write notes.md");
    assert_eq!(announced("write")["kind"], "edit");
    let written = cwd.join("notes.md").to_str().unwrap().to_owned();
    assert_eq!(
        result("write")["content"],
        json!([
            {"type":"diff","path":written,"newText":"new file\n"},
            {"type":"content","content":{"type":"text","text":"Wrote notes.md"}}
        ]),
        "a created file is a diff with no old text, as ACP defines a new file"
    );
    assert_eq!(
        result("rewrite")["content"],
        json!([{"type":"content","content":{"type":"text","text":"Wrote notes.md"}}]),
        "an overwrite states no old text, so it must not read as a new file"
    );
    assert_eq!(
        announced("huge")["locations"],
        json!([{"path":path}]),
        "a line ACP cannot carry as a u32 is left out, not truncated to another line"
    );
    let overflow = announced("overflow");
    assert_eq!(
        overflow["title"],
        format!("Read src/parser.rs:{}", u64::MAX),
        "an end past the largest offset is left out, not wrapped"
    );
    assert_eq!(overflow["locations"], json!([{"path":path}]));
    assert!(
        peer.updates.iter().any(|frame| {
            let update = &frame["params"]["update"];
            update["sessionUpdate"] == "agent_message_chunk"
                && update["content"]["text"] == "still here"
        }),
        "a read at the largest offset leaves the projection answering: {:?}",
        peer.updates
    );
    assert_eq!(announced("exec")["title"], "Run dim-acp tests");
    assert_eq!(announced("exec")["kind"], "execute");
    assert_eq!(
        announced("grep")["title"],
        "Search Projection in crates/dim-acp",
        "a search names its pattern and the place it searches"
    );
    assert_eq!(announced("grep")["kind"], "search");
    assert_eq!(
        announced("grep")["locations"],
        json!([{"path":cwd.join("crates/dim-acp").to_str().unwrap()}])
    );
    assert_eq!(announced("fetch")["title"], "https://example.com/doc");
    assert_eq!(announced("fetch")["kind"], "fetch");
    assert_eq!(announced("unknown")["title"], "image_understand");
    assert_eq!(announced("unknown")["kind"], "other");
    peer.close();
    daemon.join().unwrap();
}
