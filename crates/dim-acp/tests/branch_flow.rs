#![cfg(unix)]

use dim_acp::{AcpGateway, Io};
use dim_client::{Client, ConnectionConfig, DaemonProfile, Server};
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    sync::oneshot,
};

async fn read_frame(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Option<Value> {
    let mut line = String::new();
    let read = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    (read > 0).then(|| serde_json::from_str(&line).unwrap())
}

async fn frame(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    read_frame(reader).await.expect("the peer stays open")
}

/// The reply to `id`, skipping the notifications that may come first.
async fn reply(reader: &mut (impl AsyncBufReadExt + Unpin), id: u64) -> Value {
    loop {
        let frame = frame(reader).await;
        if frame["id"] == json!(id) {
            return frame;
        }
    }
}

async fn send(writer: &mut (impl AsyncWriteExt + Unpin), value: Value) {
    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    writer.flush().await.unwrap();
}

/// An ACP call names no line: the gateway reads the Session's default branch
/// once and every call it makes reaches it (design §4, §9, §14).
#[tokio::test]
async fn a_gateway_call_without_a_line_targets_the_default_branch() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let cwd = home.path().to_str().unwrap().to_owned();
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (submitted, submit_seen) = oneshot::channel();
    let daemon_cwd = cwd.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.attach","session.attach.expectedCwd","session.detach","session.history","session.history.latestTurn","session.input_lookup","session.submit","session.stop","session.branches","session.methods","session.input.pending","session.question.pending","session.turn.get","session.exec.pending"]}})).await;
        let mut submitted = Some(submitted);
        while let Some(request) = read_frame(&mut read).await {
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let branch = request["params"]["branchId"]
                .as_str()
                .unwrap_or("main")
                .to_owned();
            let through = request["params"]["through"].as_u64().unwrap_or(2);
            let id = request["id"].clone();
            match method.as_str() {
                "session.attach" => {
                    assert_eq!(request["params"]["expectedCwd"], json!(daemon_cwd));
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "sequence":2,"cwd":daemon_cwd,
                        "model":{"providerId":"fixture","modelId":"fixture-model"}}}),
                    )
                    .await;
                }
                "session.branches" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{
                        "defaultBranchId":"line-1",
                        "branches":[
                            {"branchId":"main","state":"open","head":2},
                            {"branchId":"line-1","parentBranchId":"main","branchPoint":1,"state":"open","head":2}]}})).await;
                }
                "session.history" => {
                    assert_eq!(branch, "line-1", "the recovery reads the default branch");
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "events":[],"through":2,"nextBefore":null,"nextAfter":2}}),
                    )
                    .await;
                }
                "session.methods" => {
                    let methods: Vec<_> = [
                        "session.input.pending",
                        "session.question.pending",
                        "session.exec.pending",
                        "session.turn.get",
                    ]
                    .into_iter()
                    .map(|name| json!({"name":name,"mutating":false}))
                    .collect();
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"methods":methods}}),
                    )
                    .await;
                }
                "session.question.pending" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "branchId":branch,"through":through,"batches":[],"next":null}}),
                    )
                    .await;
                }
                "session.exec.pending" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "branchId":branch,"through":through,"tasks":[],"next":null}}),
                    )
                    .await;
                }
                "session.input.pending" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "branchId":branch,"through":through,"inputs":[],"next":null}}),
                    )
                    .await;
                }
                "session.turn.get" => {
                    assert_eq!(branch, "line-1", "the turn read is the default branch's");
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "branchId":branch,"through":through,"turn":null,"output":null}}),
                    )
                    .await;
                }
                "session.submit" => {
                    assert_eq!(
                        request["params"]["branchId"],
                        json!("line-1"),
                        "a prompt with no line reaches the Session's default branch"
                    );
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{"event":{
                        "id":"in-1","sequence":3,"create_at":1,"type":"input","name":"message",
                        "data":{"user":{"content":[{"type":"text","text":"hello"}]},"mode":"queue"}}}})).await;
                    submitted.take().unwrap().send(()).unwrap();
                }
                "session.detach" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
                    break;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let client = Client::connect(&ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 3,
    })
    .await
    .unwrap();
    let (peer, transport) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(transport);
    let (gateway, finished) = AcpGateway::new(
        client,
        Io {
            reader: Box::new(reader),
            writer: Box::new(writer),
        },
        None,
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let root = rutis::Ctx::root().unwrap();
    let mounted = root.plugin(gateway);
    (&mounted).await.unwrap();
    let (read, mut write) = tokio::io::split(peer);
    let mut read = BufReader::new(read);
    send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let initialize = reply(&mut read, 1).await;
    assert!(initialize["error"].is_null(), "{initialize}");
    send(&mut write,json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":"session","cwd":cwd,"mcpServers":[]}})).await;
    let loaded = reply(&mut read, 2).await;
    assert!(loaded["error"].is_null(), "{loaded}");
    send(&mut write,json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"session","prompt":[{"type":"text","text":"hello"}]}})).await;
    tokio::time::timeout(Duration::from_secs(5), submit_seen)
        .await
        .unwrap()
        .unwrap();
    // The prompt is answered on the line the default branch names; the reply
    // itself is not this test's subject.
    drop(read);
    let _ = write.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), finished).await;
    mounted.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    server.await.unwrap();
}

/// The option a session reply or a config update states for `id`.
fn option<'a>(value: &'a Value, id: &str) -> &'a Value {
    value
        .pointer("/result/configOptions")
        .or_else(|| value.pointer("/params/update/configOptions"))
        .and_then(Value::as_array)
        .expect("config options")
        .iter()
        .find(|option| option["id"] == json!(id))
        .expect("the named option")
}

/// The next `session/update` that carries config options.
async fn config_update(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    loop {
        let frame = frame(reader).await;
        if frame["params"]["update"]["sessionUpdate"] == json!("config_option_update") {
            return frame;
        }
    }
}

/// The model is the loaded line's own, at or below its cursor: the attach
/// reply's model is the root's (design §14), so a Session whose conversation is
/// on a line shows the configuration that line adopted, and only the root takes
/// the reply's. The reply names the line and the directory only answers for a
/// server that does not carry the field (design §14).
#[tokio::test]
async fn a_gateway_load_folds_the_default_branchs_cursor() {
    for (line, cursor, listed_line) in [
        ("main", 2, "main"),
        ("line-1", 5, "line-1"),
        ("line-1", 5, "main"),
    ] {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let endpoint = home.path().join("daemon.sock");
        let cwd = home.path().to_str().unwrap().to_owned();
        let listener = UnixListener::bind(&endpoint).unwrap();
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
        let daemon_cwd = cwd.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let hello = frame(&mut read).await;
            send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.attach","session.attach.expectedCwd","session.branches","session.detach","session.exec.pending","session.history","session.history.latestTurn","session.input.pending","session.input_lookup","session.methods","session.model.get","session.model.update","session.question.pending","session.stop","session.submit","session.summaries","session.turn.get","provider.models.query"]}})).await;
            let mut listed = 0;
            while let Some(request) = read_frame(&mut read).await {
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                let branch = request["params"]["branchId"]
                    .as_str()
                    .unwrap_or(line)
                    .to_owned();
                let through = request["params"]["through"].as_u64().unwrap_or(cursor);
                let id = request["id"].clone();
                match method.as_str() {
                    "session.attach" => {
                        assert_eq!(request["params"]["expectedCwd"], json!(daemon_cwd));
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"sequence":2,"cwd":daemon_cwd,"model":{"providerId":"fixture","modelId":"A"},"defaultBranch":{"branchId":line,"sequence":cursor}}})).await;
                    }
                    "session.branches" => {
                        let mut branches = vec![json!({"branchId":"main","state":"open","head":2})];
                        if line != "main" {
                            branches.push(json!({"branchId":line,"parentBranchId":"main","branchPoint":1,"state":"open","head":2}));
                        }
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"defaultBranchId":listed_line,"branches":branches}})).await;
                    }
                    "session.history" => {
                        assert_eq!(branch, line, "the recovery reads the default branch");
                        assert!(
                            request["params"].get("before").is_none(),
                            "a line's configuration is not paged out of its history"
                        );
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"events":[],"through":cursor,"nextBefore":1,"nextAfter":cursor}})).await;
                    }
                    "session.model.get" => {
                        assert_eq!(branch, line, "a line's configuration is its own");
                        assert_eq!(through, cursor, "read at the line's own cursor");
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":line,"through":cursor,"configuration":{"model":{"providerId":"fixture","modelId":"B"}}}})).await;
                    }
                    "provider.models.query" => {
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"models":[{"providerId":"fixture","model":{"id":"A","name":"A"}},{"providerId":"fixture","model":{"id":"B","name":"B"}}],"next":null}})).await;
                    }
                    "session.summaries" => {
                        send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":[]})).await;
                        if listed == 0 {
                            listed = 1;
                            let sequence = cursor + 1;
                            send(&mut write,json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":line,"event":{"id":format!("configured-{sequence}"),"sequence":sequence,"create_at":1,"type":"context","name":"configured","data":{"timeline":line,"revision":"r1","provider_id":"fixture","model_id":"B"}}}})).await;
                        }
                    }
                    "session.methods" => {
                        let methods: Vec<_> = [
                            "session.input.pending",
                            "session.question.pending",
                            "session.exec.pending",
                            "session.turn.get",
                        ]
                        .into_iter()
                        .map(|name| json!({"name":name,"mutating":false}))
                        .collect();
                        send(
                            &mut write,
                            json!({"jsonrpc":"2.0","id":id,"result":{"methods":methods}}),
                        )
                        .await;
                    }
                    "session.exec.pending" => {
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"tasks":[],"next":null}})).await;
                    }
                    "session.input.pending" => {
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"inputs":[],"next":null}})).await;
                    }
                    "session.question.pending" => {
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"batches":[],"next":null}})).await;
                    }
                    "session.turn.get" => {
                        send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"turn":null,"output":null}})).await;
                    }
                    "session.detach" => {
                        send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
                        break;
                    }
                    other => panic!("unexpected {other}: {request}"),
                }
            }
        });
        let client = Client::connect(&ConnectionConfig {
            endpoint,
            server: Server {
                program: "unused".into(),
                embedded: false,
            },
            profile: Some(DaemonProfile::Product),
            client_kind: "other",
            client_name: "AcpGateway",
            protocol_version: 3,
        })
        .await
        .unwrap();
        let (peer, transport) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(transport);
        let (gateway, finished) = AcpGateway::new(
            client,
            Io {
                reader: Box::new(reader),
                writer: Box::new(writer),
            },
            None,
            env!("CARGO_PKG_VERSION").to_owned(),
        );
        let root = rutis::Ctx::root().unwrap();
        let mounted = root.plugin(gateway);
        (&mounted).await.unwrap();
        let (read, mut write) = tokio::io::split(peer);
        let mut read = BufReader::new(read);
        send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
        let initialize = reply(&mut read, 1).await;
        assert!(initialize["error"].is_null(), "{initialize}");
        send(&mut write,json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":"session","cwd":cwd,"mcpServers":[]}})).await;
        let loaded = reply(&mut read, 2).await;
        assert!(loaded["error"].is_null(), "{loaded}");
        // The reply's model is the root's, so only the root shows it; a line
        // shows the configuration its own window adopted at or below the cursor.
        assert_eq!(
            option(&loaded, "model")["currentValue"],
            json!(if line == "main" { "A" } else { "B" }),
            "the load shows the model of the line its cursor names"
        );
        send(
            &mut write,
            json!({"jsonrpc":"2.0","id":3,"method":"session/list","params":{}}),
        )
        .await;
        let update = config_update(&mut read).await;
        assert_eq!(
            option(&update, "model")["currentValue"],
            json!("B"),
            "a fact after the cursor reaches the options: {update}"
        );
        drop(read);
        let _ = write.shutdown().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), finished).await;
        mounted.dispose().await.unwrap();
        root.shutdown().await.unwrap();
        server.await.unwrap();
    }
}

/// The production display transcript omits `context/configured`: a line's
/// adopted model comes from `session.model.get` at the cursor its attach
/// reply names, and a superseded model update refreshes from the same read
/// rather than the reply's root configuration.
#[tokio::test]
async fn a_gateway_line_reads_its_model_when_display_omits_the_configuration() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let cwd = home.path().to_str().unwrap().to_owned();
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let daemon_cwd = cwd.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.attach","session.attach.expectedCwd","session.branches","session.detach","session.exec.pending","session.history","session.history.display","session.history.detail","session.history.display.tail","session.history.display.transcript","session.history.latestTurn","session.input.pending","session.input_lookup","session.methods","session.model.get","session.model.update","session.question.pending","session.stop","session.submit","session.summaries","session.turn.get","provider.models.query"]}})).await;
        let mut attachments = 0;
        while let Some(request) = read_frame(&mut read).await {
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let id = request["id"].clone();
            match method.as_str() {
                "session.attach" => {
                    attachments += 1;
                    let (sequence, model) = if attachments == 1 { (5, "A") } else { (6, "C") };
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"sequence":2,"cwd":daemon_cwd,"model":{"providerId":"fixture","modelId":model},"defaultBranch":{"branchId":"line-1","sequence":sequence}}})).await;
                }
                "session.branches" => {
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"defaultBranchId":"line-1","branches":[{"branchId":"main","state":"open","head":2},{"branchId":"line-1","parentBranchId":"main","branchPoint":1,"state":"open","head":5}]}})).await;
                }
                "session.history.display" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"items":[],"through":5,"nextBefore":1,"nextAfter":5,"done":true}})).await;
                }
                "session.model.get" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    let through = request["params"]["through"].as_u64().unwrap_or(6);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":"line-1","through":through,"configuration":{"model":{"providerId":"fixture","modelId":"B"}}}})).await;
                }
                "session.methods" => {
                    let methods: Vec<_> = [
                        "session.input.pending",
                        "session.question.pending",
                        "session.exec.pending",
                        "session.turn.get",
                        "session.model.update",
                    ]
                    .into_iter()
                    .map(|name| json!({"name":name,"mutating":name == "session.model.update"}))
                    .collect();
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"methods":methods}}),
                    )
                    .await;
                }
                "provider.models.query" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"models":[
                        {"providerId":"fixture","model":{"id":"A","name":"A"}},
                        {"providerId":"fixture","model":{"id":"B","name":"B"}},
                        {"providerId":"fixture","model":{"id":"C","name":"C"}}],"next":null}}),
                    )
                    .await;
                }
                "session.model.update" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"Configuration was superseded by another update","data":{"appError":"configuration_superseded"}}})).await;
                }
                "session.summaries" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":[]})).await;
                }
                "session.question.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"batches":[],"next":null}})).await;
                }
                "session.input.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"inputs":[],"next":null}})).await;
                }
                "session.exec.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"tasks":[],"next":null}})).await;
                }
                "session.turn.get" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"turn":null,"output":null}})).await;
                }
                "session.detach" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
                    break;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let client = Client::connect(&ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 3,
    })
    .await
    .unwrap();
    let (peer, transport) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(transport);
    let (gateway, finished) = AcpGateway::new(
        client,
        Io {
            reader: Box::new(reader),
            writer: Box::new(writer),
        },
        None,
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let root = rutis::Ctx::root().unwrap();
    let mounted = root.plugin(gateway);
    (&mounted).await.unwrap();
    let (read, mut write) = tokio::io::split(peer);
    let mut read = BufReader::new(read);
    send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let initialize = reply(&mut read, 1).await;
    assert!(initialize["error"].is_null(), "{initialize}");
    send(&mut write,json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":"session","cwd":cwd,"mcpServers":[]}})).await;
    let loaded = reply(&mut read, 2).await;
    assert!(loaded["error"].is_null(), "{loaded}");
    assert_eq!(
        option(&loaded, "model")["currentValue"],
        json!("B"),
        "the load shows the model `session.model.get` adopted for the line, not the reply's root model"
    );
    send(&mut write,json!({"jsonrpc":"2.0","id":3,"method":"session/set_config_option","params":{"sessionId":"session","configId":"model","value":"A"}})).await;
    let refused = reply(&mut read, 3).await;
    assert_eq!(
        refused["error"]["data"]["nativeAppError"],
        json!("configuration_superseded"),
        "{refused}"
    );
    let update = config_update(&mut read).await;
    assert_eq!(
        option(&update, "model")["currentValue"],
        json!("B"),
        "the refresh keeps the line's own model instead of reseeding the reply's root model: {update}"
    );
    drop(read);
    let _ = write.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), finished).await;
    mounted.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    server.await.unwrap();
}

/// A superseded refresh re-reads the line's configuration at its own cutoff:
/// a configuration older than that cutoff arrives late and must not replace
/// the refreshed model.
#[tokio::test]
async fn a_gateway_refresh_keeps_its_cutoff_against_a_delayed_configuration() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let cwd = home.path().to_str().unwrap().to_owned();
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let daemon_cwd = cwd.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.attach","session.attach.expectedCwd","session.branches","session.detach","session.exec.pending","session.history","session.history.display","session.history.detail","session.history.display.tail","session.history.display.transcript","session.history.latestTurn","session.input.pending","session.input_lookup","session.list","session.methods","session.model.get","session.model.update","session.question.pending","session.stop","session.submit","session.summaries","session.turn.get","provider.models.query"]}})).await;
        let mut attachments = 0;
        let mut delayed = false;
        while let Some(request) = read_frame(&mut read).await {
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let id = request["id"].clone();
            match method.as_str() {
                "session.attach" => {
                    attachments += 1;
                    let (sequence, model) = if attachments == 1 { (5, "A") } else { (6, "C") };
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"sequence":2,"cwd":daemon_cwd,"model":{"providerId":"fixture","modelId":model},"defaultBranch":{"branchId":"line-1","sequence":sequence}}})).await;
                }
                "session.branches" => {
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"defaultBranchId":"line-1","branches":[{"branchId":"main","state":"open","head":2},{"branchId":"line-1","parentBranchId":"main","branchPoint":1,"state":"open","head":7}]}})).await;
                }
                "session.history.display" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"items":[],"through":5,"nextBefore":1,"nextAfter":5,"done":true}})).await;
                }
                "session.model.get" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    let through = request["params"]["through"].as_u64().unwrap_or(7);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":"line-1","through":through,"configuration":{"model":{"providerId":"fixture","modelId":"B"}}}})).await;
                }
                "session.methods" => {
                    let methods: Vec<_> = [
                        "session.input.pending",
                        "session.question.pending",
                        "session.exec.pending",
                        "session.turn.get",
                        "session.model.update",
                    ]
                    .into_iter()
                    .map(|name| json!({"name":name,"mutating":name == "session.model.update"}))
                    .collect();
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"methods":methods}}),
                    )
                    .await;
                }
                "provider.models.query" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"models":[
                        {"providerId":"fixture","model":{"id":"A","name":"A"}},
                        {"providerId":"fixture","model":{"id":"B","name":"B"}},
                        {"providerId":"fixture","model":{"id":"C","name":"C"}}],"next":null}}),
                    )
                    .await;
                }
                "session.model.update" => {
                    assert_eq!(request["params"]["branchId"], json!("line-1"));
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"Configuration was superseded by another update","data":{"appError":"configuration_superseded"}}})).await;
                }
                "session.summaries" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":[]})).await;
                    if !delayed {
                        delayed = true;
                        send(&mut write,json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"line-1","event":{"id":"configured-6","sequence":6,"create_at":1,"type":"context","name":"configured","data":{"timeline":"line-1","revision":"revision-1","provider_id":"fixture","model_id":"A"}}}})).await;
                    }
                }
                "session.question.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"batches":[],"next":null}})).await;
                }
                "session.input.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"inputs":[],"next":null}})).await;
                }
                "session.exec.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"tasks":[],"next":null}})).await;
                }
                "session.turn.get" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].as_u64().unwrap_or(5);
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"turn":null,"output":null}})).await;
                }
                "session.detach" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
                    break;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let client = Client::connect(&ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 3,
    })
    .await
    .unwrap();
    let (peer, transport) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(transport);
    let (gateway, finished) = AcpGateway::new(
        client,
        Io {
            reader: Box::new(reader),
            writer: Box::new(writer),
        },
        None,
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let root = rutis::Ctx::root().unwrap();
    let mounted = root.plugin(gateway);
    (&mounted).await.unwrap();
    let (read, mut write) = tokio::io::split(peer);
    let mut read = BufReader::new(read);
    send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let initialize = reply(&mut read, 1).await;
    assert!(initialize["error"].is_null(), "{initialize}");
    send(&mut write,json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":"session","cwd":cwd,"mcpServers":[]}})).await;
    let loaded = reply(&mut read, 2).await;
    assert!(loaded["error"].is_null(), "{loaded}");
    assert_eq!(
        option(&loaded, "model")["currentValue"],
        json!("B"),
        "the load shows the model `session.model.get` adopted for the line"
    );
    send(&mut write,json!({"jsonrpc":"2.0","id":3,"method":"session/set_config_option","params":{"sessionId":"session","configId":"model","value":"A"}})).await;
    let refused = reply(&mut read, 3).await;
    assert_eq!(
        refused["error"]["data"]["nativeAppError"],
        json!("configuration_superseded"),
        "{refused}"
    );
    let refreshed = config_update(&mut read).await;
    assert_eq!(
        option(&refreshed, "model")["currentValue"],
        json!("B"),
        "the refresh reads the line's current configuration: {refreshed}"
    );
    send(
        &mut write,
        json!({"jsonrpc":"2.0","id":4,"method":"session/list","params":{}}),
    )
    .await;
    let delayed = config_update(&mut read).await;
    assert_eq!(
        option(&delayed, "model")["currentValue"],
        json!("B"),
        "a configuration older than the refresh's cutoff must not replace its model: {delayed}"
    );
    drop(read);
    let _ = write.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), finished).await;
    mounted.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    server.await.unwrap();
}

/// The requests a recovered Session makes, answered as a Session on `line`
/// would answer them.
async fn serve_recovery(
    read: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    write: &mut tokio::net::unix::OwnedWriteHalf,
    line: &str,
    cursor: u64,
) {
    while let Some(request) = read_frame(read).await {
        let method = request["method"].as_str().unwrap_or_default().to_owned();
        let branch = request["params"]["branchId"]
            .as_str()
            .unwrap_or(line)
            .to_owned();
        let through = request["params"]["through"].as_u64().unwrap_or(cursor);
        let id = request["id"].clone();
        match method.as_str() {
            "session.attach" => {
                send(
                    write,
                    json!({"jsonrpc":"2.0","id":id,"result":{
                    "sequence":cursor,"model":{"providerId":"fixture","modelId":"A"},
                    "defaultBranch":{"branchId":line,"sequence":cursor}}}),
                )
                .await;
            }
            "session.branches" => {
                let mut branches = vec![json!({"branchId":"main","state":"open","head":cursor})];
                if line != "main" {
                    branches.push(json!({"branchId":line,"parentBranchId":"main","branchPoint":1,"state":"open","head":cursor}));
                }
                send(
                    write,
                    json!({"jsonrpc":"2.0","id":id,"result":{
                    "defaultBranchId":line,"branches":branches}}),
                )
                .await;
            }
            "session.history" => {
                send(
                    write,
                    json!({"jsonrpc":"2.0","id":id,"result":{
                    "events":[],"through":cursor,"nextBefore":null,"nextAfter":cursor}}),
                )
                .await;
            }
            "session.methods" => {
                let methods: Vec<_> = [
                    "session.input.pending",
                    "session.question.pending",
                    "session.exec.pending",
                    "session.turn.get",
                ]
                .into_iter()
                .map(|name| json!({"name":name,"mutating":false}))
                .collect();
                send(
                    write,
                    json!({"jsonrpc":"2.0","id":id,"result":{"methods":methods}}),
                )
                .await;
            }
            "session.exec.pending" | "session.input.pending" | "session.question.pending" => {
                let key = if method == "session.exec.pending" {
                    "tasks"
                } else if method == "session.input.pending" {
                    "inputs"
                } else {
                    "batches"
                };
                send(
                    write,
                    json!({"jsonrpc":"2.0","id":id,"result":{
                    "branchId":branch,"through":through,key:[],"next":null}}),
                )
                .await;
            }
            "session.turn.get" => {
                if request["params"]
                    .as_object()
                    .is_some_and(|params| params.contains_key("inputId"))
                {
                    // The moved default holds no such input: a real server
                    // answers nothing for it, and this fixture says so.
                    send(
                        write,
                        json!({"jsonrpc":"2.0","id":id,"error":{
                        "code":-32000,"message":"fixture: the input is not on this line",
                        "data":{"appError":"input_not_on_line"}}}),
                    )
                    .await;
                    continue;
                }
                send(
                    write,
                    json!({"jsonrpc":"2.0","id":id,"result":{
                    "branchId":branch,"through":through,"turn":null,"output":null}}),
                )
                .await;
            }
            "session.detach" => {
                send(write, json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
                break;
            }
            other => panic!("unexpected {other}: {request}"),
        }
    }
}

/// A prompt in flight when the default branch moved is settled on the line that
/// accepted it: the reattach follows the new default, so this connection can no
/// longer carry the old line's turn, and the prompt is answered instead of
/// reading the new line for the old input (design §14).
#[tokio::test]
async fn a_recovery_settles_the_prompt_the_moved_default_left_behind() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let cwd = home.path().to_str().unwrap().to_owned();
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let daemon_cwd = cwd.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.attach","session.attach.expectedCwd","session.create","session.branches","session.detach","session.exec.pending","session.history","session.history.latestTurn","session.input.pending","session.input_lookup","session.methods","session.model.update","session.question.pending","session.stop","session.submit","session.summaries","session.turn.get","session.turn.get.inputId"]}})).await;
        while let Some(request) = read_frame(&mut read).await {
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let id = request["id"].clone();
            if method == "session.submit" {
                assert_eq!(request["params"]["branchId"], json!("main"));
                let submission = request["params"]["submissionId"].clone();
                send(
                    &mut write,
                    json!({"jsonrpc":"2.0","id":id,"result":{"event":{
                    "id":"in-1","sequence":3,"create_at":1,"type":"input","name":"message",
                    "data":{"user":{"content":[{"type":"text","text":"hello"}]},"mode":"queue"}}}}),
                )
                .await;
                send(&mut write,json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"id":"in-1","sequence":3,"create_at":1,"type":"input","name":"message","data":{"submission_id":submission,"user":{"content":[{"type":"text","text":"hello"}]},"mode":"queue"}}}})).await;
                break;
            }
            match method.as_str() {
                "session.attach" => {
                    assert_eq!(request["params"]["expectedCwd"], json!(daemon_cwd));
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{
                        "sequence":2,"cwd":daemon_cwd,"model":{"providerId":"fixture","modelId":"A"},
                        "defaultBranch":{"branchId":"main","sequence":2}}})).await;
                }
                "session.branches" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "defaultBranchId":"main",
                        "branches":[{"branchId":"main","state":"open","head":2}]}}),
                    )
                    .await;
                }
                "session.history" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "events":[],"through":2,"nextBefore":null,"nextAfter":2}}),
                    )
                    .await;
                }
                "session.methods" => {
                    let methods: Vec<_> = [
                        "session.input.pending",
                        "session.question.pending",
                        "session.exec.pending",
                        "session.turn.get",
                    ]
                    .into_iter()
                    .map(|name| json!({"name":name,"mutating":false}))
                    .collect();
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"methods":methods}}),
                    )
                    .await;
                }
                "session.input_lookup" => {
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{"status":"unknown"}}),
                    )
                    .await;
                }
                "session.exec.pending" | "session.input.pending" | "session.question.pending" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].clone();
                    let key = if method == "session.exec.pending" {
                        "tasks"
                    } else if method == "session.input.pending" {
                        "inputs"
                    } else {
                        "batches"
                    };
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "branchId":branch,"through":through,key:[],"next":null}}),
                    )
                    .await;
                }
                "session.turn.get" => {
                    let branch = request["params"]["branchId"].clone();
                    let through = request["params"]["through"].clone();
                    send(
                        &mut write,
                        json!({"jsonrpc":"2.0","id":id,"result":{
                        "branchId":branch,"through":through,"turn":null,"output":null}}),
                    )
                    .await;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
        drop(write);
        drop(read);
        // The client reconnects and recovers; the default moved to `line-1`
        // while it was away.
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.attach","session.attach.expectedCwd","session.create","session.branches","session.detach","session.exec.pending","session.history","session.history.latestTurn","session.input.pending","session.input_lookup","session.methods","session.model.update","session.question.pending","session.stop","session.submit","session.summaries","session.turn.get","session.turn.get.inputId"]}})).await;
        serve_recovery(&mut read, &mut write, "line-1", 5).await;
    });
    let client = Client::connect(&ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 3,
    })
    .await
    .unwrap();
    let (peer, transport) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(transport);
    let (gateway, finished) = AcpGateway::new(
        client,
        Io {
            reader: Box::new(reader),
            writer: Box::new(writer),
        },
        None,
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let root = rutis::Ctx::root().unwrap();
    let mounted = root.plugin(gateway);
    (&mounted).await.unwrap();
    let (read, mut write) = tokio::io::split(peer);
    let mut read = BufReader::new(read);
    send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let initialize = reply(&mut read, 1).await;
    assert!(initialize["error"].is_null(), "{initialize}");
    send(&mut write,json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":"session","cwd":cwd,"mcpServers":[]}})).await;
    let loaded = reply(&mut read, 2).await;
    assert!(loaded["error"].is_null(), "{loaded}");
    send(&mut write,json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"session","prompt":[{"type":"text","text":"hello"}]}})).await;
    let settled = reply(&mut read, 3).await;
    assert_eq!(
        settled["error"]["data"]["code"],
        json!("submission_unknown"),
        "the moved default settles the prompt instead of reading another line: {settled}"
    );
    drop(read);
    let _ = write.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), finished).await;
    mounted.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    server.await.unwrap();
}

/// A load whose default branch is a line hands the request's `expectedConfig`
/// to the checked attach, reads the directory only after the attach passed,
/// and shows that line's own model instead of the attach reply's root model.
/// Writing the model afterwards targets the line, and a temporary route keeps
/// the catalog provider the model was chosen under (design §14).
#[tokio::test]
async fn a_gateway_load_keeps_the_default_line_apart_from_the_root() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = home.path().join("daemon.sock");
    let cwd = home.path().to_str().unwrap().to_owned();
    let listener = UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let daemon_cwd = cwd.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let hello = frame(&mut read).await;
        send(&mut write,json!({"jsonrpc":"2.0","id":hello["id"],"result":{"instanceId":"fixture","serverVersion":"fixture","protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.attach","session.attach.expectedCwd","session.attach.expectedConfig","session.branches","session.create","session.detach","session.history","session.history.latestTurn","session.input_lookup","session.methods","session.model.get","session.model.update","session.stop","session.submit","session.turn.get","provider.models.query"]}})).await;
        let mut attached = false;
        while let Some(request) = read_frame(&mut read).await {
            let method = request["method"].as_str().unwrap_or_default().to_owned();
            let branch = request["params"]["branchId"]
                .as_str()
                .unwrap_or("line-1")
                .to_owned();
            let through = request["params"]["through"].as_u64().unwrap_or(5);
            let id = request["id"].clone();
            match method.as_str() {
                "session.attach" => {
                    assert_eq!(request["params"]["expectedCwd"], json!(daemon_cwd));
                    assert_eq!(
                        request["params"]["expectedConfig"],
                        json!({"modelId":"B"}),
                        "the load hands the request's expectation to the attach"
                    );
                    attached = true;
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"sequence":2,"cwd":daemon_cwd,"model":{"providerId":"root-route","modelId":"A"},"catalogProviderId":"root-catalog","defaultBranch":{"branchId":"line-1","sequence":5}}})).await;
                }
                "session.branches" => {
                    assert!(
                        attached,
                        "the checked attach passes before the directory is read"
                    );
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"defaultBranchId":"line-1","branches":[{"branchId":"main","state":"open","head":2},{"branchId":"line-1","parentBranchId":"main","branchPoint":1,"state":"open","head":5}]}})).await;
                }
                "session.history" => {
                    assert_eq!(branch, "line-1", "the recovery reads the default line");
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"events":[],"through":5,"nextBefore":null,"nextAfter":5}})).await;
                }
                "session.methods" => {
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"methods":[{"name":"session.model.update","mutating":true}]}})).await;
                }
                "provider.models.query" => {
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"models":[{"providerId":"fixture","model":{"id":"A","name":"A"}},{"providerId":"fixture","model":{"id":"B","name":"B"}},{"providerId":"fixture","model":{"id":"C","name":"C"}}],"next":null}})).await;
                }
                "session.model.get" => {
                    assert_eq!(branch, "line-1", "the line's configuration is its own");
                    assert_eq!(through, 5, "read at the line's own cursor");
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":"line-1","through":5,"configuration":{"model":{"providerId":"fixture","modelId":"B"}}}})).await;
                }
                "session.model.update" => {
                    assert_eq!(branch, "line-1", "the config write stays on the line");
                    assert_eq!(request["params"]["providerId"], json!("fixture"));
                    assert_eq!(request["params"]["modelId"], json!("C"));
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"model":{"providerId":"route-1","modelId":"C"},"outcome":"adopted"}})).await;
                }
                "session.turn.get" => {
                    assert_eq!(branch, "line-1", "the turn read is the default line's");
                    send(&mut write,json!({"jsonrpc":"2.0","id":id,"result":{"branchId":branch,"through":through,"turn":null,"output":null}})).await;
                }
                "session.detach" => {
                    send(&mut write, json!({"jsonrpc":"2.0","id":id,"result":{}})).await;
                    break;
                }
                other => panic!("unexpected {other}: {request}"),
            }
        }
    });
    let client = Client::connect(&ConnectionConfig {
        endpoint,
        server: Server {
            program: "unused".into(),
            embedded: false,
        },
        profile: Some(DaemonProfile::Product),
        client_kind: "other",
        client_name: "AcpGateway",
        protocol_version: 3,
    })
    .await
    .unwrap();
    let (peer, transport) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(transport);
    let (gateway, finished) = AcpGateway::new(
        client,
        Io {
            reader: Box::new(reader),
            writer: Box::new(writer),
        },
        None,
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let root = rutis::Ctx::root().unwrap();
    let mounted = root.plugin(gateway);
    (&mounted).await.unwrap();
    let (read, mut write) = tokio::io::split(peer);
    let mut read = BufReader::new(read);
    send(&mut write,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})).await;
    let initialize = reply(&mut read, 1).await;
    assert!(initialize["error"].is_null(), "{initialize}");
    send(&mut write,json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":"session","cwd":cwd,"mcpServers":[],"_meta":{"lody":{"sessionConfig":{"version":1,"modelId":"B","configOptionValues":{}}}}}})).await;
    let loaded = reply(&mut read, 2).await;
    assert!(loaded["error"].is_null(), "{loaded}");
    assert_eq!(
        option(&loaded, "model")["currentValue"],
        json!("B"),
        "the load shows the default line's model, not the attach reply's root model: {loaded}"
    );
    send(&mut write,json!({"jsonrpc":"2.0","id":3,"method":"session/set_config_option","params":{"sessionId":"session","configId":"model","value":"C"}})).await;
    let configured = reply(&mut read, 3).await;
    assert!(configured["error"].is_null(), "{configured}");
    assert_eq!(
        option(&configured, "model")["currentValue"],
        json!("C"),
        "the adopted model keeps the catalog provider its line chose: {configured}"
    );
    drop(read);
    let _ = write.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), finished).await;
    mounted.dispose().await.unwrap();
    root.shutdown().await.unwrap();
    server.await.unwrap();
}
